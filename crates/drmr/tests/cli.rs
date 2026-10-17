//! The CLI's exit codes and output shapes.

use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr-core/tests/fixtures")
        .join(name)
}

fn drmr(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stdout)))
}

#[test]
fn version_prints_the_workspace_version() {
    let output = drmr(&["version"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("drmr {}\n", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn verify_snapshot_reports_ok_changed_and_invalid_with_their_exit_codes() {
    let sample = fixture("analysis.sample.json");
    let output = drmr(&["verify-snapshot", sample.to_str().unwrap()]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = stdout_json(&output);
    assert_eq!(report["status"], "ok");
    assert_eq!(report["measured_digest"], report["recorded_digest"]);
    assert_eq!(report["analysis_id"], "hls-e63d73664e10730c148d26c0");
    assert_eq!(report["files_verified"], Value::Null);
    assert_eq!(report["resources"], 2);

    let dir = tempfile::tempdir().unwrap();
    let tampered = std::fs::read_to_string(&sample)
        .unwrap()
        .replace("\"asset_id\": \"sample\"", "\"asset_id\": \"other\"");
    std::fs::write(dir.path().join("analysis.json"), tampered).unwrap();
    let output = drmr(&["verify-snapshot", dir.path().to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(stdout_json(&output)["status"], "snapshot-changed");
    assert!(String::from_utf8_lossy(&output.stderr).contains("snapshot-changed"));

    let output = drmr(&[
        "verify-snapshot",
        dir.path().join("missing").to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout_json(&output)["status"], "snapshot-invalid");

    // --files on the sample: its resource files do not exist beside it
    let output = drmr(&["verify-snapshot", "--files", sample.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(stdout_json(&output)["status"], "snapshot-invalid");

    let output = drmr(&["verify-snapshot"]);
    assert_eq!(output.status.code(), Some(2), "clap usage error");
}

#[test]
fn canonical_prints_the_digest_input_bytes() {
    let output = drmr(&["canonical", fixture("canonical.in.json").to_str().unwrap()]);
    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        std::fs::read(fixture("canonical.expected.json")).unwrap()
    );
    let output = drmr(&["canonical", fixture("canonical.sha256").to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
}
