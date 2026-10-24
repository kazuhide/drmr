//! Verification depth is shared by the CLI media commands and remains explicit in reports.
use std::process::{Command, Output};

fn invoke(args: &[&str]) -> Output {
    let root = drmr_core::native::private_tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .current_dir(root.path())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn media_help_explains_one_additional_decode_option() {
    for command in ["analyze", "plan", "convert", "verify"] {
        let output = invoke(&[command, "--help"]);
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("--decode"), "{command}: {help}");
        assert!(!help.contains("--ts-validation"));
    }
}

#[test]
fn decode_reaches_direct_saved_and_bundle_media_commands() {
    for mut args in [
        vec!["analyze", "missing.mp4", "--json"],
        vec![
            "plan",
            "missing.mp4",
            "--to",
            "clear",
            "--output",
            "result",
            "--save",
            "plan.json",
            "--json",
        ],
        vec![
            "plan",
            "missing.m3u8",
            "--outputs",
            "outputs.json",
            "--output",
            "result",
            "--save",
            "plan.json",
            "--json",
        ],
        vec![
            "convert",
            "missing.mp4",
            "--to",
            "clear",
            "--output",
            "result",
            "--json",
        ],
        vec!["convert", "--plan", "plan.json", "--json"],
        vec!["verify", "result.mp4", "--source", "missing.mp4", "--json"],
        vec!["verify", "--plan", "plan.json", "--json"],
    ] {
        args.push("--decode");
        let output = invoke(&args);
        assert!(!output.status.success());
        let value: serde_json::Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("{args:?}: {}", String::from_utf8_lossy(&output.stderr)));
        assert_eq!(value["command"], args[0]);
        assert_eq!(value["status"], "failed");
    }
}

#[test]
fn metadata_light_plan_and_manifest_only_refuse_decode_before_work() {
    for args in [
        vec!["analyze", "missing.mp4", "--metadata-only", "--decode"],
        vec!["plan", "missing.mp4", "--to", "clear", "--decode"],
        vec!["verify", "missing.m3u8", "--manifest-only", "--decode"],
    ] {
        let output = invoke(&args);
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(output.stdout, Vec::<u8>::new());
    }
}

#[test]
fn source_reports_distinguish_native_evidence_from_actual_complete_decoding() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("source.mp4");
    let original = include_bytes!("fixtures/bbb/clip.mp4");
    std::fs::write(&input, original).unwrap();
    for decode in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
        command
            .arg("analyze")
            .arg(&input)
            .args(["--json", "--measure"]);
        if decode {
            command.arg("--decode");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(value["schema_version"], 2);
        assert_eq!(value["input_admitted"], true);
        let proof = &value["verification"];
        assert_eq!(proof["schema_version"], 2);
        assert_eq!(proof["scope"], "source-admission");
        assert_eq!(
            proof["required_depth"],
            if decode { "decoded" } else { "native" }
        );
        assert_eq!(proof["passed"], true);
        assert_eq!(proof["checks"]["stream_metadata"], "pass");
        assert_eq!(proof["checks"]["packet_payloads_equal"], true);
        assert_eq!(proof["checks"]["packet_timing_equal"], true);
        assert!(proof.get("media_verified").is_none());
        assert!(proof["checks"].get("decoded_content_equal").is_none());
        assert_eq!(
            proof["decode_evidence"]["status"],
            if decode {
                "source-decoded"
            } else {
                "not-checked"
            }
        );
        if decode {
            assert!(
                value["measurements"]["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["calls"].as_u64().unwrap() > 0)
            );
        } else {
            assert_eq!(proof["decode_evidence"]["reason"], "not-requested");
            for tool in value["measurements"]["tools"].as_array().unwrap() {
                assert_eq!(tool["calls"], 0, "{}", tool["tool"]);
                assert_eq!(tool["unsuccessful_calls"], 0);
            }
            assert_eq!(value["measurements"]["unclassified_tool_calls"], 0);
            assert_eq!(value["measurements"]["invalid_tool_records"], 0);
        }
        let mut readable = Command::new(env!("CARGO_BIN_EXE_drmr"));
        readable
            .arg("analyze")
            .arg(&input)
            .args(["--progress", "off"]);
        if decode {
            readable.arg("--decode");
        }
        let readable = readable.output().unwrap();
        assert!(
            readable.status.success(),
            "{}",
            String::from_utf8_lossy(&readable.stderr)
        );
        let text = String::from_utf8(readable.stdout).unwrap();
        assert!(text.contains(if decode {
            "Complete source decoding: passed."
        } else {
            "Additional complete decoding: not requested."
        }));
        assert!(!text.contains("decoded content, and timestamps"));
        assert_eq!(std::fs::read(&input).unwrap(), original);
    }
}

fn json_at(root: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .current_dir(root)
        .args(args)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn assert_comparison_depth(proof: &serde_json::Value, decoded: bool) {
    assert_eq!(proof["schema_version"], 2);
    assert_eq!(proof["scope"], "comparison");
    assert_eq!(proof["passed"], true);
    assert_eq!(proof["checks"]["stream_metadata"], "pass");
    assert_eq!(proof["checks"]["packet_payloads_equal"], true);
    assert_eq!(proof["checks"]["packet_timing_equal"], true);
    assert_eq!(
        proof["required_depth"],
        if decoded { "decoded" } else { "native" }
    );
    assert_eq!(
        proof["decode_evidence"],
        if decoded {
            serde_json::json!({"status":"content-compared", "equal":true})
        } else {
            serde_json::json!({"status":"not-checked", "reason":"not-requested"})
        }
    );
}

#[test]
fn saved_single_and_named_output_minima_survive_runtime_defaults_and_can_be_strengthened() {
    for named in [false, true] {
        for decoded in [false, true] {
            let root = drmr_core::native::private_tempdir().unwrap();
            let original = include_bytes!("fixtures/bbb/clip.mp4");
            std::fs::write(root.path().join("source.mp4"), original).unwrap();
            std::fs::write(root.path().join("outputs.json"), br#"{"schema_version":1,"outputs":[{"id":"clear","format":"mp4","target":"clear","drms":[]}]}"#).unwrap();
            let mut plan = vec![
                "plan",
                "source.mp4",
                "--output",
                "result",
                "--save",
                "plan.json",
            ];
            if named {
                plan.extend(["--outputs", "outputs.json"]);
            } else {
                plan.extend(["--to", "clear", "--format", "mp4"]);
            }
            if decoded {
                plan.push("--decode");
            }
            let saved = json_at(root.path(), &plan);
            assert_eq!(saved["schema_version"], if named { 4 } else { 3 });
            assert_eq!(
                saved["required_depth"],
                if decoded { "decoded" } else { "native" }
            );
            let before = std::fs::read(root.path().join("plan.json")).unwrap();
            let converted = json_at(root.path(), &["convert", "--plan", "plan.json"]);
            let conversion_proof = if named {
                &converted["outputs"][0]["conversion"]["verification"]
            } else {
                &converted["verification"]
            };
            assert_comparison_depth(conversion_proof, decoded);
            let processing = if named {
                &converted["outputs"][0]["conversion"]["processing"]
            } else {
                &converted["processing"]
            };
            assert_eq!(processing["route"], "progressive-mp4");
            assert_eq!(
                processing["required_tools"],
                if decoded {
                    serde_json::json!(["ffmpeg", "ffprobe"])
                } else {
                    serde_json::json!([])
                }
            );
            for strengthen in [false, true] {
                let mut args = vec!["verify", "--plan", "plan.json"];
                if strengthen {
                    args.push("--decode");
                }
                let verified = json_at(root.path(), &args);
                assert_eq!(verified["source_unchanged"], true);
                let proof = if named {
                    &verified["outputs"][0]["verification"]
                } else {
                    &verified["verification"]
                };
                assert_comparison_depth(proof, decoded || strengthen);
                assert_eq!(
                    std::fs::read(root.path().join("plan.json")).unwrap(),
                    before
                );
                assert_eq!(
                    std::fs::read(root.path().join("source.mp4")).unwrap(),
                    original
                );
            }
        }
    }
}
