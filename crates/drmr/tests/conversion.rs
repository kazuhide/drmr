//! End-to-end generic command contracts and distinct operator key domains.
use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(args)
        .output()
        .unwrap()
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn generic_commands_explain_safe_key_inputs_and_required_reference() {
    for command in ["convert", "verify"] {
        let result = run(&[command, "--help"]);
        assert!(result.status.success(), "{}", text(&result.stderr));
        let help = text(&result.stdout);
        assert!(
            help.contains("--keys-file") && help.contains("--json"),
            "{help}"
        );
        assert!(!help.contains("--key <") && !help.contains("--iv <"));
    }
    let result = run(&["verify", "output.mp4"]);
    assert_eq!(result.status.code(), Some(2));
    assert!(text(&result.stderr).contains("--source"));
}

fn private_config(path: &Path, body: &str) {
    use std::io::Write as _;
    drmr_core::native::create_private_dir_all(path.parent().unwrap()).unwrap();
    drmr_core::native::create_private_or_truncate(path)
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
}

#[test]
fn real_convert_and_verify_keep_json_machine_readable_and_keys_private() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.mp4");
    let output = dir.path().join("output.mp4");
    let keys = dir.path().join("private/keys.json");
    let verify_keys = dir.path().join("private/verify-keys.json");
    assert!(
        Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=s=64x64:r=24:d=1",
                "-an",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+frag_keyframe+empty_moov"
            ])
            .arg(&source)
            .status()
            .unwrap()
            .success()
    );
    private_config(
        &keys,
        r#"{"schema_version":1,"output":{"kid":"00000000000000000000000000000002","key":"22222222222222222222222222222222"}}"#,
    );
    let result = run(&[
        "convert",
        source.to_str().unwrap(),
        "--to",
        "cbcs",
        "--output",
        output.to_str().unwrap(),
        "--keys-file",
        keys.to_str().unwrap(),
        "--json",
    ]);
    assert!(result.status.success(), "{}", text(&result.stderr));
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["output"]["protection"], "cbcs");
    assert_eq!(report["verification"]["passed"], true);
    assert!(!text(&result.stdout).contains("222222"));
    assert!(!text(&result.stderr).contains("222222"));
    private_config(
        &verify_keys,
        r#"{"schema_version":1,"output":{"kid":"00000000000000000000000000000002","key":"22222222222222222222222222222222"}}"#,
    );
    let result = run(&[
        "verify",
        output.to_str().unwrap(),
        "--source",
        source.to_str().unwrap(),
        "--keys-file",
        verify_keys.to_str().unwrap(),
        "--json",
    ]);
    assert!(result.status.success(), "{}", text(&result.stderr));
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(report["checks"]["packet_timing_equal"], true);
    assert!(!text(&result.stdout).contains("222222"));
}

#[test]
fn invalid_key_configuration_fails_without_echoing_secrets_or_publishing_output() {
    let dir = tempfile::tempdir().unwrap();
    let keys = dir.path().join("private/credential-sentinel.json");
    let output = dir.path().join("output.mp4");
    private_config(
        &keys,
        r#"{"schema_version":1,"output":{"kid":"x","key":"secret-sentinel"}}"#,
    );
    let result = run(&[
        "convert",
        "source.mp4",
        "--to",
        "cbcs",
        "--output",
        output.to_str().unwrap(),
        "--keys-file",
        keys.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(result.status.code(), Some(2));
    assert!(!text(&result.stderr).contains("sentinel"));
    assert!(!output.exists());
}

#[test]
fn generic_worker_limits_are_optional_positive_upper_bounds() {
    for command in ["convert", "verify"] {
        let help = run(&[command, "--help"]);
        assert!(text(&help.stdout).contains("--max-workers"));
    }
    for limit in ["0", "65"] {
        let result = run(&[
            "convert",
            "source.mp4",
            "--to",
            "clear",
            "--output",
            "new.mp4",
            "--max-workers",
            limit,
        ]);
        assert_eq!(result.status.code(), Some(2));
        assert!(text(&result.stderr).contains("invalid value"));
    }
}
