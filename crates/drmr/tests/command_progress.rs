//! Progress has its own safe stderr stream and never changes the result protocol.
use serde_json::Value;
use std::process::{Command, Output};

fn doctor(options: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["doctor", "--json"])
        .args(options)
        .output()
        .unwrap()
}

#[test]
fn structured_progress_is_separate_from_the_single_result() {
    let result = doctor(&["--progress", "json"]);
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert!(report["tools"].is_array());
    let lines = String::from_utf8(result.stderr).unwrap();
    let records: Vec<Value> = lines
        .lines()
        .map(|line| serde_json::from_str(line).expect("each progress line is JSON"))
        .collect();
    assert!(records.len() >= 2, "{lines}");
    let id = records[0]["run_id"].as_str().unwrap();
    assert_ne!(id, "");
    assert!(records.iter().all(|r| r["schema_version"] == 1
        && r["command"] == "doctor"
        && r["entry_point"] == "cli"
        && r["run_id"] == id
        && r["elapsed_ms"].is_u64()));
    assert!(records.iter().any(|r| r["activity"] == "tool-check"));
    assert_eq!(records.last().unwrap()["event"], "finished");
    for record in &records {
        for key in record.as_object().unwrap().keys() {
            assert!(
                [
                    "schema_version",
                    "command",
                    "entry_point",
                    "run_id",
                    "event",
                    "elapsed_ms",
                    "activity",
                    "task",
                    "scopes_started",
                    "scopes_closed",
                    "active_scopes",
                    "workers",
                    "tools_started",
                    "tools_closed",
                    "active_tools",
                    "active_tool_count",
                    "resources_started",
                    "resources_completed",
                    "bytes_received",
                    "work_units_completed",
                    "work_units_total",
                    "error_code"
                ]
                .contains(&key.as_str()),
                "unexpected progress field: {key}"
            );
        }
    }
}

#[test]
fn automatic_json_and_explicit_off_keep_progress_silent() {
    for options in [&[][..], &["--progress", "off"][..]] {
        let result = doctor(options);
        serde_json::from_slice::<Value>(&result.stdout).unwrap();
        assert!(result.stderr.is_empty(), "{:?}", result.stderr);
    }
}

#[test]
fn explicit_text_is_available_with_json_results() {
    let result = doctor(&["--progress", "text"]);
    serde_json::from_slice::<Value>(&result.stdout).unwrap();
    let text = String::from_utf8(result.stderr).unwrap();
    assert!(text.contains("Activity: tool-check"), "{text}");
    assert!(text.contains("Checking external media tools"));
    assert!(!text.contains('%') && !text.contains("ETA") && !text.contains('\u{1b}'));
}

#[test]
fn failed_commands_keep_structured_progress_secret_free() {
    let root = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args([
            "--progress",
            "json",
            "convert",
            "https://example.invalid/private-sentinel.m3u8?secret-sentinel",
            "--to",
            "clear",
            "--output",
        ])
        .arg(root.path().join("output-sentinel"))
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["error"]["code"], "download-directory-required");
    let text = String::from_utf8(result.stderr).unwrap();
    assert!(!text.contains("sentinel") && !text.contains("https://"));
    let rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(rows.iter().all(|row| row["command"] == "convert"));
    assert_eq!(rows.last().unwrap()["event"], "failed");
    assert_eq!(
        rows.last().unwrap()["error_code"],
        "download-directory-required"
    );
}
