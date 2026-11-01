//! Native codec facts are recomputed for plans and never accepted from saved JSON.
use super::*;
use crate::{
    conversion::{HlsInputKeys, InputKeys, OutputKey},
    events::Collector,
    toolrun::{Secrets, ToolRunner},
};

pub(super) fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/vp9-p0-8.mp4")
}

pub(super) fn command() -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(runner, "absent-packager", "absent-ffmpeg").with_ffprobe("absent-ffprobe"),
        events,
    )
}

pub(super) fn output_keys() -> OperatorKeys {
    let mut keys = OperatorKeys::default();
    keys.output = Some(
        OutputKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap(),
    );
    keys
}

pub(super) fn no_tools(events: &Collector) {
    assert!(
        events
            .records()
            .iter()
            .all(|event| event.get("event").and_then(serde_json::Value::as_str) != Some("command"))
    );
}

#[test]
fn vp9_cbcs_single_plans_and_valid_saved_replay_refuse_before_publication() {
    let root = crate::native::private_tempdir().unwrap();
    let source = source();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let input = VerificationSource {
        input: &source,
        input_keys: &empty,
        hls_keys: &hls,
    };
    let (command, events) = command();
    for format in [OutputFormat::Mp4, OutputFormat::Hls, OutputFormat::Dash] {
        let output = root.path().join(format!("refused-{}", format.as_str()));
        let error = command
            .create_execution_plan(ExecutionPlanRequest {
                input,
                output: &output,
                format: Some(format),
                target: TargetProtection::Cbcs,
            })
            .unwrap_err();
        assert_eq!(error, super::super::vp9::PROTECTION_NOT_SUPPORTED);
        assert!(!output.exists());
    }
    let output = root.path().join("saved.mp4");
    let mut plan = command
        .create_execution_plan(ExecutionPlanRequest {
            input,
            output: &output,
            format: Some(OutputFormat::Mp4),
            target: TargetProtection::Cenc,
        })
        .unwrap();
    let qualified = command
        .validate_source(input, SourceValidationPurpose::Conversion)
        .unwrap();
    assert!(qualified.has_vp9);
    assert!(
        serde_json::to_value(&qualified)
            .unwrap()
            .get("has_vp9")
            .is_none()
    );
    plan.intent.target = TargetProtection::Cbcs;
    plan.plan_id = identity(&plan).unwrap();
    validate(&plan).unwrap();
    let saved = root.path().join("intent.plan.json");
    write_new(&saved, &plan).unwrap();
    let SavedPlan::Single(reopened) = read_saved(&saved).unwrap() else {
        panic!("single plan expected");
    };
    let bytes = std::fs::read(&saved).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("has_vp9"));
    assert_eq!(
        command.execute_plan(&reopened, &output_keys()).unwrap_err(),
        super::super::vp9::PROTECTION_NOT_SUPPORTED
    );
    assert!(!output.exists());
    assert_eq!(std::fs::read(saved).unwrap(), bytes);
    no_tools(&events);
}

#[test]
fn vp9_segment_plan_uses_native_codec_facts_when_manifest_codecs_are_absent() {
    let root = crate::native::private_tempdir().unwrap();
    let prepared = std::process::Command::new("packager")
        .arg("--single_threaded")
        .arg(format!(
            "input={},stream=video,init_segment={},segment_template={},playlist_name=video.m3u8",
            source().display(),
            root.path().join("init.mp4").display(),
            root.path().join("segment-$Number$.m4s").display(),
        ))
        .arg("--hls_master_playlist_output")
        .arg(root.path().join("master.m3u8"))
        .args(["--segment_duration", "2", "--fragment_duration", "2"])
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "independent HLS fixture preparation failed"
    );
    let input = root.path().join("master.m3u8");
    let text = std::fs::read_to_string(&input).unwrap();
    let without_codecs = text
        .lines()
        .map(|line| {
            if line.starts_with("#EXT-X-STREAM-INF:") {
                line.split(',')
                    .filter(|field| !field.starts_with("CODECS="))
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert!(!without_codecs.contains("vp09"));
    std::fs::write(&input, without_codecs).unwrap();
    let (command, events) = command();
    let command = command.with_processing(ProcessingMode::Segments);
    let output = root.path().join("refused");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let source = VerificationSource {
        input: &input,
        input_keys: &empty,
        hls_keys: &hls,
    };
    let qualified = command
        .validate_source(source, SourceValidationPurpose::Conversion)
        .unwrap();
    assert!(qualified.has_vp9);
    assert_eq!(
        command
            .create_execution_plan(ExecutionPlanRequest {
                input: source,
                output: &output,
                format: Some(OutputFormat::Hls),
                target: TargetProtection::Cbcs,
            })
            .unwrap_err(),
        super::super::vp9::PROTECTION_NOT_SUPPORTED
    );
    assert!(!output.exists());
    no_tools(&events);
}
