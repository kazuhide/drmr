//! An owned Unix process group must finish releasing resources before cleanup returns.
#![cfg(unix)]

use drmr_core::native::ManagedChild;
use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn exclusive_lock(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    file.try_lock().map_err(io::Error::other)?;
    Ok(file)
}

#[test]
fn confirmed_cleanup_releases_a_descendants_last_lock_without_post_return_waiting() {
    // Every iteration must pass; this exercises natural teardown, not a retry after failure.
    for case in 0..32 {
        // Read the inherited limit without changing it; reserve lock/readiness descriptors.
        let requested = rustix::process::getrlimit(rustix::process::Resource::Nofile)
            .current
            .unwrap_or(u64::MAX)
            .saturating_sub(64)
            .min(4096);
        assert!(requested > 0);
        let root = tempfile::tempdir().unwrap();
        let pid = root.path().join("descendant.pid");
        let count_file = root.path().join("descriptor-count");
        let path = root.path().join("held.lock");
        let tool = drmr_test_support::install(
            &root.path().join("response"),
            &drmr_test_support::ToolSpec::default()
                .with("descendant_pid_file", pid.to_str().unwrap())
                .with("descendant_lock_file", path.to_str().unwrap())
                .with("descendant_open_files", requested.to_string())
                .with(
                    "descendant_descriptor_count_file",
                    count_file.to_str().unwrap(),
                )
                .with(
                    "release_file",
                    root.path().join("never-release").to_str().unwrap(),
                ),
        )
        .unwrap();
        let mut command = Command::new(tool);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = ManagedChild::spawn(command, true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let count = loop {
            if std::fs::read_to_string(&pid)
                .is_ok_and(|text| text.ends_with('\n') && text.trim().parse::<u32>().is_ok())
            {
                break std::fs::read_to_string(&count_file)
                    .unwrap()
                    .parse::<u64>()
                    .unwrap();
            }
            assert!(
                child.try_wait().unwrap().is_none(),
                "fixture exited before readiness"
            );
            assert!(Instant::now() < deadline, "fixture did not become ready");
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(count, requested);
        assert!(exclusive_lock(&path).is_err(), "fixture must hold the lock");
        let completed = child.finish();
        // Capture the actual lock state immediately, before diagnostics or any other I/O.
        let immediate = exclusive_lock(&path);
        assert!(
            completed.is_ok(),
            "case={case}, ordinary_handles={count}: {completed:?}"
        );
        immediate.unwrap_or_else(|error| {
            panic!("confirmed cleanup preceded lock release: case={case}, ordinary_handles={count}: {error:?}")
        });
        child.finish().unwrap();
        root.close().unwrap();
    }
}
