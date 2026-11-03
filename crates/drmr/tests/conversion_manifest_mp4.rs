//! Separate fragmented tracks form one native-verified MP4 through direct and saved-plan CLI paths.
use drmr_core::native;
use serde_json::{Value, json};
use std::{path::Path, process::Command};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const OUTPUT_KID: &str = "303132333435363738393a3b3c3d3e3f";
const OUTPUT_KEY: &str = "404142434445464748494a4b4c4d4e4f";

fn cli(root: &Path, arguments: &[&str]) -> Value {
    let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .current_dir(root)
        .args(arguments)
        .args(["--json", "--progress", "off"])
        .output()
        .unwrap();
    for bytes in [&result.stdout, &result.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains(KEY) && !text.contains(OUTPUT_KEY));
    }
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}
fn keys(root: &Path, name: &str, value: &Value) {
    serde_json::to_writer(native::create_private_new(&root.join(name)).unwrap(), value).unwrap();
}
fn proof(value: &Value) {
    assert_eq!(value["passed"], true, "{value}");
    assert_eq!(value["required_depth"], "native");
    assert_eq!(value["checks"]["stream_metadata"], "pass");
    assert_eq!(value["checks"]["packet_payloads_equal"], true);
    assert_eq!(value["checks"]["packet_timing_equal"], true);
    assert_eq!(value["decode_evidence"]["status"], "not-checked");
    assert_eq!(value["license_playback_verified"], false);
}
#[test]
fn native_joint_manifest_mp4_analyzes_plans_converts_and_verifies_with_distinct_keys() {
    let owner = native::private_tempdir().unwrap();
    let root = owner.path();
    let original = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let before = std::fs::read(&original).unwrap();
    keys(
        root,
        "input.json",
        &json!({"schema_version":2,"input":{"keys":[{"kid":KID,"key":KEY}]},"outputs":{}}),
    );
    keys(
        root,
        "both.json",
        &json!({"schema_version":2,"input":{"keys":[{"kid":KID,"key":KEY}]},"outputs":{"default":{"kid":OUTPUT_KID,"key":OUTPUT_KEY}}}),
    );
    keys(
        root,
        "result.json",
        &json!({"schema_version":2,"outputs":{"default":{"kid":OUTPUT_KID,"key":OUTPUT_KEY}}}),
    );
    for format in ["dash", "hls-fmp4"] {
        let output_key = if format == "dash" {
            json!({"kid":KID,"key":KEY})
        } else {
            json!({"kid":KID,"key":KEY,"hls_key_uri":"https://operator.invalid/manifest-key"})
        };
        let seed_keys = format!("{format}.keys.json");
        keys(
            root,
            &seed_keys,
            &json!({"schema_version":2,"outputs":{"default":output_key}}),
        );
        let directory = format!("{format}-source");
        proof(
            &cli(
                root,
                &[
                    "convert",
                    original.to_str().unwrap(),
                    "--encryption",
                    "cenc",
                    "--output-format",
                    format,
                    "--keys-file",
                    &seed_keys,
                    "--output-dir",
                    &directory,
                ],
            )["verification"],
        );
        let input = format!(
            "{directory}/{}",
            if format == "dash" {
                "manifest.mpd"
            } else {
                "master.m3u8"
            }
        );
        let analysis = cli(root, &["analyze", &input, "--keys-file", "input.json"]);
        assert_eq!(analysis["input_admitted"], true);
        assert!(
            analysis["eligible_formats"]
                .as_array()
                .unwrap()
                .contains(&json!("mp4"))
        );
        let direct = format!("{format}-clear.mp4");
        proof(
            &cli(
                root,
                &[
                    "convert",
                    &input,
                    "--encryption",
                    "clear",
                    "--output-format",
                    "mp4",
                    "--output-file",
                    &direct,
                    "--keys-file",
                    "input.json",
                ],
            )["verification"],
        );
        verify_saved_mp4(root, &input, format, &original);
    }
    assert_eq!(std::fs::read(original).unwrap(), before);
}

fn verify_saved_mp4(root: &Path, input: &str, format: &str, original: &Path) {
    let output = format!("{format}-cbcs.mp4");
    let plan = format!("{format}.plan.json");
    let saved = cli(
        root,
        &[
            "plan",
            input,
            "--encryption",
            "cbcs",
            "--output-format",
            "mp4",
            "--output-file",
            &output,
            "--keys-file",
            "both.json",
            "--save",
            &plan,
        ],
    );
    assert_eq!(saved["intent"]["format"], "mp4");
    let plan_bytes = std::fs::read(root.join(&plan)).unwrap();
    let converted = cli(
        root,
        &["convert", "--plan-file", &plan, "--keys-file", "both.json"],
    );
    proof(&converted["verification"]);
    assert_eq!(converted["output"]["tracks"].as_array().unwrap().len(), 2);
    let verified = cli(
        root,
        &["verify", "--plan-file", &plan, "--keys-file", "both.json"],
    );
    assert_eq!(verified["kind"], "plan-verification");
    assert_eq!(verified["plan_id"], saved["plan_id"]);
    assert_eq!(verified["source_unchanged"], true);
    assert_eq!(verified["output_matches_plan"], true);
    proof(&verified["verification"]);
    proof(&cli(
        root,
        &[
            "verify",
            &output,
            "--source",
            original.to_str().unwrap(),
            "--keys-file",
            "result.json",
        ],
    ));
    if format == "dash" {
        let decoded = cli(
            root,
            &[
                "verify",
                &output,
                "--source",
                original.to_str().unwrap(),
                "--keys-file",
                "result.json",
                "--decode",
            ],
        );
        assert_eq!(decoded["passed"], true);
        assert_eq!(decoded["required_depth"], "decoded");
        assert_eq!(
            decoded["decode_evidence"],
            json!({"status":"content-compared", "equal":true})
        );
    }
    assert_eq!(std::fs::read(root.join(&plan)).unwrap(), plan_bytes);
}
