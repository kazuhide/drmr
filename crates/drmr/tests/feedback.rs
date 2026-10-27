//! Generic CLI progress and failure results contain only fixed classifications and numeric observations.
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn clip() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4")
}
fn invoke(command: &str, source: &Path, output: &Path, extra: &[&str]) -> Output {
    let mut process = Command::new(env!("CARGO_BIN_EXE_drmr"));
    process.arg(command);
    if command == "convert" {
        process
            .arg(source)
            .args(["--to", "clear", "--output"])
            .arg(output);
    } else {
        process.arg(output).arg("--source").arg(source);
    }
    process.args(extra).output().unwrap()
}
fn failure(result: &Output, command: &str, code: &str, exit: i32) -> Value {
    failure_with_schema(result, command, code, exit, 1)
}
fn failure_with_schema(
    result: &Output,
    command: &str,
    code: &str,
    exit: i32,
    schema: u32,
) -> Value {
    assert_eq!(result.status.code(), Some(exit));
    let value: Value = serde_json::from_slice(&result.stdout)
        .expect("a valid --json command emits one structured failure object");
    assert_eq!(value["schema_version"], schema);
    assert_eq!(value["command"], command);
    assert_eq!(value["status"], "failed");
    assert_eq!(value["error"]["code"], code);
    assert!(
        value["error"]["hint"]
            .as_str()
            .is_some_and(|text| !text.is_empty())
    );
    assert!(value["error"].get("last_activity").is_some());
    value
}
#[test]
fn configuration_and_input_errors_are_json_without_echoing_untrusted_values() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("destination-sentinel.mp4");
    let missing = root.path().join("source-sentinel.mp4");
    for command in ["convert", "verify"] {
        let result = invoke(
            command,
            &missing,
            &output,
            &["--json", "--keys-file", "missing-secret-sentinel.json"],
        );
        failure(&result, command, "key-file-invalid", 2);
        for bytes in [&result.stdout, &result.stderr] {
            assert!(!String::from_utf8_lossy(bytes).contains("sentinel"));
        }
    }
    let result = invoke("convert", &missing, &output, &["--json"]);
    failure(&result, "convert", "invalid-input-media", 1);
    let result = invoke(
        "convert",
        Path::new("https://example.invalid/master.m3u8?credential-sentinel"),
        &output,
        &["--json"],
    );
    failure(&result, "convert", "download-directory-required", 2);
    assert!(!String::from_utf8_lossy(&result.stdout).contains("sentinel"));
    assert!(!output.exists());
}
#[test]
fn an_unavailable_real_tool_has_structured_failure_and_safe_last_activity() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mp4");
    let original = std::fs::read(clip()).unwrap();
    let copied = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-copyts", "-i"])
        .arg(clip())
        .args(["-map", "0:v:0", "-an", "-c", "copy"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        copied.status.success(),
        "{}",
        String::from_utf8_lossy(&copied.stderr)
    );
    // Keep the whole original video; finite AAC admission is a separate contract.
    let evidence = |path: &Path| {
        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_packets",
                "-show_streams",
                "-show_data_hash",
                "sha256",
                "-show_entries",
                "stream=codec_name,time_base:packet=pts,dts,duration,data_hash",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .unwrap();
        assert!(
            probe.status.success(),
            "{}",
            String::from_utf8_lossy(&probe.stderr)
        );
        serde_json::from_slice::<Value>(&probe.stdout).unwrap()
    };
    let expected = evidence(&clip());
    assert_eq!(expected["packets"].as_array().unwrap().len(), 240);
    assert_eq!(evidence(&source), expected);
    let source_before = std::fs::read(&source).unwrap();
    let output = root.path().join("output.mp4");
    let missing = root.path().join("tool-sentinel");
    let result = invoke(
        "convert",
        &source,
        &output,
        &[
            "--json",
            "--format",
            "dash",
            "--packager-bin",
            missing.to_str().unwrap(),
        ],
    );
    let report = failure(&result, "convert", "media-tool-unavailable-or-timeout", 1);
    assert_eq!(report["error"]["last_activity"], "normalization");
    for bytes in [&result.stdout, &result.stderr] {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("sentinel"));
        assert!(!text.contains(&root.path().display().to_string()));
    }
    assert_eq!(std::fs::read(clip()).unwrap(), original);
    assert_eq!(std::fs::read(&source).unwrap(), source_before);
    assert!(!output.exists());
}
#[test]
fn redirected_text_reports_steps_and_elapsed_without_internal_counters_or_fake_percentages() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("output.mp4");
    let result = invoke("convert", &clip(), &output, &[]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let progress = String::from_utf8(result.stderr).unwrap();
    assert!(progress.contains("Converting media"), "{progress}");
    assert!(
        progress.matches("Converting media").count() <= 2,
        "{progress}"
    );
    assert!(
        !progress.contains("packager") && !progress.contains("ffprobe"),
        "{progress}"
    );
    assert!(
        progress.contains("elapsed") && progress.contains("Finished convert in"),
        "{progress}"
    );
    assert!(!progress.contains("scopes") && !progress.contains("Activity:"));
    assert!(!progress.contains('\u{1b}') && !progress.contains('%') && !progress.contains("ETA"));
    assert!(
        !progress.contains("clip.mp4") && !progress.contains(&root.path().display().to_string())
    );
    // Internal scope observations remain available in the explicit JSON stream.
    let structured = invoke(
        "verify",
        &clip(),
        &output,
        &["--json", "--progress", "json"],
    );
    assert!(structured.status.success());
    let records: Vec<Value> = String::from_utf8(structured.stderr)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let last = records.last().unwrap();
    assert_eq!(last["event"], "finished");
    assert!(last["scopes_started"].as_u64().unwrap() > 0);
    assert_eq!(
        last["scopes_started"], last["scopes_closed"],
        "all observed scopes are closed before the result"
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("Verified stream metadata"));
}

#[test]
fn observed_capacity_refusal_is_structured_without_launching_packaging() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("video.mp4");
    let output = root.path().join("output.mp4");
    assert!(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(clip())
            .args(["-map", "0:v:0", "-an", "-c", "copy"])
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    let profile = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_packets",
            "-show_streams",
            "-of",
            "json",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(profile.status.success());
    let mut profile: Value = serde_json::from_slice(&profile.stdout).unwrap();
    profile["streams"][0]["width"] = u32::MAX.into();
    profile["streams"][0]["height"] = u32::MAX.into();
    let tools = root.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    drmr_test_support::install(
        &tools.join("ffprobe"),
        &drmr_test_support::ToolSpec {
            stdout: serde_json::to_vec(&profile).unwrap(),
            ..Default::default()
        },
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("convert")
        .arg("--decode")
        .arg(&source)
        .args(["--to", "clear", "--format", "mp4", "--json", "--output"])
        .arg(&output)
        .env("PATH", tools)
        .output()
        .unwrap();
    failure(&result, "convert", "resource-budget", 1);
    assert!(!output.exists());
}

#[test]
fn longer_public_source_preserves_full_proof_and_progress_without_changing_success_json() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("longer.mp4");
    let output = root.path().join("result.mp4");
    assert!(
        Command::new("ffmpeg")
            .args(["-v", "error", "-stream_loop", "3", "-i"])
            .arg(clip())
            .args(["-map", "0:v:0", "-an", "-c", "copy"])
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    let packets = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-count_packets",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "csv=p=0",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(packets.status.success());
    assert_eq!(String::from_utf8(packets.stdout).unwrap().trim(), "960");
    let before = std::fs::read(&source).unwrap();
    let converted = invoke("convert", &source, &output, &["--decode", "--json"]);
    assert!(
        converted.status.success(),
        "{}",
        String::from_utf8_lossy(&converted.stderr)
    );
    let converted: Value = serde_json::from_slice(&converted.stdout).unwrap();
    assert!(converted.get("status").is_none() && converted.get("error").is_none());
    assert_eq!(converted["schema_version"], 2);
    assert_eq!(converted["verification"]["required_depth"], "decoded");
    assert_eq!(converted["verification"]["scope"], "comparison");
    assert_eq!(
        converted["verification"]["checks"],
        serde_json::json!({"stream_metadata":"pass", "packet_payloads_equal":true, "packet_timing_equal":true})
    );
    assert_eq!(
        converted["verification"]["decode_evidence"],
        serde_json::json!({"status":"content-compared","equal":true})
    );
    let verified = invoke("verify", &source, &output, &["--decode"]);
    assert!(
        verified.status.success(),
        "{}",
        String::from_utf8_lossy(&verified.stderr)
    );
    let progress = String::from_utf8(verified.stderr).unwrap();
    assert!(progress.contains("Verifying media") && progress.contains("Finished verify in"));
    assert_eq!(std::fs::read(&source).unwrap(), before);
}

#[test]
fn a_verification_mismatch_retains_the_check_report_and_adds_safe_failure_fields() {
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source.mp4");
    let altered = root.path().join("altered.mp4");
    assert!(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(clip())
            .args(["-map", "0:v:0", "-an", "-c", "copy"])
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    assert!(
        Command::new("ffmpeg")
            .args(["-v", "error", "-copyts", "-i"])
            .arg(&source)
            .args([
                "-map", "0:v:0", "-an", "-vf", "negate", "-c:v", "libx264", "-pix_fmt", "yuv420p"
            ])
            .arg(&altered)
            .status()
            .unwrap()
            .success()
    );
    let result = invoke("verify", &source, &altered, &["--decode", "--json"]);
    let report = failure_with_schema(&result, "verify", "verification-mismatch", 1, 2);
    assert_eq!(report["passed"], false);
    assert_eq!(report["required_depth"], "decoded");
    assert_eq!(report["scope"], "comparison");
    assert_eq!(report["checks"]["packet_payloads_equal"], false);
    assert_eq!(report["license_playback_verified"], false);
    assert!(report.get("media_verified").is_none());
    assert!(report["checks"].get("decoded_content_equal").is_none());
    assert!(report["checks"].is_object());
    assert_eq!(
        report["decode_evidence"],
        serde_json::json!({"status":"content-compared","equal":false})
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("verify: verification-mismatch"));
}

#[test]
fn missing_real_source_keys_have_fixed_json_and_leave_source_and_destination_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let source = clip().parent().unwrap().join("cenc-iv8.mp4");
    let original = std::fs::read(&source).unwrap();
    for command in ["convert", "verify"] {
        let destination = root.path().join(format!("{command}.mp4"));
        let result = invoke(command, &source, &destination, &["--json"]);
        failure(&result, command, "input-key-required", 2);
        assert!(!destination.exists());
        assert_eq!(std::fs::read(&source).unwrap(), original);
        for bytes in [&result.stdout, &result.stderr] {
            let text = String::from_utf8_lossy(bytes);
            for private in [
                "101112131415161718191a1b1c1d1e1f",
                "202122232425262728292a2b2c2d2e2f",
            ] {
                assert!(!text.contains(private));
            }
        }
    }
}

#[test]
fn a_closed_conversion_result_stream_never_reports_finished_first() {
    use std::process::Stdio;
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("result.mp4");
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("convert")
        .arg(clip())
        .args(["--to", "clear", "--format", "mp4", "--output"])
        .arg(&output)
        .args(["--progress", "json", "--json"])
        .stdout(Stdio::from(writer))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(1));
    let text = String::from_utf8(result.stderr).unwrap();
    let records: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(
        output.is_file(),
        "the media completed before the result writer failed: {text}"
    );
    assert_eq!(
        records.last().unwrap()["error_code"],
        "result-output-unavailable"
    );
    assert!(
        records.iter().all(|record| record["event"] != "finished"),
        "{text}"
    );
}

#[test]
fn saved_plan_result_streams_never_report_finished_before_a_write_failure() {
    use std::ffi::OsString;
    use std::process::Stdio;
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("result.mp4");
    let plan = root.path().join("conversion.plan.json");
    let cases: Vec<Vec<OsString>> = vec![
        vec!["analyze".into(), clip().as_os_str().into()],
        vec![
            "plan".into(),
            clip().as_os_str().into(),
            "--to".into(),
            "clear".into(),
            "--format".into(),
            "mp4".into(),
            "--output".into(),
            output.as_os_str().into(),
            "--save".into(),
            plan.as_os_str().into(),
        ],
        vec!["convert".into(), "--plan".into(), plan.as_os_str().into()],
        vec!["verify".into(), "--plan".into(), plan.as_os_str().into()],
        vec![
            "verify".into(),
            output.as_os_str().into(),
            "--source".into(),
            clip().as_os_str().into(),
        ],
    ];
    let mut premature = Vec::new();
    for args in cases {
        let (reader, writer) = std::io::pipe().unwrap();
        drop(reader);
        let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
            .args(&args)
            .args(["--progress", "json", "--json"])
            .stdout(Stdio::from(writer))
            .output()
            .unwrap();
        assert_eq!(result.status.code(), Some(1));
        let text = String::from_utf8(result.stderr).unwrap();
        let records: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            records.last().unwrap()["error_code"],
            "result-output-unavailable",
            "{text}"
        );
        if records.iter().any(|record| record["event"] == "finished") {
            premature.push(args[0].to_string_lossy().to_string());
        }
    }
    assert!(plan.is_file() && output.is_file());
    assert!(
        premature.is_empty(),
        "premature success before result write: {premature:?}"
    );
}
