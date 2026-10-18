//! Discovery never executes tools; explicit probes use bounded native subprocess fixtures.

use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

fn doctor(path: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["doctor"])
        .args(args)
        .env_clear()
        .env("PATH", path)
        .env("DRM_KEY", "secret-must-not-appear")
        .output()
        .unwrap()
}

fn fixture_tool(directory: &Path, name: &str, executable: bool) {
    let path = directory.join(if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    });
    // Deliberately invalid executable content: discovery must not try running it.
    std::fs::write(&path, "this is not an executable program").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = executable;
}

#[test]
fn missing_dependencies_report_failure_as_json_without_environment_values() {
    let directory = tempfile::tempdir().unwrap();
    let output = doctor(directory.path(), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stderr, [] as [u8; 0]);
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ready"], false);
    let tools = report["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 4);
    for tool in tools {
        assert_eq!(tool["available"], false);
        assert_eq!(tool["required"], true);
    }
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("secret-must-not-appear"));
    assert!(!text.contains(directory.path().to_str().unwrap()));
}

#[test]
fn all_executable_dependencies_report_success_without_running_them() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["ffmpeg", "ffprobe", "mp4dump", "packager"] {
        fixture_tool(directory.path(), name, true);
    }
    let output = doctor(directory.path(), &["--json"]);
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ready"], true);
    for tool in report["tools"].as_array().unwrap() {
        assert_eq!(tool["available"], true);
    }
    assert_eq!(output.stderr, [] as [u8; 0]);
}

#[test]
fn selected_route_does_not_require_unrelated_tools() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["ffmpeg", "ffprobe"] {
        fixture_tool(directory.path(), name, true);
    }
    let output = doctor(directory.path(), &["--route", "progressive-mp4", "--json"]);
    assert_eq!(output.status.code(), Some(0));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ready"], true);
    assert_eq!(report["mode"], "discovery");
    assert_eq!(report["tools"][2]["required"], false);
    assert_eq!(report["tools"][3]["required"], false);
}

fn probing_tool(directory: &Path, name: &str, version: &str, options: &str) {
    drmr_test_support::install(
        &directory.join(name),
        &drmr_test_support::ToolSpec {
            stdout: format!(
                "{name} version {version}\n{options}\nuntrusted-secret-must-not-appear\n"
            )
            .into_bytes(),
            ..Default::default()
        },
    )
    .unwrap();
}

fn probing_pair(directory: &Path, version: &str, ffmpeg_options: &str) {
    probing_tool(directory, "ffmpeg", version, ffmpeg_options);
    probing_tool(
        directory,
        "ffprobe",
        "9.0.1",
        "-show_packets\n-show_entries\n-show_data_hash",
    );
}

#[test]
fn explicit_probe_reports_resolved_paths_versions_and_required_features() {
    let directory = tempfile::tempdir().unwrap();
    probing_pair(
        directory.path(),
        "9.0.2",
        "-encryption_scheme\n-decryption_key",
    );
    let output = doctor(
        directory.path(),
        &["--probe", "--route", "progressive-mp4", "--json"],
    );
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["mode"], "probe");
    assert_eq!(report["media_compatibility_checked"], false);
    assert_eq!(report["tools"][0]["version"], "9.0.2");
    assert_eq!(report["tools"][0]["version_status"], "supported");
    assert_eq!(report["tools"][0]["feature_status"], "available");
    let resolved = drmr_test_support::program_path(&directory.path().join("ffmpeg"))
        .canonicalize()
        .unwrap();
    assert_eq!(report["tools"][0]["path"], resolved.to_str().unwrap());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!all.contains("untrusted-secret"));
    assert!(!all.contains("secret-must-not-appear"));
}

#[test]
fn an_old_version_and_missing_options_are_separate_failures() {
    for (version, options, version_status, feature_status) in [
        (
            "8.0.0",
            "-encryption_scheme\n-decryption_key",
            "below-minimum",
            "available",
        ),
        ("99.0.0", "-encryption_scheme", "supported", "missing"),
        (
            "malformed",
            "-encryption_scheme\n-decryption_key",
            "unrecognized",
            "not-checked",
        ),
    ] {
        let directory = tempfile::tempdir().unwrap();
        probing_pair(directory.path(), version, options);
        let output = doctor(
            directory.path(),
            &["--probe", "--route", "progressive-mp4", "--json"],
        );
        assert_eq!(output.status.code(), Some(1));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["tools"][0]["version_status"], version_status);
        assert_eq!(report["tools"][0]["feature_status"], feature_status);
    }
}

#[test]
fn probes_bound_hanging_and_oversized_version_output() {
    for oversized in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let mut spec = drmr_test_support::ToolSpec::default();
        if oversized {
            spec.values
                .insert("stdout_repeat".into(), (2 * 1024 * 1024).to_string());
        } else {
            spec.sleep_ms = 8000;
        }
        drmr_test_support::install(&directory.path().join("ffmpeg"), &spec).unwrap();
        probing_tool(
            directory.path(),
            "ffprobe",
            "9.0.1",
            "-show_packets\n-show_entries\n-show_data_hash",
        );
        let started = std::time::Instant::now();
        let output = doctor(
            directory.path(),
            &["--probe", "--route", "progressive-mp4", "--json"],
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(7));
        assert_eq!(output.status.code(), Some(1));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["tools"][0]["failure_code"],
            if oversized {
                "probe-output-limit"
            } else {
                "probe-timeout"
            }
        );
    }
}

// Linux permits non-UTF-8 names; macOS APFS rejects this fixture at mkdir.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_tool_paths_are_reported_without_panics_or_ambiguous_execution() {
    use std::os::unix::ffi::OsStringExt;
    let root = tempfile::tempdir().unwrap();
    let directory = root
        .path()
        .join(std::ffi::OsString::from_vec(b"tools-\xff".to_vec()));
    std::fs::create_dir(&directory).unwrap();
    fixture_tool(&directory, "ffmpeg", true);
    let output = doctor(&directory, &["--probe", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["tools"][0]["available"], true);
    assert_eq!(report["tools"][0]["path"], Value::Null);
    assert_eq!(report["tools"][0]["path_status"], "not-utf8");
    assert_eq!(report["tools"][0]["failure_code"], "probe-path-not-utf8");
    assert!(output.stderr.is_empty());
}

#[test]
fn default_report_explains_next_steps_for_missing_dependencies() {
    let directory = tempfile::tempdir().unwrap();
    let output = doctor(directory.path(), &[]);
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    for name in ["ffmpeg", "ffprobe", "mp4dump", "packager"] {
        assert!(text.contains(name));
    }
    assert!(text.contains("PATH"));
    assert!(text.contains("Bento4"));
    assert!(text.contains("Shaka Packager"));
    assert!(!text.contains("secret-must-not-appear"));
}

#[cfg(unix)]
#[test]
fn directories_and_non_executable_files_do_not_count_as_available() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("ffmpeg")).unwrap();
    fixture_tool(directory.path(), "ffprobe", false);
    fixture_tool(directory.path(), "mp4dump", true);
    fixture_tool(directory.path(), "packager", true);
    let output = doctor(directory.path(), &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let tools = report["tools"].as_array().unwrap();
    assert_eq!(tools[0]["available"], false);
    assert_eq!(tools[1]["available"], false);
    assert_eq!(tools[2]["available"], true);
}

#[test]
fn root_help_shows_getting_started_examples_and_command_help_is_available() {
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["--help"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for example in [
        "drmr doctor",
        "drmr inspect",
        "drmr plan media.mp4 --to cbcs",
        "drmr convert",
        "drmr verify",
        "--download-dir",
        "drmr plan --mpd",
        "drmr run --help",
    ] {
        assert!(text.contains(example), "missing {example}: {text}");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["help", "doctor"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--json"));
}
