//! Configuration failures identify safe schema locations without echoing operator data.
use serde_json::{Value, json};
use std::{io::Write as _, path::Path, process::Command};

const CANARY: &str = "PRIVATE_CONFIGURATION_CANARY_7e96419a";

fn write(path: &Path, bytes: &[u8]) {
    drmr_core::native::create_private_new(path)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

fn check(result: &std::process::Output, command: &str, code: &str) -> Value {
    assert_eq!(result.status.code(), Some(2));
    for stream in [&result.stdout, &result.stderr] {
        assert!(!String::from_utf8_lossy(stream).contains(CANARY));
    }
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["command"], command);
    assert_eq!(report["status"], "failed");
    assert_eq!(report["error"]["code"], code);
    report
}

#[test]
fn typed_key_errors_keep_existing_codes_and_show_only_schema_fields_and_indices() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let keys = root.path().join(format!("{CANARY}.json"));
    let value = json!({"schema_version":1,"input":{"keys":[
        {"kid":"000102030405060708090a0b0c0d0e0f","key":false}
    ]},CANARY:CANARY});
    write(&keys, value.to_string().as_bytes());
    for command in ["analyze", "plan", "convert", "verify"] {
        let mut process = Command::new(env!("CARGO_BIN_EXE_drmr"));
        process.args([command, "missing.mp4", "--keys-file"]);
        process.arg(&keys).args(["--json", "--progress", "json"]);
        match command {
            "plan" => {
                process.args(["--to", "clear", "--format", "dash", "--output"]);
                process
                    .arg(root.path().join("result"))
                    .arg("--save")
                    .arg(root.path().join("plan.json"));
            }
            "convert" => {
                process.args(["--to", "clear", "--format", "dash", "--output"]);
                process.arg(root.path().join("result"));
            }
            "verify" => {
                process.args(["--source", "original.mp4"]);
            }
            _ => {}
        }
        let report = check(&process.output().unwrap(), command, "key-file-invalid");
        assert_eq!(report["error"]["configuration"]["file"], "keys");
        assert_eq!(
            report["error"]["configuration"]["path"],
            json!(["input", "keys", 0, "key"])
        );
        assert_eq!(report["error"]["configuration"]["expected"], "hex-16-bytes");
        assert!(report["error"]["configuration"]["line"].as_u64().is_some());
        assert!(!root.path().join("result").exists());
        assert!(!root.path().join("plan.json").exists());
    }
}

#[test]
fn output_spec_errors_report_array_positions_without_ids_or_private_values() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let spec = root.path().join(format!("{CANARY}.json"));
    write(&spec, json!({"schema_version":3,"outputs":[{"id":CANARY,"format":true,"target":"clear","drms":[]}]}).to_string().as_bytes());
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    for command in ["plan", "convert"] {
        let mut process = Command::new(env!("CARGO_BIN_EXE_drmr"));
        process
            .arg(command)
            .arg(&source)
            .arg("--outputs")
            .arg(&spec)
            .arg("--output")
            .arg(root.path().join("result"))
            .arg("--json");
        if command == "plan" {
            process.arg("--save").arg(root.path().join("plan.json"));
        }
        let report = check(&process.output().unwrap(), command, "output-spec-invalid");
        assert_eq!(report["error"]["configuration"]["file"], "outputs");
        assert_eq!(
            report["error"]["configuration"]["path"],
            json!(["outputs", 0, "format"])
        );
        assert_eq!(
            report["error"]["configuration"]["expected"],
            "output-format"
        );
    }
}

#[test]
fn key_diagnostics_are_safe_in_human_json_and_all_progress_modes() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let keys = root.path().join(format!("{CANARY}.json"));
    write(
        &keys,
        json!({"schema_version":1,"input":{"keys":[
            {"kid":"000102030405060708090a0b0c0d0e0f","key":CANARY}
        ]}})
        .to_string()
        .as_bytes(),
    );
    for command in ["analyze", "plan", "convert", "verify"] {
        for mode in ["auto", "text", "json", "off"] {
            for as_json in [false, true] {
                let mut process = Command::new(env!("CARGO_BIN_EXE_drmr"));
                process
                    .args([command, "missing.mp4", "--keys-file"])
                    .arg(&keys)
                    .args(["--progress", mode]);
                if as_json {
                    process.arg("--json");
                }
                match command {
                    "plan" => {
                        process
                            .args(["--to", "clear", "--format", "dash", "--output"])
                            .arg(root.path().join("result"))
                            .arg("--save")
                            .arg(root.path().join("plan.json"));
                    }
                    "convert" => {
                        process
                            .args(["--to", "clear", "--format", "dash", "--output"])
                            .arg(root.path().join("result"));
                    }
                    "verify" => {
                        process.args(["--source", "source.mp4"]);
                    }
                    _ => {}
                }
                let output = process.output().unwrap();
                assert_eq!(output.status.code(), Some(2));
                for stream in [&output.stdout, &output.stderr] {
                    let text = String::from_utf8_lossy(stream);
                    assert!(!text.contains(CANARY));
                    assert!(!text.contains("invalid type:"));
                }
                if as_json {
                    let value = check(&output, command, "key-file-invalid");
                    assert_eq!(
                        value["error"]["configuration"]["path"],
                        json!(["input", "keys", 0, "key"])
                    );
                } else if mode != "json" {
                    assert!(String::from_utf8_lossy(&output.stderr).contains("input.keys[0].key"));
                }
            }
        }
    }
}

#[test]
fn named_output_keys_never_disclose_the_operator_output_id() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let keys = root.path().join("keys.json");
    let spec = root.path().join("outputs.json");
    write(
        &keys,
        json!({"schema_version":2,"outputs":{CANARY:{
            "kid":"000102030405060708090a0b0c0d0e0f","key":false
        }}})
        .to_string()
        .as_bytes(),
    );
    write(&spec, br#"{"schema_version":3,"outputs":[{"id":"clear","format":"dash","target":"clear","drms":[]}]}"#);
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["convert", "missing.mp4", "--outputs"])
        .arg(&spec)
        .arg("--keys-file")
        .arg(&keys)
        .arg("--output")
        .arg(root.path().join("result"))
        .arg("--json")
        .output()
        .unwrap();
    let value = check(&output, "convert", "key-file-invalid");
    assert_eq!(
        value["error"]["configuration"]["path"],
        json!(["outputs", "*", "key"])
    );
}
