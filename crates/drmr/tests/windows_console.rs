//! Real Windows console events must cancel the CLI and reclaim detached media children.
#![cfg(windows)]

use drmr_core::native::ManagedChild;
use drmr_test_support::{ToolSpec, install};
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn console_cancellation(command: &str, event: &str) {
    let root = tempfile::tempdir().unwrap();
    let tools = root.path().join("tools");
    let control = root.path().join("control");
    std::fs::create_dir(&control).unwrap();
    let specification = ToolSpec::default()
        .with("pid_file", control.join("media.pid").to_str().unwrap())
        .with(
            "descendant_pid_file",
            control.join("descendant.pid").to_str().unwrap(),
        )
        .with(
            "descendant_lock_file",
            control.join("descendant.lock").to_str().unwrap(),
        )
        .with("descendant_detached_console", "1")
        .with(
            "release_file",
            control.join("never-release").to_str().unwrap(),
        );
    for program in ["ffprobe", "ffmpeg"] {
        install(&tools.join(program), &specification).unwrap();
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let output = root.path().join("controller.stdout");
    let errors = root.path().join("controller.stderr");
    let mut invocation = Command::new("python");
    invocation
        .arg(manifest.join("tests/fixtures/windows_console.py"))
        .arg("--drmr")
        .arg(env!("CARGO_BIN_EXE_drmr"))
        .arg("--clip")
        .arg(manifest.join("tests/fixtures/bbb/clip.mp4"))
        .arg("--case")
        .arg(root.path())
        .arg("--runner-pid")
        .arg(std::process::id().to_string())
        .args(["--command", command, "--event", event])
        .stdin(Stdio::null())
        .stdout(File::create(&output).unwrap())
        .stderr(File::create(&errors).unwrap());
    // The controller allocates and verifies its console before starting the CLI.
    // ManagedChild retains job ownership independently of console membership.
    let mut controller = ManagedChild::spawn(invocation, true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(40);
    let observed = loop {
        match controller.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(_) => break Err("console controller polling failed"),
            Ok(None) if Instant::now() >= deadline => break Err("console controller timed out"),
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
        }
    };
    // This owns the whole test job, including failure paths. Never delete the
    // fixture workspace or report a passed case before this result is checked.
    let cleanup = controller.finish();
    assert!(cleanup.is_ok(), "console test cleanup is unconfirmed");
    let status = observed.expect("console controller must finish within its deadline");
    let report = std::fs::read_to_string(output).unwrap();
    assert!(
        report.len() < 16 * 1024,
        "unexpected controller output size"
    );
    assert!(status.success(), "console case failed: {report}");
    assert_eq!(std::fs::read(errors).unwrap(), b"");
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert_eq!(report["passed"], true);
    assert_eq!(report["command"], command);
    assert_eq!(report["event"], event);
    assert_eq!(report["exit_code"], 130);
    assert_eq!(report["immediate_processes"], 3);
    assert_eq!(report["process_states"], serde_json::json!([0, 0, 0]));
    assert_eq!(report["detached_descendant"], true);
    println!("{report}");
}

#[test]
fn ctrl_c_cancels_convert_and_reclaims_detached_media() {
    console_cancellation("convert", "ctrl-c");
}

#[test]
fn ctrl_break_cancels_convert_and_reclaims_detached_media() {
    console_cancellation("convert", "ctrl-break");
}

#[test]
fn ctrl_c_cancels_verify_and_reclaims_detached_media() {
    console_cancellation("verify", "ctrl-c");
}

#[test]
fn ctrl_break_cancels_verify_and_reclaims_detached_media() {
    console_cancellation("verify", "ctrl-break");
}
