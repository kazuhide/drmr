//! Native batch entry point, staging, worker protocol and resume contracts.

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn batch(work_list: &Path, output: &Path, resume: &Path, store: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("batch")
        .arg("--work-list")
        .arg(work_list)
        .arg("--output")
        .arg(output)
        .arg("--resume-log")
        .arg(resume)
        .arg("--storage-root")
        .arg(store)
        .args(["--max-attempts", "1", "--per-asset-timeout-s", "10"])
        .env_remove("DRM_KID")
        .env_remove("DRM_KEY")
        .env_remove("DRM_IV")
        .output()
        .unwrap()
}

fn records(output: &Output) -> Vec<Value> {
    String::from_utf8(output.stdout.clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn native_batch_stages_before_missing_keys_then_resumes_without_a_new_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let store = temp.path().join("store 日本語");
    std::fs::create_dir_all(store.join("catalog")).unwrap();
    let manifest = store.join("catalog/manifest.mpd");
    std::fs::write(&manifest, br#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"><BaseURL>https://media.example/input/</BaseURL></MPD>"#).unwrap();
    let source_before = std::fs::read(&manifest).unwrap();
    let work_list = temp.path().join("work.jsonl");
    std::fs::write(&work_list, concat!(
        "{\"schema_version\":\"1\"}\n",
        "{\"asset_id\":\"native\",\"kind\":\"dash-cenc\",\"dash_manifest\":\"catalog/manifest.mpd\",\"hls_master\":null}\n"
    )).unwrap();
    let output = temp.path().join("output");
    let resume = temp.path().join("logs/resume.jsonl");
    let first = batch(&work_list, &output, &resume, &store);
    assert_eq!(
        first.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let rows = records(&first);
    assert!(
        rows.iter()
            .any(|row| row["stage"] == "batch-report" && row["failed"] == 1)
    );
    let saved = std::fs::read(&resume).unwrap();
    let log = drmr_core::batch::load_resume_log(&resume).unwrap();
    assert_eq!(log.get("native").unwrap().reason, "missing-key");
    assert_eq!(std::fs::read(&manifest).unwrap(), source_before);
    let second = batch(&work_list, &output, &resume, &store);
    let rows = records(&second);
    assert!(
        rows.iter()
            .any(|row| row["stage"] == "batch-report" && row["skipped"] == 1),
        "{rows:?}"
    );
    assert_eq!(std::fs::read(resume).unwrap(), saved);
    assert_eq!(std::fs::read(manifest).unwrap(), source_before);
}
