//! Complete decoding does not change the admitted multiplexed segment layout.
use super::*;
use drmr_core::conversion::{
    ConversionRequest, VerificationReport, VerificationRequest, VerificationScope,
};
use sha2::{Digest as _, Sha256};
use std::{fmt::Write as _, path::PathBuf};

fn command(mode: ProcessingMode, layout: LayoutMode, decode: bool) -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(runner, "packager", "ffmpeg")
            .with_processing(mode)
            .with_layout(layout)
            .with_decode(decode),
        events,
    )
}

fn assert_decoded(report: &VerificationReport) {
    assert!(report.passed && report.checks.is_pass());
    assert_eq!(report.required_depth, ValidationDepth::Decoded);
    assert_eq!(report.scope, VerificationScope::Comparison);
    assert_eq!(
        report.decode_evidence,
        DecodeEvidence::ContentCompared { equal: true }
    );
}

fn assert_decoder_ran(events: &Collector) {
    assert!(events.records().iter().any(|record| {
        record.get("event").and_then(serde_json::Value::as_str) == Some("command")
            && record.get("tool").and_then(serde_json::Value::as_str) == Some("ffmpeg")
    }));
}

fn playlist(root: &Path) -> PathBuf {
    let text = fs::read_to_string(root.join("master.m3u8")).unwrap();
    let rows: Vec<_> = text
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "the output retains one multiplexed physical rendition"
    );
    root.join(rows[0])
}

fn assert_original_resources(input: &Path, root: &Path) {
    let before = fs::read_to_string(input).unwrap();
    let child = playlist(root);
    let after = fs::read_to_string(&child).unwrap();
    let resources = |text: &str| {
        text.lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let original = resources(&before);
    let result = resources(&after);
    assert_eq!(original.len(), result.len());
    for (a, b) in original.iter().zip(result) {
        assert_eq!(
            fs::read(input.parent().unwrap().join(a)).unwrap(),
            fs::read(child.parent().unwrap().join(b)).unwrap()
        );
    }
    assert_eq!(
        before
            .lines()
            .filter(|line| line.starts_with("#EXTINF:"))
            .collect::<Vec<_>>(),
        after
            .lines()
            .filter(|line| line.starts_with("#EXTINF:"))
            .collect::<Vec<_>>()
    );
}

fn request<'a>(input: &'a Path, output: &'a Path, keys: &'a OperatorKeys) -> ConversionRequest<'a> {
    ConversionRequest {
        input,
        output,
        format: OutputFormat::HlsTs,
        target: TargetProtection::Clear,
        input_keys: &keys.input,
        hls_input_keys: &keys.hls_input,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    }
}

#[test]
fn direct_muxed_segments_keep_layout_and_complete_decode_for_source_and_legacy_placement() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("input"));
    let before = common::snapshot_tree(input.parent().unwrap());
    let keys = OperatorKeys::default();
    for (ordinal, layout) in [LayoutMode::Source, LayoutMode::Normalized]
        .into_iter()
        .enumerate()
    {
        let (command, events) = command(ProcessingMode::Segments, layout, true);
        let output = root.path().join(format!("direct-{ordinal}"));
        assert_decoded(
            &command
                .convert(request(&input, &output, &keys))
                .unwrap()
                .verification,
        );
        assert_original_resources(&input, &output);
        assert_decoded(
            &command
                .verify(VerificationRequest {
                    source: VerificationSource {
                        input: &input,
                        input_keys: &keys.input,
                        hls_keys: &keys.hls_input,
                    },
                    output: VerificationSource {
                        input: &output.join("master.m3u8"),
                        input_keys: &keys.input,
                        hls_keys: &keys.hls_input,
                    },
                })
                .unwrap(),
        );
        assert_decoder_ran(&events);
    }
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
}

#[test]
fn unavailable_additional_decoder_cannot_publish_a_native_valid_source_layout_output() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("input"));
    let output = root.path().join("unpublished");
    let events = Arc::new(Collector::default());
    let command = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
        "packager",
        "a-decoder-that-does-not-exist",
    )
    .with_processing(ProcessingMode::Segments)
    .with_layout(LayoutMode::Source)
    .with_decode(true);
    let error = command
        .convert(request(&input, &output, &OperatorKeys::default()))
        .unwrap_err();
    assert_ne!(error.0, "execution-plan-output-options-changed");
    assert_decoder_ran(&events);
    assert!(!output.exists());
}

fn identity<T: serde::Serialize + serde::de::DeserializeOwned>(saved: &T) -> T {
    let mut value = serde_json::to_value(saved).unwrap();
    value.as_object_mut().unwrap().shift_remove("plan_id");
    let id = hex::encode(Sha256::digest(serde_json::to_vec(&value).unwrap()));
    value["plan_id"] = id.into();
    serde_json::from_value(value).unwrap()
}

#[test]
fn saved_single_muxed_segments_keep_source_selection_and_the_decode_floor() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("input"));
    let before = common::snapshot_tree(input.parent().unwrap());
    let keys = OperatorKeys::default();
    let (planner, _) = command(ProcessingMode::Segments, LayoutMode::Source, true);
    let saved = planner
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output: &root.path().join("current"),
            format: Some(OutputFormat::HlsTs),
            target: TargetProtection::Clear,
        })
        .unwrap();
    let tracks = &saved
        .output_options
        .as_ref()
        .unwrap()
        .layout
        .as_ref()
        .unwrap()
        .tracks;
    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0].output_ordinal, tracks[1].output_ordinal);
    let mut legacy = saved.clone();
    legacy.schema_version = 5;
    legacy.output_options = None;
    legacy.intent.output = saved.intent.output.with_file_name("legacy");
    let legacy = identity(&legacy);
    let (executor, events) = command(ProcessingMode::Auto, LayoutMode::Normalized, false);
    for (ordinal, plan) in [saved, legacy].into_iter().enumerate() {
        let file = root.path().join(format!("single-{ordinal}.json"));
        plan::write_new(&file, &plan).unwrap();
        let loaded = plan::read(&file).unwrap();
        assert_decoded(&executor.execute_plan(&loaded, &keys).unwrap().verification);
        assert_original_resources(&input, &loaded.intent.output);
        assert_decoded(&executor.verify_plan(&loaded, &keys).unwrap().verification);
    }
    assert_decoder_ran(&events);
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
}

#[test]
fn saved_bundle_muxed_segments_keep_source_selection_and_the_decode_floor() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("input"));
    let before = common::snapshot_tree(input.parent().unwrap());
    let keys = BundleOperatorKeys::default();
    let (planner, _) = command(ProcessingMode::Segments, LayoutMode::Source, true);
    let outputs = [BundleOutputIntent {
        id: "ts".into(),
        format: OutputFormat::HlsTs,
        target: TargetProtection::Clear,
        drms: Vec::new(),
        conformance: ManifestValidation::None,
        processing: None,
        dash_addressing: None,
    }];
    let saved = planner
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output_root: &root.path().join("current"),
            outputs: &outputs,
        })
        .unwrap();
    let mut legacy = saved.clone();
    legacy.schema_version = 6;
    legacy.output_options = None;
    legacy.output_root = saved.output_root.with_file_name("legacy");
    let legacy = identity(&legacy);
    let tools = ManifestValidationTools::default();
    let (executor, events) = command(ProcessingMode::Auto, LayoutMode::Normalized, false);
    for (ordinal, plan) in [saved, legacy].into_iter().enumerate() {
        let file = root.path().join(format!("bundle-{ordinal}.json"));
        plan::write_bundle(&file, &plan).unwrap();
        let SavedPlan::Bundle(loaded) = plan::read_saved(&file).unwrap() else {
            panic!("bundle required")
        };
        let converted = executor
            .execute_bundle_plan(&loaded, &keys, &tools)
            .unwrap();
        assert_decoded(&converted.outputs[0].conversion.verification);
        assert_original_resources(&input, &loaded.output_root.join("ts"));
        let verified = executor.verify_bundle_plan(&loaded, &keys, &tools).unwrap();
        assert!(verified.passed && verified.source_unchanged);
        assert_decoded(&verified.outputs[0].verification);
    }
    assert_decoder_ran(&events);
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
}

fn merge_first_two_resources(root: &Path) {
    let path = playlist(root);
    let text = fs::read_to_string(&path).unwrap();
    let mut pending = None;
    let mut resources = Vec::<(f64, String)>::new();
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("#EXTINF:") {
            pending = Some(value.split(',').next().unwrap().parse().unwrap());
        } else if !line.is_empty() && !line.starts_with('#') {
            resources.push((pending.take().unwrap(), line.into()));
        }
    }
    assert!(resources.len() >= 3);
    let parent = path.parent().unwrap();
    let mut bytes = fs::read(parent.join(&resources[0].1)).unwrap();
    bytes.extend_from_slice(&fs::read(parent.join(&resources[1].1)).unwrap());
    fs::write(parent.join(&resources[0].1), bytes).unwrap();
    let merged = resources[0].0 + resources[1].0;
    let target = resources
        .iter()
        .skip(2)
        .fold(merged, |maximum, (duration, _)| maximum.max(*duration))
        .ceil();
    let mut manifest = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target:.0}\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:{merged:.9},\n{}\n",
        resources[0].1
    );
    for (duration, uri) in resources.iter().skip(2) {
        writeln!(manifest, "#EXTINF:{duration:.9},\n{uri}").unwrap();
    }
    manifest.push_str("#EXT-X-ENDLIST\n");
    fs::write(path, manifest).unwrap();
}

#[test]
fn decoded_equality_does_not_hide_changed_muxed_resource_membership() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("input"));
    let before = common::snapshot_tree(input.parent().unwrap());
    let keys = OperatorKeys::default();
    let output = root.path().join("result");
    let (planner, _) = command(ProcessingMode::Segments, LayoutMode::Source, true);
    let saved = planner
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output: &output,
            format: Some(OutputFormat::HlsTs),
            target: TargetProtection::Clear,
        })
        .unwrap();
    let (executor, _) = command(ProcessingMode::Auto, LayoutMode::Normalized, false);
    assert_decoded(&executor.execute_plan(&saved, &keys).unwrap().verification);
    merge_first_two_resources(&output);
    let (whole, _) = command(ProcessingMode::Stitch, LayoutMode::Normalized, true);
    assert_decoded(
        &whole
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &keys.input,
                    hls_keys: &keys.hls_input,
                },
                output: VerificationSource {
                    input: &output.join("master.m3u8"),
                    input_keys: &keys.input,
                    hls_keys: &keys.hls_input,
                },
            })
            .unwrap(),
    );
    let result = executor.verify_plan(&saved, &keys).unwrap();
    assert!(result.source_unchanged && result.output_matches_plan);
    assert!(!result.verification.passed);
    assert!(!result.verification.checks.packet_payloads_equal);
    assert!(!result.verification.checks.packet_timing_equal);
    assert_eq!(result.verification.required_depth, ValidationDepth::Decoded);
    assert_eq!(
        result.verification.decode_evidence,
        DecodeEvidence::ContentCompared { equal: true }
    );
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
}
