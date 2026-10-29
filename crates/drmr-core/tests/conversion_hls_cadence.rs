//! A declared HLS ceiling is separate from the admitted video segment cadence.
mod common;
use drmr_core::{
    conversion::{
        Aes128Key, ConversionRequest, Converter, HlsInputKeys, HlsOutputKey, InputKeys,
        VerificationSource,
        key_file::OperatorKeys,
        plan::{self, ExecutionPlanRequest},
    },
    events::{Collector, NoSink},
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn converter(events: Arc<dyn drmr_core::events::Sink>) -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events)),
        "packager",
        "must-not-run-decoder",
    )
    .with_ffprobe("must-not-run-probe")
}

fn source(root: &Path) -> PathBuf {
    fs::create_dir(root).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut command = std::process::Command::new("packager");
    command.arg("--single_threaded");
    for role in ["video", "audio"] {
        let directory = root.join(role);
        fs::create_dir(&directory).unwrap();
        command.arg(format!(
            "in={},stream={role},init_segment={}/init.mp4,segment_template={}/seg_$Number$.m4s",
            clip.display(),
            directory.display(),
            directory.display()
        ));
    }
    let manifest = root.join("manifest.mpd");
    let result = command
        .args([
            "--segment_duration",
            "2",
            "--fragment_duration",
            "2",
            "--mp4_reset_initial_composition_offset_to_zero=false",
            "--generate_static_live_mpd",
            "--mpd_output",
        ])
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(result.status.success(), "BBB fixture generation failed");
    manifest
}

fn hls_rows(root: &Path) -> Vec<(u64, Vec<f64>)> {
    let mut rows = Vec::new();
    for entry in fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rows.extend(hls_rows(&path));
        } else if path
            .extension()
            .is_some_and(|extension| extension == "m3u8")
        {
            let text = fs::read_to_string(path).unwrap();
            if let Some(target) = text
                .lines()
                .find_map(|line| line.strip_prefix("#EXT-X-TARGETDURATION:"))
            {
                rows.push((
                    target.parse().unwrap(),
                    text.lines()
                        .filter_map(|line| line.strip_prefix("#EXTINF:"))
                        .map(|value| value.split(',').next().unwrap().parse().unwrap())
                        .collect(),
                ));
            }
        }
    }
    rows
}

#[test]
fn two_second_video_under_a_three_second_bound_survives_clear_and_aes_repackaging() {
    let root = common::private_tempdir().unwrap();
    let dash = source(&root.path().join("dash"));
    let key = HlsOutputKey::new(
        "https://synthetic.invalid/source",
        Aes128Key::from_hex(&"31".repeat(16)).unwrap(),
    )
    .unwrap();
    for (ordinal, protection) in [TargetProtection::Clear, TargetProtection::HlsAes128]
        .into_iter()
        .enumerate()
    {
        let initial = root.path().join(format!("source-{ordinal}"));
        converter(Arc::new(NoSink))
            .convert(ConversionRequest {
                input: &dash,
                output: &initial,
                format: OutputFormat::Hls,
                target: protection,
                input_keys: &InputKeys::default(),
                hls_input_keys: &HlsInputKeys::default(),
                output_key: None,
                hls_key_uri: None,
                hls_output_key: (protection == TargetProtection::HlsAes128).then_some(&key),
            })
            .unwrap();
        let rows = hls_rows(&initial);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(target, _)| *target == 3));
        assert!(
            rows.iter()
                .flat_map(|(_, durations)| durations)
                .all(|duration| *duration < 2.1)
        );
        let original = common::snapshot_tree(&initial);
        let input = initial.join("master.m3u8");
        let mut keys = OperatorKeys::default();
        keys.hls_input = if protection == TargetProtection::HlsAes128 {
            HlsInputKeys::new(vec![(
                key.key_uri().into(),
                Aes128Key::from_hex(&"31".repeat(16)).unwrap(),
            )])
            .unwrap()
        } else {
            HlsInputKeys::default()
        };
        keys.hls_output = Some(
            HlsOutputKey::new(
                "https://synthetic.invalid/result",
                Aes128Key::from_hex(&"42".repeat(16)).unwrap(),
            )
            .unwrap(),
        );
        let events = Arc::new(Collector::default());
        let command = converter(events.clone());
        let output = root.path().join(format!("result-{ordinal}"));
        let loaded = execute_saved(&command, &input, &output, &keys);
        let actual = hls_rows(&loaded.intent.output);
        assert_eq!(actual.len(), 2);
        assert!(actual.iter().all(|(target, _)| *target == 3));
        assert!(
            actual
                .iter()
                .flat_map(|(_, durations)| durations)
                .all(|duration| *duration < 2.1)
        );
        assert_eq!(common::snapshot_tree(&initial), original);
        for record in events.records() {
            if record.get("event").and_then(serde_json::Value::as_str) == Some("command") {
                assert_eq!(
                    record.get("tool").and_then(serde_json::Value::as_str),
                    Some("packager")
                );
            }
        }
    }
}

fn execute_saved(
    command: &Converter,
    input: &Path,
    output: &Path,
    keys: &OperatorKeys,
) -> plan::ExecutionPlan {
    let saved = command
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output,
            format: Some(OutputFormat::Hls),
            target: TargetProtection::HlsAes128,
        })
        .unwrap();
    assert_eq!(saved.processing.unwrap().segment_duration_ms, Some(2000));
    let file = output.with_extension("plan.json");
    plan::write_new(&file, &saved).unwrap();
    let loaded = plan::read(&file).unwrap();
    assert!(
        command
            .execute_plan(&loaded, keys)
            .unwrap()
            .verification
            .passed
    );
    let verified = command.verify_plan(&loaded, keys).unwrap();
    assert!(
        verified.source_unchanged && verified.output_matches_plan && verified.verification.passed
    );
    loaded
}
