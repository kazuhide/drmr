//! Initialization placement is part of the saved output layout, separately from media equality.
use super::*;
use drmr_core::conversion::{ConversionError, VerificationRequest};
use std::path::PathBuf;

fn initialization(root: &Path, format: OutputFormat) -> (PathBuf, String) {
    if format == OutputFormat::Dash {
        let manifest = root.join("manifest.mpd");
        let text = fs::read_to_string(&manifest).unwrap();
        let xml = roxmltree::Document::parse(&text).unwrap();
        let node = xml
            .descendants()
            .find(|node| node.has_tag_name("SegmentTemplate") || node.has_tag_name("SegmentList"))
            .unwrap();
        let uri = node
            .attribute("initialization")
            .or_else(|| {
                node.children()
                    .find(|child| child.has_tag_name("Initialization"))
                    .and_then(|child| child.attribute("sourceURL"))
            })
            .unwrap();
        (manifest, uri.into())
    } else {
        let master = fs::read_to_string(root.join("master.m3u8")).unwrap();
        let child = master
            .lines()
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .unwrap();
        let playlist = root.join(child);
        let text = fs::read_to_string(&playlist).unwrap();
        let map = text
            .lines()
            .find_map(|line| line.strip_prefix("#EXT-X-MAP:"))
            .unwrap();
        let uri = map
            .split("URI=\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        (playlist, uri.into())
    }
}

fn relocate(
    root: &Path,
    format: OutputFormat,
    other_directory: bool,
) -> (PathBuf, String, PathBuf, PathBuf) {
    let (manifest, old_uri) = initialization(root, format);
    let old = manifest.parent().unwrap().join(&old_uri);
    let new = if other_directory {
        root.join("relocated/initialization.mp4")
    } else {
        old.parent().unwrap().join("unbound-initialization.mp4")
    };
    fs::create_dir_all(new.parent().unwrap()).unwrap();
    let bytes = fs::read(&old).unwrap();
    fs::rename(&old, &new).unwrap();
    assert_eq!(fs::read(&new).unwrap(), bytes);
    let relative = new
        .strip_prefix(manifest.parent().unwrap())
        .unwrap()
        .to_str()
        .unwrap()
        .replace('\\', "/");
    let original = fs::read_to_string(&manifest).unwrap();
    fs::write(&manifest, original.replace(&old_uri, &relative)).unwrap();
    (manifest, original, old, new)
}

fn native_matches(command: &Converter, input: &Path, output: &Path) {
    let keys = OperatorKeys::default();
    assert!(
        command
            .verify(VerificationRequest {
                source: VerificationSource {
                    input,
                    input_keys: &keys.input,
                    hls_keys: &keys.hls_input
                },
                output: VerificationSource {
                    input: output,
                    input_keys: &keys.input,
                    hls_keys: &keys.hls_input
                },
            })
            .unwrap()
            .passed
    );
}

#[test]
fn single_saved_verification_refuses_byte_identical_initialization_relocation() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("input"));
    let original_source = common::snapshot_tree(input.parent().unwrap());
    let cases = [
        (OutputFormat::Dash, DashAddressing::TemplateDuration),
        (OutputFormat::Dash, DashAddressing::Template),
        (OutputFormat::Dash, DashAddressing::List),
        (OutputFormat::Hls, DashAddressing::TemplateDuration),
    ];
    for (ordinal, (format, addressing)) in cases.into_iter().enumerate() {
        let output = root.path().join(format!("output-{ordinal}"));
        let keys = OperatorKeys::default();
        let (planner, _) = converter(ProcessingMode::Stitch);
        let plan = planner
            .with_new_output_defaults()
            .with_dash_addressing(addressing)
            .create_execution_plan(ExecutionPlanRequest {
                input: VerificationSource {
                    input: &input,
                    input_keys: &keys.input,
                    hls_keys: &keys.hls_input,
                },
                output: &output,
                format: Some(format),
                target: TargetProtection::Clear,
            })
            .unwrap();
        let (executor, events) = converter(ProcessingMode::Auto);
        assert!(
            executor
                .execute_plan(&plan, &keys)
                .unwrap()
                .verification
                .passed
        );
        assert!(
            executor
                .verify_plan(&plan, &keys)
                .unwrap()
                .verification
                .passed
        );
        for other_directory in [true, false] {
            let (manifest, original, old, new) = relocate(&output, format, other_directory);
            let entrypoint = output.join(if format == OutputFormat::Dash {
                "manifest.mpd"
            } else {
                "master.m3u8"
            });
            native_matches(&executor, &input, &entrypoint);
            assert_eq!(
                executor.verify_plan(&plan, &keys).err(),
                Some(ConversionError("source-layout-path-unsafe")),
                "case {ordinal}, directory {other_directory}"
            );
            fs::rename(new, old).unwrap();
            fs::write(manifest, original).unwrap();
            assert!(
                executor
                    .verify_plan(&plan, &keys)
                    .unwrap()
                    .verification
                    .passed
            );
        }
        assert_no_decoder(&events);
    }
    assert_eq!(
        common::snapshot_tree(input.parent().unwrap()),
        original_source
    );
}

#[test]
fn bundle_saved_verification_refuses_changed_dash_and_hls_initialization_placement() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("input"));
    let keys = BundleOperatorKeys::default();
    let mut hls = intent("hls", DashAddressing::List);
    hls.format = OutputFormat::Hls;
    hls.dash_addressing = None;
    let outputs = [intent("dash", DashAddressing::List), hls];
    let output = root.path().join("result");
    let (planner, _) = converter(ProcessingMode::Auto);
    let plan = planner
        .with_new_output_defaults()
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output_root: &output,
            outputs: &outputs,
        })
        .unwrap();
    let (executor, events) = converter(ProcessingMode::Auto);
    let tools = ManifestValidationTools::default();
    executor.execute_bundle_plan(&plan, &keys, &tools).unwrap();
    assert!(
        executor
            .verify_bundle_plan(&plan, &keys, &tools)
            .unwrap()
            .passed
    );
    for (name, format) in [("dash", OutputFormat::Dash), ("hls", OutputFormat::Hls)] {
        let child = output.join(name);
        let (manifest, original, old, new) = relocate(&child, format, true);
        native_matches(
            &executor,
            &input,
            &child.join(if format == OutputFormat::Dash {
                "manifest.mpd"
            } else {
                "master.m3u8"
            }),
        );
        assert_eq!(
            executor.verify_bundle_plan(&plan, &keys, &tools).err(),
            Some(ConversionError("source-layout-path-unsafe"))
        );
        fs::rename(new, old).unwrap();
        fs::write(manifest, original).unwrap();
        assert!(
            executor
                .verify_bundle_plan(&plan, &keys, &tools)
                .unwrap()
                .passed
        );
    }
    assert_no_decoder(&events);
}
