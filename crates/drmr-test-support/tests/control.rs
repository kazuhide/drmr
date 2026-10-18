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

#[test]
fn packager_proxy_forwards_demux_and_holds_only_ts_output() {
    let directory = tempfile::tempdir().unwrap();
    let release = directory.path().join("release");
    let audit = directory.path().join("route-audit");
    std::fs::write(&release, b"release").unwrap();
    let actual = install(
        &directory.path().join("real"),
        &ToolSpec {
            stdout: b"forwarded".to_vec(),
            ..ToolSpec::default()
        },
    )
    .unwrap();
    let proxy = install(
        &directory.path().join("proxy"),
        &ToolSpec {
            stdout: b"held".to_vec(),
            ..ToolSpec::default()
        }
        .with("real_packager", actual.to_str().unwrap())
        .with("packager_route_audit", audit.to_str().unwrap())
        .with("release_file", release.to_str().unwrap()),
    )
    .unwrap();
    let demux = Command::new(&proxy)
        .arg("input=fixture.ts,stream=video,output=copy.mp4")
        .output()
        .unwrap();
    assert!(demux.status.success());
    assert_eq!(demux.stdout, b"forwarded");
    assert_eq!(std::fs::read(&audit).unwrap(), b"demux\n");
    let output = Command::new(&proxy)
        .args([
            "input=copy.mp4,stream=0,segment_template=segment-$Number$.ts",
            "--hls_master_playlist_output",
            "master.m3u8",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"held");
    assert_eq!(std::fs::read(&audit).unwrap(), b"demux\nts-output\n");
    let invalid = Command::new(&proxy)
        .args([
            "input=copy.mp4,stream=0,segment_template=segment-$Number$.m4s",
            "--hls_master_playlist_output",
            "master.m3u8",
        ])
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(125));
    assert_eq!(std::fs::read(audit).unwrap(), b"demux\nts-output\n");
}
