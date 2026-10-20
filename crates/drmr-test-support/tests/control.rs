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

#[cfg(unix)]
#[test]
fn overlapping_invocations_keep_independent_descendant_locks_and_pid_records() {
    use std::os::unix::process::CommandExt as _;
    use std::time::{Duration, Instant};

    struct Worker(std::process::Child);
    impl Drop for Worker {
        fn drop(&mut self) {
            // Both groups were created by this test; cleanup is separate from the
            // production Stop/return oracle exercised by container qualification.
            let _ = Command::new("/bin/sh")
                .args([
                    "-c",
                    "kill -s KILL -- \"-$1\"",
                    "fixture",
                    &self.0.id().to_string(),
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn ready(worker: &mut Worker, root: &std::path::Path) -> std::path::PathBuf {
        let entry = root.join(worker.0.id().to_string());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !entry.join("complete").is_file() {
            assert!(
                worker.0.try_wait().unwrap().is_none(),
                "worker exited before its independent complete record"
            );
            assert!(Instant::now() < deadline, "invocation readiness deadline");
            std::thread::yield_now();
        }
        entry
    }
    fn pid(path: &std::path::Path) -> u32 {
        let value = std::fs::read_to_string(path).unwrap();
        assert!(value.ends_with('\n'));
        value.trim().parse().unwrap()
    }

    let directory = tempfile::tempdir().unwrap();
    let registry = directory.path().join("invocations");
    std::fs::create_dir(&registry).unwrap();
    let tool = install(
        &directory.path().join("worker"),
        &ToolSpec::default()
            .with("invocation_root", registry.to_str().unwrap())
            .with(
                "release_file",
                directory.path().join("never-release").to_str().unwrap(),
            ),
    )
    .unwrap();
    let spawn = || {
        Worker(
            Command::new(&tool)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    };
    let mut first = spawn();
    let first_entry = ready(&mut first, &registry);
    let first_descendant = pid(&first_entry.join("descendant.pid"));
    let first_lock = std::fs::File::open(first_entry.join("descendant.lock")).unwrap();
    assert!(matches!(
        first_lock.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));

    // Start B only after A holds its lock: this deterministically exercises the
    // old shared-ready/shared-lock collision, rather than relying on a race.
    let mut second = spawn();
    let second_entry = ready(&mut second, &registry);
    let second_descendant = pid(&second_entry.join("descendant.pid"));
    let second_lock = std::fs::File::open(second_entry.join("descendant.lock")).unwrap();
    assert_ne!(first_entry, second_entry);
    assert_ne!(first_descendant, second_descendant);
    assert_eq!(std::fs::read_dir(&registry).unwrap().count(), 2);
    for lock in [&first_lock, &second_lock] {
        assert!(matches!(
            lock.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
    }
    assert_eq!(pid(&first_entry.join("descendant.pid")), first_descendant);
    drop(second);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match second_lock.try_lock() {
            Ok(()) => break,
            Err(std::fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::yield_now();
            }
            other => panic!("owned fixture cleanup did not release its lock: {other:?}"),
        }
    }
    second_lock.unlock().unwrap();
    assert!(matches!(
        first_lock.try_lock(),
        Err(std::fs::TryLockError::WouldBlock)
    ));
}

#[test]
fn invocation_records_do_not_reinterpret_legacy_pid_controls() {
    let directory = tempfile::tempdir().unwrap();
    let registry = directory.path().join("invocations");
    let legacy = directory.path().join("legacy.pid");
    std::fs::create_dir(&registry).unwrap();
    let tool = install(
        &directory.path().join("worker"),
        &ToolSpec::default()
            .with("invocation_root", registry.to_str().unwrap())
            .with("pid_file", legacy.to_str().unwrap()),
    )
    .unwrap();
    let result = Command::new(tool).output().unwrap();
    assert_eq!(result.status.code(), Some(125));
    assert!(!legacy.exists());
    assert_eq!(std::fs::read_dir(registry).unwrap().count(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn a_registry_fixture_refuses_to_borrow_its_parent_process_group() {
    let directory = tempfile::tempdir().unwrap();
    let registry = directory.path().join("invocations");
    std::fs::create_dir(&registry).unwrap();
    let tool = install(
        &directory.path().join("worker"),
        &ToolSpec::default().with("invocation_root", registry.to_str().unwrap()),
    )
    .unwrap();
    let result = Command::new(tool).output().unwrap();
    assert_eq!(result.status.code(), Some(125));
    assert_eq!(std::fs::read_dir(registry).unwrap().count(), 0);
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
