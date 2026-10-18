//! Opt-in measurements preserve the complete media proof and the default JSON contract.
use serde_json::Value;
use std::{path::Path, process::Command};

fn convert(source: &Path, output: &Path, measure: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command
        .arg("convert")
        .arg(source)
        .args(["--to", "clear", "--format", "mp4", "--json", "--output"])
        .arg(output);
    if measure {
        command.arg("--measure");
    }
    command.output().unwrap()
}

#[test]
fn measured_conversion_and_verification_keep_the_complete_proof_and_default_shape() {
    let directory = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let before = std::fs::read(&source).unwrap();
    let plain = convert(&source, &directory.path().join("plain.mp4"), false);
    assert!(plain.status.success(), "{:?}", plain.stderr);
    let plain: Value = serde_json::from_slice(&plain.stdout).unwrap();
    assert!(plain.get("measurements").is_none());

    let target = directory.path().join("measured.mp4");
    let measured = convert(&source, &target, true);
    assert!(measured.status.success(), "{:?}", measured.stderr);
    let measured: Value = serde_json::from_slice(&measured.stdout).unwrap();
    assert_eq!(plain["verification"], measured["verification"]);
    assert_eq!(measured["verification"]["media_verified"], true);
    assert_eq!(measured["measurements"]["schema_version"], 1);
    assert!(
        measured["measurements"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["calls"].as_u64().unwrap() > 0)
    );
    assert_eq!(measured["measurements"]["io"]["status"], "unmeasured");
    assert!(
        measured["measurements"]["workspace"]["checkpoints"]
            .as_u64()
            .unwrap()
            > 0
    );

    let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("verify")
        .arg(&target)
        .arg("--source")
        .arg(&source)
        .args(["--measure", "--json"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}", result.stderr);
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(report["media_verified"], true);
    assert_eq!(report["measurements"]["schema_version"], 1);
    let encoded = serde_json::to_string(&report["measurements"]).unwrap();
    assert!(!encoded.contains(&directory.path().to_string_lossy().to_string()));
    assert!(!encoded.contains("clip.mp4"));
    assert_eq!(std::fs::read(source).unwrap(), before);
}
