//! Shared real-command assertions for presentation regressions.
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use serde_json::{Value, json};

// Public fixture material only; source and result keys are deliberately different.
const VIDEO_KEY: &str = "101112131415161718191a1b1c1d1e1f";
const AUDIO_KEY: &str = "202122232425262728292a2b2c2d2e2f";
const RESULT_ID: &str = "404142434445464748494a4b4c4d4e4f";
const RESULT_KEY: &str = "505152535455565758595a5b5c5d5e5f";
const RESULT_URI: &str = "https://operator.invalid/presentation-result";

pub(super) fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/bbb")
        .join(name)
}

fn cli(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(arguments)
        .args(decode_option(arguments))
        .output()
        .unwrap()
}

fn checked(result: &Output, operation: &str) -> Value {
    for bytes in [&result.stdout, &result.stderr] {
        let text = String::from_utf8_lossy(bytes);
        for key in [VIDEO_KEY, AUDIO_KEY, RESULT_KEY] {
            assert!(!text.contains(key), "key leaked during {operation}");
        }
        assert!(
            !text.contains(RESULT_URI),
            "key URI leaked during {operation}"
        );
    }
    assert!(
        result.status.success(),
        "{operation} failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}

fn assert_verified(report: &Value) {
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(
        report["decode_evidence"],
        serde_json::json!({"status":"content-compared","equal":true}),
        "{report}"
    );
    assert_eq!(report["license_playback_verified"], false);
    assert_eq!(report["checks"]["stream_metadata"], "pass");
    for check in ["packet_payloads_equal", "packet_timing_equal"] {
        assert_eq!(report["checks"][check], true, "{report}");
    }
}

pub(super) fn configuration(
    path: &Path,
    encrypted_source: bool,
    target: &str,
    format: &str,
) -> Vec<u8> {
    let mut value = json!({"schema_version": 1});
    if encrypted_source {
        value["input"] = json!({"keys": [
            {"kid": "000102030405060708090a0b0c0d0e0f", "key": VIDEO_KEY},
            {"kid": "303132333435363738393a3b3c3d3e3f", "key": AUDIO_KEY}
        ]});
    }
    match target {
        "clear" => {}
        "hls-aes128" => {
            value["output"] = json!({"key": RESULT_KEY, "hls_key_uri": RESULT_URI});
        }
        _ => {
            value["output"] = json!({"kid": RESULT_ID, "key": RESULT_KEY});
            if format == "hls" {
                value["output"]["hls_key_uri"] = json!(RESULT_URI);
            }
        }
    }
    let bytes = serde_json::to_vec(&value).unwrap();
    drmr_core::native::create_private_new(path)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
    bytes
}

pub(super) fn convert_and_verify(
    source: &Path,
    destination: &Path,
    config: &Path,
    format: &str,
    target: &str,
) -> Value {
    let planned = cli(&[
        "plan",
        source.to_str().unwrap(),
        "--to",
        target,
        "--format",
        format,
        "--json",
    ]);
    let plan = checked(&planned, &format!("{format}/{target} planning"));
    assert_eq!(plan["compatible"], true, "{plan}");
    assert_eq!(plan["execution_available"], true, "{plan}");
    let converted = cli(&[
        "convert",
        source.to_str().unwrap(),
        "--to",
        target,
        "--format",
        format,
        "--output",
        destination.to_str().unwrap(),
        "--keys-file",
        config.to_str().unwrap(),
        "--max-workers",
        "1",
        "--json",
    ]);
    let report = checked(&converted, &format!("{format}/{target} conversion"));
    assert_verified(&report["verification"]);
    assert!(
        plan["processing"]["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| {
                candidate["route"] == report["processing"]["route"]
                    && candidate["output_format"] == format
                    && candidate["revalidation_required"] == true
            }),
        "the executed handler must have been a conditional planning candidate"
    );
    let output = match format {
        "dash" => destination.join("manifest.mpd"),
        "hls" => destination.join("master.m3u8"),
        _ => destination.to_owned(),
    };
    let verified = cli(&[
        "verify",
        output.to_str().unwrap(),
        "--source",
        source.to_str().unwrap(),
        "--keys-file",
        config.to_str().unwrap(),
        "--max-workers",
        "1",
        "--json",
    ]);
    assert_verified(&checked(
        &verified,
        &format!("{format}/{target} fresh verification"),
    ));
    report
}

fn decode_option(arguments: &[&str]) -> Option<&'static str> {
    (matches!(
        arguments.first().copied(),
        Some("analyze" | "plan" | "convert" | "verify")
    ) && !arguments.contains(&"--decode")
        && !arguments.contains(&"--metadata-only")
        && !arguments.contains(&"--manifest-only")
        && (arguments.first().copied() != Some("plan") || arguments.contains(&"--save")))
    .then_some("--decode")
}
