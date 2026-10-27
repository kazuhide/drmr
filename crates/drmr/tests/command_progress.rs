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
                    "resources_total",
                    "bytes_received",
                    "analysis_task",
                    "analysis_state",
                    "analysis_completed",
                    "analysis_total",
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
    assert!(text.contains("Checking tools"), "{text}");
    assert!(text.contains("Finished doctor in"));
    assert!(!text.contains("scopes") && !text.contains("Activity:"));
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

#[test]
fn multiple_output_configuration_uses_one_command_observer() {
    let root = tempfile::tempdir().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args([
            "convert",
            "source-sentinel.mp4",
            "--progress",
            "json",
            "--json",
            "--outputs",
        ])
        .arg(root.path().join("missing-sentinel.json"))
        .arg("--output")
        .arg(root.path().join("result"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    let result_report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result_report["status"], "failed");
    let text = String::from_utf8(result.stderr).unwrap();
    let rows: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let id = rows[0]["run_id"].as_str().unwrap();
    assert!(
        rows.iter().all(|r| r["run_id"] == id),
        "one observer must own the complete operation"
    );
    assert_eq!(rows.last().unwrap()["event"], "failed");
    assert!(!text.contains("sentinel"));
}

#[test]
fn doctor_reports_a_closed_result_stream_without_panicking_into_json_logs() {
    use std::process::Stdio;
    for options in [&["--json"][..], &[][..]] {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
            .args(["doctor", "--progress", "json"])
            .args(options)
            .stdout(Stdio::from(writer))
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        let text = String::from_utf8(result.stderr).unwrap();
        let rows: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).expect("a write failure remains JSON Lines"))
            .collect();
        assert_eq!(rows.last().unwrap()["event"], "failed");
        assert_eq!(
            rows.last().unwrap()["error_code"],
            "result-output-unavailable"
        );
        assert!(!text.contains("panicked"));
        assert!(rows.iter().all(|row| row["event"] != "finished"), "{text}");
    }
}

#[cfg(unix)]
#[test]
fn console_reports_actual_readiness_and_heartbeat_then_stops_cleanly() {
    use std::io::{BufRead as _, BufReader};
    use std::process::Stdio;
    use std::time::{Duration, Instant};
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = tempfile::tempdir().unwrap();
    let input = root.path().join("input");
    std::fs::create_dir(&input).unwrap();
    let mut child = OwnedChild(
        Command::new(env!("CARGO_BIN_EXE_drmr"))
            .args(["serve", "--progress", "json"])
            .env("DRMR_INPUT_ROOT", &input)
            .env("DRMR_OUTPUT_ROOT", root.path().join("output"))
            .env("DRMR_BIND_HOST", "127.0.0.1")
            .env("DRMR_BIND_PORT", "0")
            .env_remove("DRMR_COMMON_CONFIG_ROOT")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let stderr = child.0.stderr.take().unwrap();
    let (sent, received) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            if sent.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let mut ready = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let line = received
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        let event: Value = serde_json::from_str(&line).expect("console progress is JSON Lines");
        assert_eq!(event["command"], "serve");
        if event["activity"] == "serving" {
            if event["event"] == "activity" {
                ready = true;
            }
            if event["event"] == "heartbeat" {
                assert!(ready);
                break;
            }
        }
    }
    assert!(ready);
    let pid = rustix::process::Pid::from_raw(i32::try_from(child.0.id()).unwrap()).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "console must confirm shutdown");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    // Drain the sender before joining so neither bounded observation nor teardown blocks.
    let remaining: Vec<_> = received.iter().collect();
    reader.join().unwrap();
    let last: Value = serde_json::from_str(remaining.last().unwrap()).unwrap();
    assert_eq!(last["event"], "finished");
}
