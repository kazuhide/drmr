use drmr_test_support::{ToolSpec, install};
use std::io::Write as _;
use std::process::{Command, Stdio};

#[test]
fn controlled_response_consumes_stdin_before_answer_and_captures_environment_without_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("stdin");
    let environment = directory.path().join("environment");
    let arguments = directory.path().join("argv");
    let tool = install(
        &directory.path().join("worker"),
        &ToolSpec {
            stdout: b"[\"ok\", false]\n".to_vec(),
            ..ToolSpec::default()
        }
        .with("stdin_file", input.to_str().unwrap())
        .with("drm_env_file", environment.to_str().unwrap())
        .with("argv_file", arguments.to_str().unwrap()),
    )
    .unwrap();
    let mut child = Command::new(tool)
        .env("DRM_KID", "synthetic-kid")
        .env("DRM_KEY", "synthetic-key")
        .env("DRM_IV", "synthetic-iv")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut pipe = child.stdin.take().unwrap();
    pipe.write_all(b"private\0configuration\r\n").unwrap();
    assert!(
        child.try_wait().unwrap().is_none(),
        "the answer requires stdin EOF"
    );
    drop(pipe);
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"[\"ok\", false]\n");
    assert_eq!(std::fs::read(input).unwrap(), b"private\0configuration\r\n");
    assert_eq!(
        std::fs::read(environment).unwrap(),
        b"synthetic-kid synthetic-key synthetic-iv"
    );
    assert_eq!(std::fs::read(arguments).unwrap(), b"");
}

#[test]
fn controlled_response_waits_for_release_after_signalling_readiness() {
    let directory = tempfile::tempdir().unwrap();
    let release = directory.path().join("release");
    let ready = directory.path().join("ready");
    let tool = install(
        &directory.path().join("worker"),
        &ToolSpec {
            stdout: b"released".to_vec(),
            ..ToolSpec::default()
        }
        .with("release_file", release.to_str().unwrap())
        .with("ready_file", ready.to_str().unwrap()),
    )
    .unwrap();
    let mut child = Command::new(tool).stdout(Stdio::piped()).spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !ready.is_file() {
        assert!(
            child.try_wait().unwrap().is_none(),
            "worker exited before readiness"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "worker readiness deadline"
        );
        std::thread::yield_now();
    }
    assert!(child.try_wait().unwrap().is_none());
    std::fs::write(release, b"release").unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    assert_eq!(result.stdout, b"released");
}
