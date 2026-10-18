//! A generic command must reclaim its independent media process groups before exiting.
#![cfg(unix)]

use drmr_test_support::{ToolSpec, install};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Cleanup {
    child: Child,
    descendant: PathBuf,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Ok(pid) = std::fs::read_to_string(&self.descendant)
            && let Ok(pid) = pid.trim().parse::<i32>()
            && let Some(pid) = rustix::process::Pid::from_raw(pid)
        {
            if let Ok(group) = rustix::process::getpgid(Some(pid))
                && group != rustix::process::getpgrp()
            {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            }
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}

fn cancelled_command(signal: rustix::process::Signal, command: &str) {
    let root = tempfile::tempdir().unwrap();
    let tools = root.path().join("tools");
    std::fs::create_dir(&tools).unwrap();
    let descendant = root.path().join("descendant.pid");
    let lock = root.path().join("descendant.lock");
    install(
        &tools.join("ffprobe"),
        &ToolSpec::default()
            .with("descendant_pid_file", descendant.to_str().unwrap())
            .with("descendant_lock_file", lock.to_str().unwrap())
            .with(
                "release_file",
                root.path().join("never-release").to_str().unwrap(),
            ),
    )
    .unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let output = root.path().join("result");
    let mut invocation = Command::new(env!("CARGO_BIN_EXE_drmr"));
    invocation.arg(command);
    if command == "convert" {
        invocation
            .arg(&clip)
            .args(["--to", "clear", "--format", "dash", "--output"])
            .arg(&output);
    } else {
        invocation.arg(&clip).arg("--source").arg(&clip);
    }
    invocation
        .args(["--json", "--max-workers", "1"])
        .env("PATH", &tools)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut cleanup = Cleanup {
        child: invocation.spawn().unwrap(),
        descendant,
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !std::fs::read_to_string(&cleanup.descendant)
        .is_ok_and(|text| text.ends_with('\n') && text.trim().parse::<i32>().is_ok())
    {
        assert!(
            cleanup.child.try_wait().unwrap().is_none(),
            "command exited before the media fixture started"
        );
        assert!(
            Instant::now() < deadline,
            "media fixture did not become ready"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let held = std::fs::File::open(&lock).unwrap();
    assert!(
        held.try_lock().is_err(),
        "descendant must actually own the lock before cancellation"
    );
    let pid = std::fs::read_to_string(&cleanup.descendant)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let pid = rustix::process::Pid::from_raw(pid).unwrap();
    assert_ne!(
        rustix::process::getpgid(Some(pid)).unwrap(),
        rustix::process::getpgid(Some(rustix::process::Pid::from_child(&cleanup.child))).unwrap(),
        "the media process group must be independent of the CLI's group"
    );
    rustix::process::kill_process(rustix::process::Pid::from_child(&cleanup.child), signal)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Some(status) = cleanup.child.try_wait().unwrap() {
            break status;
        }
        assert!(Instant::now() < deadline, "cancellation did not finish");
        std::thread::sleep(Duration::from_millis(5));
    };
    // This is an immediate post-return check; no eventual cleanup polling.
    held.try_lock()
        .expect("CLI returned while its media descendant still held the lock");
    assert_eq!(status.code(), Some(130));
    let mut json = String::new();
    std::io::Read::read_to_string(&mut cleanup.child.stdout.take().unwrap(), &mut json).unwrap();
    let report: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["command"], command);
    assert_eq!(report["status"], "cancelled");
    assert_eq!(report["error"]["code"], "cancelled");
    assert!(report["error"]["last_activity"].is_string());
    assert!(!json.contains(&root.path().to_string_lossy().to_string()));
    assert!(
        !output.exists(),
        "cancelled conversion must not publish output"
    );
}

#[test]
fn sigterm_cancels_convert_and_reclaims_media_descendants() {
    cancelled_command(rustix::process::Signal::TERM, "convert");
}

#[test]
fn sigint_cancels_verify_and_reclaims_media_descendants() {
    cancelled_command(rustix::process::Signal::INT, "verify");
}
