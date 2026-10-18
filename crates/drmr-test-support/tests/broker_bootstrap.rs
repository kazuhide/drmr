//! A direct native fixture must acknowledge the private CLI handoff before dispatch.
#![cfg(unix)]
use drmr_test_support::{ToolSpec, install};
use std::{
    fs,
    io::{Read as _, Write as _},
    os::{fd::OwnedFd, unix::net::UnixStream},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn completed(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "fixture handoff did not finish within its bound"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn installed(root: &std::path::Path) -> std::path::PathBuf {
    install(
        &root.join("cli"),
        &ToolSpec {
            stdout: b"fixed-response".to_vec(),
            ..Default::default()
        }
        .with("argv_file", root.join("argv").to_str().unwrap()),
    )
    .unwrap()
}
fn launch(
    program: &std::path::Path,
    socket: UnixStream,
    prefix: bool,
    marker: Option<&str>,
) -> Process {
    let descriptor: OwnedFd = socket.into();
    let mut command = Command::new(program);
    if prefix {
        command.arg("--internal-tool-broker-v1");
    }
    command
        .arg("run")
        .stdin(Stdio::from(descriptor))
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command.env_remove("DRMR_TOOL_BROKER_V1");
    if let Some(marker) = marker {
        command.env("DRMR_TOOL_BROKER_V1", marker);
    }
    Process(command.spawn().unwrap())
}
fn socket() -> (UnixStream, UnixStream) {
    let (parent, child) = UnixStream::pair().unwrap();
    parent
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    parent
        .set_write_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    (parent, child)
}

#[test]
fn native_fixture_acknowledges_exact_handoff_then_strips_only_the_private_prefix() {
    let root = tempfile::tempdir().unwrap();
    let program = installed(root.path());
    let (mut parent, child) = socket();
    let mut process = launch(&program, child, true, Some(&std::process::id().to_string()));
    let mut hello = [0; 8];
    parent.read_exact(&mut hello).unwrap();
    assert_eq!(&hello, b"DRMRBRK1");
    parent.write_all(b"OK01").unwrap();
    assert!(completed(&mut process.0).success());
    let mut output = Vec::new();
    process
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    assert_eq!(output, b"fixed-response");
    assert_eq!(fs::read(root.path().join("argv")).unwrap(), b"run");
}

#[test]
fn native_fixture_rejects_invalid_ack_before_any_mode_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let program = installed(root.path());
    let (mut parent, child) = socket();
    let mut process = launch(&program, child, true, Some(&std::process::id().to_string()));
    let mut hello = [0; 8];
    parent.read_exact(&mut hello).unwrap();
    parent.write_all(b"NOPE").unwrap();
    assert_eq!(completed(&mut process.0).code(), Some(125));
    assert!(!root.path().join("argv").exists());
}

#[test]
fn native_fixture_rejects_missing_prefix_missing_marker_and_non_socket_stdin() {
    let root = tempfile::tempdir().unwrap();
    let program = installed(root.path());
    for (prefix, marker) in [(false, Some("2")), (true, None), (true, Some("not-a-pid"))] {
        let (_parent, child) = socket();
        let mut process = launch(&program, child, prefix, marker);
        assert_eq!(completed(&mut process.0).code(), Some(125));
        assert!(!root.path().join("argv").exists());
    }
    let mut process = Process(
        Command::new(program)
            .args(["--internal-tool-broker-v1", "run"])
            .env("DRMR_TOOL_BROKER_V1", std::process::id().to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    assert_eq!(completed(&mut process.0).code(), Some(125));
    assert!(!root.path().join("argv").exists());
}
