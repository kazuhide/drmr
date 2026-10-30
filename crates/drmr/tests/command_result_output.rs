//! Closed result streams retain the structured failure protocol.
// Keep this binary to one test: concurrent spawns can inherit a pipe reader
// on platforms that set close-on-exec after creating the pipe.
use serde_json::Value;
use std::process::Command;

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
