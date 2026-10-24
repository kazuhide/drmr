//! Protected operator configuration supplies both keys for a finite progressive presentation.
use std::{io::Write as _, path::Path, process::Command};

const VIDEO_KEY: &str = "101112131415161718191a1b1c1d1e1f";
const AUDIO_KEY: &str = "202122232425262728292a2b2c2d2e2f";

fn assert_verified(report: &serde_json::Value) {
    assert_eq!(report["passed"], true);
    assert_eq!(
        report["decode_evidence"],
        serde_json::json!({"status":"content-compared","equal":true})
    );
    assert_eq!(report["license_playback_verified"], false);
    assert_eq!(report["checks"]["stream_metadata"], "pass");
    for name in ["packet_payloads_equal", "packet_timing_equal"] {
        assert_eq!(report["checks"][name], true);
    }
}

#[test]
fn real_two_key_finite_convert_and_verify_use_the_input_key_domain_only() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("private");
    drmr_core::native::create_private_dir_all(&root).unwrap();
    let keyfile = root.join("operator.json");
    let body = serde_json::json!({
        "schema_version": 1,
        "input": { "keys": [
            {"kid": "000102030405060708090a0b0c0d0e0f", "key": VIDEO_KEY},
            {"kid": "303132333435363738393a3b3c3d3e3f", "key": AUDIO_KEY}
        ] }
    })
    .to_string();
    drmr_core::native::create_private_new(&keyfile)
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/cenc-iv16.mp4");
    let source_before = std::fs::read(&source).unwrap();
    let output = root.join("clear.mp4");
    let converted = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("convert")
        .arg("--decode")
        .arg(&source)
        .args(["--to", "clear", "--format", "mp4", "--output"])
        .arg(&output)
        .arg("--keys-file")
        .arg(&keyfile)
        .args([
            "--json",
            "--packager-bin",
            "unused-packager-for-progressive-profile",
        ])
        .output()
        .unwrap();
    assert!(
        converted.status.success(),
        "finite conversion failed with exit {:?}",
        converted.status.code()
    );
    let report: serde_json::Value = serde_json::from_slice(&converted.stdout).unwrap();
    assert_eq!(report["output"]["protection"], "clear");
    assert_eq!(report["output"]["tracks"].as_array().unwrap().len(), 2);
    assert_verified(&report["verification"]);
    let verified = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("verify")
        .arg("--decode")
        .arg(&output)
        .arg("--source")
        .arg(&source)
        .arg("--keys-file")
        .arg(&keyfile)
        .args(["--json", "--max-workers", "1"])
        .output()
        .unwrap();
    assert!(
        verified.status.success(),
        "finite verification failed with exit {:?}",
        verified.status.code()
    );
    assert_verified(&serde_json::from_slice::<serde_json::Value>(&verified.stdout).unwrap());
    assert_eq!(std::fs::read(&source).unwrap(), source_before);
    assert_eq!(std::fs::read_to_string(&keyfile).unwrap(), body);
    for bytes in [
        &converted.stdout,
        &converted.stderr,
        &verified.stdout,
        &verified.stderr,
    ] {
        let text = String::from_utf8_lossy(bytes);
        for key in [VIDEO_KEY, AUDIO_KEY] {
            assert!(!text.contains(key), "secret in CLI output");
        }
        assert!(!text.contains("-decryption_keys"));
    }
}
