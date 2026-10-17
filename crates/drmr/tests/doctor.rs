//! Dependency discovery must be deterministic and must never execute discovered tools.

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
    for example in ["drmr doctor", "drmr plan --mpd", "drmr run --help"] {
        assert!(text.contains(example), "missing {example}: {text}");
    }
    let output = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["help", "doctor"])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("--json"));
}
