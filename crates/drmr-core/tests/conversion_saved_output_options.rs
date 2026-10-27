//! Saved path and addressing choices govern actual outputs after reopening.
mod common;
#[path = "support/source_layout_decode.rs"]
mod decode;
#[path = "support/saved_initialization_layout.rs"]
mod initialization;

use drmr_core::{
    conversion::{
        Converter, DecodeEvidence, ProcessingMode, ValidationDepth, VerificationSource,
        conformance::ManifestValidationTools,
        dash_addressing::DashAddressing,
        key_file::{BundleOperatorKeys, OperatorKeys},
        layout::LayoutMode,
        plan::{
            self, ExecutionPlanRequest, SavedPlan,
            bundle::{BundleOutputIntent, BundlePlanRequest, ManifestValidation},
        },
    },
    events::Collector,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{fs, path::Path, sync::Arc};

fn converter(mode: ProcessingMode) -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(runner, "packager", "must-not-run-decoder")
            .with_ffprobe("must-not-run-probe")
            .with_processing(mode),
        events,
    )
}

fn source(root: &Path) -> std::path::PathBuf {
    let media = root.join("video/480p");
    fs::create_dir_all(&media).unwrap();
    let playlist = root.join("source.m3u8");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let result = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture)
        .args([
            "-map",
            "0",
            "-c",
            "copy",
            "-hls_time",
            "3",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(media.join("source-%d.ts"))
        .arg(&playlist)
        .output()
        .unwrap();
    assert!(result.status.success(), "BBB fixture generation failed");
    // FFmpeg writes basenames when the segment directory differs. Reconstruct
    // the authored relative URI while retaining its original timing and packets.
    let text = fs::read_to_string(&playlist).unwrap();
    let text = text
        .lines()
        .map(|line| {
            if line.is_empty() || line.starts_with('#') {
                line.to_owned()
            } else {
                format!("video/480p/{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    fs::write(&playlist, format!("{text}\n")).unwrap();
    playlist
}

fn assert_no_decoder(events: &Collector) {
    for record in events.records() {
        if record.get("event").and_then(serde_json::Value::as_str) == Some("command") {
            assert_eq!(
                record.get("tool").and_then(serde_json::Value::as_str),
                Some("packager")
            );
        }
    }
}

#[test]
fn saved_single_preserves_nested_source_directories_without_running_media_tools() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("input"));
    let original = common::snapshot_tree(input.parent().unwrap());
    let output = root.path().join("result");
    let keys = OperatorKeys::default();
    let (planner, events) = converter(ProcessingMode::Segments);
    let saved = planner
        .with_new_output_defaults()
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
    assert_eq!(saved.schema_version, 9);
    let placements = &saved
        .output_options
        .as_ref()
        .unwrap()
        .layout
        .as_ref()
        .unwrap()
        .tracks;
    assert_eq!(placements.len(), 2);
    assert_eq!(placements[0].output_ordinal, placements[1].output_ordinal);
    assert_eq!(placements[0].media_directory, Path::new("video/480p"));
    let file = root.path().join("single.plan.json");
    plan::write_new(&file, &saved).unwrap();
    let SavedPlan::Single(loaded) = plan::read_saved(&file).unwrap() else {
        panic!("one output must reopen as a single plan");
    };
    // Older Rust defaults are intentionally different; the saved choice wins.
    let (executor, execution_events) = converter(ProcessingMode::Auto);
    let converted = executor.execute_plan(&loaded, &keys).unwrap();
    assert!(converted.verification.passed);
    assert_eq!(
        converted.verification.required_depth,
        ValidationDepth::Native
    );
    assert_eq!(
        converted.verification.decode_evidence,
        DecodeEvidence::not_requested()
    );
    let verified = executor.verify_plan(&loaded, &keys).unwrap();
    assert!(
        verified.source_unchanged && verified.output_matches_plan && verified.verification.passed
    );
    assert!(output.join("video/480p").is_dir());
    assert!(!output.join("track-0000").exists());
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
    assert!(
        events
            .records()
            .iter()
            .all(|record| record.get("stage").and_then(serde_json::Value::as_str) != Some("tool"))
    );
    assert!(
        execution_events
            .records()
            .iter()
            .all(|record| record.get("stage").and_then(serde_json::Value::as_str) != Some("tool"))
    );
}

fn intent(id: &str, addressing: DashAddressing) -> BundleOutputIntent {
    BundleOutputIntent {
        id: id.into(),
        format: OutputFormat::Dash,
        target: TargetProtection::Clear,
        drms: Vec::new(),
        conformance: ManifestValidation::None,
        processing: Some(ProcessingMode::Stitch),
        dash_addressing: Some(addressing),
    }
}

fn assert_addressing(root: &Path, id: &str) {
    let text = fs::read_to_string(root.join(id).join("manifest.mpd")).unwrap();
    let xml = roxmltree::Document::parse(&text).unwrap();
    let expected = match id {
        "template" | "timeline" => "SegmentTemplate",
        "list" => "SegmentList",
        "base" => "SegmentBase",
        _ => panic!("unexpected saved output"),
    };
    let representations = xml
        .descendants()
        .filter(|node| node.has_tag_name("Representation"))
        .collect::<Vec<_>>();
    assert_eq!(representations.len(), 2);
    for representation in representations {
        let addressing = representation
            .children()
            .find(|node| node.has_tag_name(expected))
            .unwrap();
        if id == "template" {
            assert!(addressing.attribute("duration").is_some());
            assert!(addressing.attribute("media").unwrap().contains("$Number$"));
            assert!(
                addressing
                    .children()
                    .all(|node| !node.has_tag_name("SegmentTimeline"))
            );
        } else if id == "timeline" {
            assert!(
                addressing
                    .children()
                    .any(|node| node.has_tag_name("SegmentTimeline"))
            );
        }
    }
}

#[test]
fn saved_bundle_keeps_each_dash_form_and_source_directory_through_conversion_and_verification() {
    let root = common::private_tempdir().unwrap();
    let input = source(&root.path().join("input"));
    let original = common::snapshot_tree(input.parent().unwrap());
    let output = root.path().join("result");
    let keys = BundleOperatorKeys::default();
    let outputs = [
        intent("template", DashAddressing::TemplateDuration),
        intent("timeline", DashAddressing::Template),
        intent("list", DashAddressing::List),
        intent("base", DashAddressing::Base),
    ];
    let (planner, _) = converter(ProcessingMode::Auto);
    let saved = planner
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
    assert_eq!(saved.schema_version, 10);
    let file = root.path().join("bundle.plan.json");
    plan::write_bundle(&file, &saved).unwrap();
    let SavedPlan::Bundle(loaded) = plan::read_saved(&file).unwrap() else {
        panic!("several outputs must reopen as a bundle");
    };
    let (executor, events) = converter(ProcessingMode::Auto);
    let executor = executor
        .with_layout(LayoutMode::Normalized)
        .with_dash_addressing(DashAddressing::Base);
    let tools = ManifestValidationTools::default();
    let converted = executor
        .execute_bundle_plan(&loaded, &keys, &tools)
        .unwrap();
    assert_eq!(converted.outputs.len(), 4);
    let verified = executor.verify_bundle_plan(&loaded, &keys, &tools).unwrap();
    assert!(verified.passed && verified.source_unchanged);
    for child in &verified.outputs {
        assert!(child.output_matches_plan && child.verification.passed);
        assert_eq!(child.verification.required_depth, ValidationDepth::Native);
        assert_eq!(
            child.verification.decode_evidence,
            DecodeEvidence::not_requested()
        );
        assert!(output.join(&child.id).join("video/480p").is_dir());
        assert!(!output.join(&child.id).join("track-0000").exists());
        assert_addressing(&output, &child.id);
    }
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), original);
    assert_no_decoder(&events);
}
