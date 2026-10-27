//! Common commands keep multi-output intent and validator execution explicit.
use serde_json::{Value, json};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn invoke(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(args)
        .output()
        .unwrap()
}

fn recipe(root: &Path, value: &Value) -> PathBuf {
    let path = root.join("outputs.json");
    drmr_core::native::create_private_new(&path)
        .unwrap()
        .write_all(value.to_string().as_bytes())
        .unwrap();
    path
}

fn outputs() -> Value {
    json!({"schema_version":1,"outputs":[
        {"id":"dash","format":"dash","target":"clear","drms":[]},
        {"id":"hls","format":"hls","target":"clear","drms":[]}
    ]})
}

fn decoded_comparison(proof: &Value) {
    assert_eq!(proof["schema_version"], 2);
    assert_eq!(proof["required_depth"], "decoded");
    assert_eq!(proof["scope"], "comparison");
    assert_eq!(proof["passed"], true);
    assert_eq!(
        proof["checks"],
        json!({"stream_metadata":"pass", "packet_payloads_equal":true, "packet_timing_equal":true})
    );
    assert_eq!(
        proof["decode_evidence"],
        json!({"status":"content-compared", "equal":true})
    );
    assert_eq!(proof["license_playback_verified"], false);
    assert!(proof.get("media_verified").is_none());
    assert!(proof.get("failure").is_none());
    assert_eq!(proof["native_manifest"]["errors"], 0);
    assert_eq!(proof["native_manifest"]["truncated"], false);
    assert_eq!(proof["native_manifest"]["incomplete"], false);
}

fn prepare_fragmented_video(input: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let prepared = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(fixture)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-an",
            "-map_metadata",
            "-1",
            "-bitexact",
            "-movflags",
            "delay_moov+default_base_moof+frag_keyframe",
        ])
        .arg(input)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
}

fn failure(result: &Output, code: &str) -> Value {
    failure_with_exit(result, code, 2)
}

fn failure_with_exit(result: &Output, code: &str, exit: i32) -> Value {
    assert_eq!(result.status.code(), Some(exit));
    let report: Value = serde_json::from_slice(&result.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&result.stderr)));
    assert_eq!(report["status"], "failed");
    assert_eq!(report["error"]["code"], code);
    for bytes in [&result.stdout, &result.stderr] {
        assert!(!String::from_utf8_lossy(bytes).contains("confidential-sentinel"));
    }
    report
}

#[test]
fn help_exposes_output_sets_and_explicit_manifest_validator_executables() {
    for command in ["plan", "convert"] {
        let result = invoke(&[command, "--help"]);
        assert!(result.status.success());
        let help = String::from_utf8(result.stdout).unwrap();
        assert!(help.contains("--outputs"), "{help}");
        assert!(help.contains("--decode"), "{help}");
    }
    for command in ["convert", "verify"] {
        let result = invoke(&[command, "--help"]);
        let help = String::from_utf8(result.stdout).unwrap();
        assert!(help.contains("--hls-validator-bin"), "{help}");
        assert!(help.contains("--dash-validator-bin"), "{help}");
        assert!(help.contains("manifest"), "{help}");
        assert!(help.contains("--decode"), "{help}");
        if command == "verify" {
            assert!(help.contains("--manifest-only"), "{help}");
        }
    }
}

#[test]
fn manifest_only_cannot_claim_a_source_comparison_or_load_keys() {
    for extra in [
        vec!["--source", "source.mp4"],
        vec!["--keys-file", "keys.json"],
        vec!["--plan", "plan.json"],
        vec!["--decode"],
    ] {
        let mut args = vec!["verify", "manifest.mpd", "--manifest-only"];
        args.extend(extra);
        let result = invoke(&args);
        assert_eq!(result.status.code(), Some(2));
        assert_eq!(result.stdout, [] as [u8; 0]);
    }
}

#[test]
fn output_sets_refuse_single_output_and_saved_plan_overrides() {
    for command in ["plan", "convert"] {
        for extra in [vec!["--to", "clear"], vec!["--format", "dash"]] {
            let mut args = vec![
                command,
                "missing.mp4",
                "--outputs",
                "outputs.json",
                "--output",
                "result",
            ];
            if command == "plan" {
                args.extend(["--save", "plan.json"]);
            }
            args.extend(extra);
            let result = invoke(&args);
            assert_eq!(result.status.code(), Some(2));
            assert_eq!(result.stdout, [] as [u8; 0]);
        }
    }
    let result = invoke(&[
        "convert",
        "--plan",
        "plan.json",
        "--outputs",
        "outputs.json",
    ]);
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(result.stdout, [] as [u8; 0]);
}

#[test]
fn invalid_output_recipe_fails_privately_before_source_checks_or_publication() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let path = recipe(
        root.path(),
        &json!({
            "schema_version":1,"outputs":[],"license_url":"confidential-sentinel"
        }),
    );
    let destination = root.path().join("result");
    let saved = root.path().join("plan.json");
    let result = invoke(&[
        "plan",
        "missing.mp4",
        "--outputs",
        path.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--save",
        saved.to_str().unwrap(),
        "--json",
    ]);
    failure(&result, "output-spec-invalid");
    assert!(!destination.exists() && !saved.exists());
}

#[test]
fn duplicate_recipe_fields_are_not_silently_replaced() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let path = root.path().join("outputs.json");
    drmr_core::native::create_private_new(&path).unwrap().write_all(
        br#"{"schema_version":1,"schema_version":1,"outputs":[{"id":"dash","format":"dash","target":"clear","drms":[]}]}"#
    ).unwrap();
    let destination = root.path().join("result");
    let saved = root.path().join("plan.json");
    let result = invoke(&[
        "plan",
        "missing.mp4",
        "--outputs",
        path.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--save",
        saved.to_str().unwrap(),
        "--json",
    ]);
    failure(&result, "output-spec-invalid");
    assert!(!destination.exists() && !saved.exists());
}

#[test]
fn direct_multi_output_url_still_requires_retained_download_directory() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let path = recipe(root.path(), &outputs());
    let destination = root.path().join("result");
    let result = invoke(&[
        "convert",
        "https://example.invalid/master.m3u8?confidential-sentinel",
        "--outputs",
        path.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--json",
    ]);
    failure(&result, "download-directory-required");
    assert!(!destination.exists());
}

#[test]
fn validator_binary_options_are_runtime_settings_not_plan_overrides() {
    let result = invoke(&[
        "verify",
        "--plan",
        "missing.plan.json",
        "--hls-validator-bin",
        "confidential-sentinel",
        "--dash-validator-bin",
        "confidential-sentinel",
        "--json",
    ]);
    failure(&result, "execution-plan-invalid");
}

#[test]
fn common_commands_save_execute_and_freshly_verify_two_real_outputs() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    prepare_fragmented_video(&input);
    let recipe = recipe(root.path(), &outputs());
    let destination = root.path().join("result");
    let plan = root.path().join("conversion.plan.json");
    let planned = invoke(&[
        "plan",
        "--decode",
        input.to_str().unwrap(),
        "--outputs",
        recipe.to_str().unwrap(),
        "--output",
        destination.to_str().unwrap(),
        "--save",
        plan.to_str().unwrap(),
        "--json",
    ]);
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let value: Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(value["kind"], "execution-plan");
    assert_eq!(value["schema_version"], 10);
    assert_eq!(value["required_depth"], "decoded");
    assert_eq!(value["outputs"].as_array().unwrap().len(), 2);
    let plan_id = value["plan_id"].clone();
    let saved = std::fs::read(&plan).unwrap();
    assert_eq!(value, serde_json::from_slice::<Value>(&saved).unwrap());
    assert!(!destination.exists());
    let converted = invoke(&["convert", "--plan", plan.to_str().unwrap(), "--json"]);
    assert!(
        converted.status.success(),
        "{} {}",
        String::from_utf8_lossy(&converted.stdout),
        String::from_utf8_lossy(&converted.stderr)
    );
    let value: Value = serde_json::from_slice(&converted.stdout).unwrap();
    assert_eq!(value["kind"], "bundle-conversion");
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["plan_id"], plan_id);
    assert_eq!(value["outputs"].as_array().unwrap().len(), 2);
    for child in value["outputs"].as_array().unwrap() {
        decoded_comparison(&child["conversion"]["verification"]);
        assert!(child["conformance"].is_null());
        assert_eq!(child["signaling"]["verified"], false);
    }
    assert!(destination.join("dash/manifest.mpd").is_file());
    assert!(destination.join("hls/master.m3u8").is_file());
    let verified = invoke(&["verify", "--plan", plan.to_str().unwrap(), "--json"]);
    assert!(
        verified.status.success(),
        "{} {}",
        String::from_utf8_lossy(&verified.stdout),
        String::from_utf8_lossy(&verified.stderr)
    );
    let value: Value = serde_json::from_slice(&verified.stdout).unwrap();
    assert_eq!(value["kind"], "bundle-verification");
    assert_eq!(value["schema_version"], 2);
    assert_eq!(value["plan_id"], plan_id);
    assert_eq!(value["source_unchanged"], true);
    assert_eq!(value["passed"], true);
    assert_eq!(value["outputs"].as_array().unwrap().len(), 2);
    for child in value["outputs"].as_array().unwrap() {
        assert_eq!(child["output_matches_plan"], true);
        decoded_comparison(&child["verification"]);
    }
    assert_eq!(std::fs::read(&plan).unwrap(), saved);
    let mut changed = std::fs::read(&input).unwrap();
    *changed.last_mut().unwrap() ^= 1;
    std::fs::write(&input, changed).unwrap();
    let refused = invoke(&["verify", "--plan", plan.to_str().unwrap(), "--json"]);
    failure_with_exit(&refused, "execution-plan-source-changed", 1);
    assert!(destination.join("dash/manifest.mpd").is_file());
    assert!(destination.join("hls/master.m3u8").is_file());
}
