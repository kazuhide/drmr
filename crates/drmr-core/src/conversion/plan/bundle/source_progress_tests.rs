//! Admission progress covers reader work and its remaining validation owner.
use super::*;
use crate::{
    events::{Collector, NoSink, Record, Sink},
    toolrun::{Secrets, ToolRunner},
};
use std::{collections::BTreeMap, fs, sync::Arc, time::Duration};

fn source(root: &Path) -> PathBuf {
    fs::create_dir(root).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    for name in ["first", "second"] {
        let directory = root.join(name);
        fs::create_dir(&directory).unwrap();
        let created = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(&clip)
            .args([
                "-map",
                "0:v:0",
                "-t",
                "10",
                "-c",
                "copy",
                "-an",
                "-hls_time",
                "2",
                "-hls_playlist_type",
                "vod",
                "-hls_segment_filename",
            ])
            .arg(directory.join("segment-%02d.ts"))
            .arg(directory.join("media.m3u8"))
            .output()
            .unwrap();
        assert!(
            created.status.success(),
            "short transport fixture creation failed"
        );
    }
    let input = root.join("master.m3u8");
    fs::write(
        &input,
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100000,CODECS=\"avc1.64001f\"\nfirst/media.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=200000,CODECS=\"avc1.64001f\"\nsecond/media.m3u8\n",
    )
    .unwrap();
    input
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn read(directory: &Path, root: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                read(&path, root, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().into(),
                    fs::read(path).unwrap(),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    read(root, root, &mut files);
    files
}

fn command(sink: Arc<dyn Sink>) -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "must-not-start-decoder",
    )
    .with_ffprobe("must-not-start-probe")
    .with_activity_sink(sink)
    .with_max_workers(Some(1))
    .unwrap()
    .with_timeout(Duration::from_secs(15))
}

fn outputs() -> Vec<BundleOutputIntent> {
    [("dash", OutputFormat::Dash), ("hls", OutputFormat::Hls)]
        .into_iter()
        .map(|(id, format)| BundleOutputIntent {
            id: id.into(),
            format,
            target: TargetProtection::Clear,
            drms: vec![],
            conformance: ManifestValidation::None,
            processing: Some(ProcessingMode::Stitch),
            dash_addressing: None,
        })
        .collect()
}

fn is_admission(record: &Record, stage: &str) -> bool {
    record.get("stage").and_then(serde_json::Value::as_str) == Some(stage)
        && record.get("subject").and_then(serde_json::Value::as_str) == Some("source")
        && record.get("purpose").and_then(serde_json::Value::as_str) == Some("admission")
        && !record.contains_key("output_index")
}

fn assert_admission(records: &[Record]) {
    let qualified = records
        .iter()
        .position(|record| {
            record.get("event").and_then(serde_json::Value::as_str) == Some("source-qualified")
                && record.get("purpose").and_then(serde_json::Value::as_str) == Some("conversion")
                && !record.contains_key("output_index")
        })
        .expect("the actual source admission must qualify the source");
    let before: Vec<_> = records[..qualified]
        .iter()
        .filter(|record| is_admission(record, "media-progress"))
        .collect();
    assert!(
        !before.is_empty(),
        "source admission must issue its own media progress before qualification"
    );
    let last = before.last().unwrap();
    assert_eq!(last["unit"], "renditions");
    assert_eq!(last["total_units"], 2);
    assert_eq!(last["completed_units"], 2);
    assert_eq!(
        last["state"], "running",
        "reader completion must retain the owner's remaining validation tail"
    );
    assert!(
        before
            .iter()
            .all(|record| { record["state"] != "finished" && record["state"] != "aborted" })
    );
    let issued: Vec<_> = records[..qualified]
        .iter()
        .filter(|record| is_admission(record, "rendition-progress"))
        .collect();
    assert!(
        issued
            .iter()
            .any(|record| record["active_units"].as_u64().unwrap() > 0)
    );
    let last = issued.last().unwrap();
    assert_eq!(last["operation"], "preparation");
    assert_eq!(last["started_units"], 2);
    assert_eq!(last["completed_units"], 2);
    assert_eq!(last["active_units"], 0);
    assert_eq!(last["waiting_units"], 0);
    assert_eq!(last["stopped_units"], 0);
    let finished = records
        .iter()
        .position(|record| is_admission(record, "media-progress") && record["state"] == "finished")
        .expect("the admission owner must close its media batch");
    let source_finished = records
        .iter()
        .position(|record| {
            record.get("stage").and_then(serde_json::Value::as_str) == Some("pipeline")
                && record.get("scope").and_then(serde_json::Value::as_str) == Some("source")
                && record.get("state").and_then(serde_json::Value::as_str) == Some("finished")
        })
        .expect("the actual source phase must finish");
    assert!(qualified < finished && finished < source_finished);
}

fn exercise(replay: bool) {
    let root = crate::native::private_tempdir().unwrap();
    let input = source(&root.path().join("source"));
    let original = snapshot(input.parent().unwrap());
    let output = root.path().join("result");
    let observed = Arc::new(Collector::default());
    let command = command(observed.clone());
    let keys = BundleOperatorKeys::default();
    let tools = ManifestValidationTools::default();
    let outputs = outputs();
    let plan = replay.then(|| {
        self::command(Arc::new(NoSink))
            .create_bundle_plan(BundlePlanRequest {
                input: VerificationSource {
                    input: &input,
                    input_keys: &keys.input,
                    hls_keys: &keys.hls_input,
                },
                output_root: &output,
                outputs: &outputs,
            })
            .unwrap()
    });
    let report = if let Some(plan) = &plan {
        command.execute_bundle_plan(plan, &keys, &tools)
    } else {
        command.convert_bundle(&input, &outputs, &output, &keys, &tools)
    }
    .unwrap();
    assert_eq!(report.outputs.len(), 2);
    for output in &report.outputs {
        assert!(output.conversion.verification.passed);
        assert!(output.conversion.verification.checks.is_pass());
    }
    assert_admission(&observed.records());
    assert_eq!(snapshot(input.parent().unwrap()), original);
    if let Some(plan) = plan {
        let verification = Arc::new(Collector::default());
        let report = command
            .with_activity_sink(verification.clone())
            .verify_bundle_plan(&plan, &keys, &tools)
            .unwrap();
        assert!(report.passed && report.source_unchanged);
        assert!(!verification.records().iter().any(|record| {
            record.get("stage").and_then(serde_json::Value::as_str) == Some("pipeline")
                && record.get("scope").and_then(serde_json::Value::as_str) == Some("source")
        }));
        assert_eq!(snapshot(input.parent().unwrap()), original);
    }
}

#[test]
fn direct_bundle_source_admission_reports_actual_reader_work_and_validation_tail() {
    exercise(false);
}

#[test]
fn saved_bundle_source_admission_reports_actual_reader_work_and_validation_tail() {
    exercise(true);
}
