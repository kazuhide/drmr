//! Opt-in measurements preserve the complete media proof and the default JSON contract.
use serde_json::Value;
use std::{path::Path, process::Command};

fn convert(source: &Path, output: &Path, measure: bool) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command
        .arg("convert")
        .arg("--decode")
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
    assert_eq!(
        measured["verification"]["decode_evidence"],
        serde_json::json!({"status":"content-compared","equal":true})
    );
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
        .arg("--decode")
        .arg(&target)
        .arg("--source")
        .arg(&source)
        .args(["--measure", "--json"])
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}", result.stderr);
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(
        report["decode_evidence"],
        serde_json::json!({"status":"content-compared","equal":true})
    );
    assert_eq!(report["measurements"]["schema_version"], 1);
    let encoded = serde_json::to_string(&report["measurements"]).unwrap();
    assert!(!encoded.contains(&directory.path().to_string_lossy().to_string()));
    assert!(!encoded.contains("clip.mp4"));
    assert_eq!(std::fs::read(source).unwrap(), before);
}

#[test]
fn measurement_helper_observes_real_bbb_tools_in_both_worker_modes() {
    for condition in ["uncontrolled", "warmed-by-equivalent-verified-run"] {
        let directory = tempfile::tempdir().unwrap();
        let harness = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/bbb/measurement_regression.py");
        let output = Command::new(if cfg!(windows) { "python" } else { "python3" })
            .arg(harness)
            .arg("--drmr")
            .arg(env!("CARGO_BIN_EXE_drmr"))
            .arg("--output")
            .arg(directory.path())
            .arg("--condition")
            .arg(condition)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["success"], true);
        assert_eq!(report["runs"].as_array().unwrap().len(), 2);
        assert_eq!(
            report["warmups"].as_array().unwrap().len(),
            if condition == "uncontrolled" { 0 } else { 2 }
        );
        for row in report["runs"].as_array().unwrap() {
            assert_eq!(
                row["conversion_proof"]["decode_evidence"],
                serde_json::json!({"status":"content-compared","equal":true})
            );
            assert_eq!(
                row["independent_proof"]["decode_evidence"],
                serde_json::json!({"status":"content-compared","equal":true})
            );
            assert_eq!(
                row["independent_proof"]["checks"]["packet_timing_equal"],
                true
            );
        }
        println!("{}", String::from_utf8_lossy(&output.stdout));
    }
}

#[test]
fn measurements_require_json_and_failure_results_keep_the_existing_exit_contract() {
    for command in ["convert", "verify"] {
        let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
            .args([command, "--measure"])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
    }
    let directory = tempfile::tempdir().unwrap();
    let missing = directory.path().join("source-does-not-exist.mp4");
    let default = convert(&missing, &directory.path().join("plain.mp4"), false);
    let measured = convert(&missing, &directory.path().join("measured.mp4"), true);
    assert_eq!(default.status.code(), measured.status.code());
    assert_eq!(default.stdout, measured.stdout);
    assert_eq!(default.stderr, measured.stderr);
    assert!(!directory.path().join("measured.mp4").exists());
}
