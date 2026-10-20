use super::*;
use crate::{
    conversion::{Converter, backend::tool_error},
    events::{Collector, NoSink},
    toolrun::{CleanupOwner, RunOptions, Secrets, ToolRunner},
};
use std::time::{Duration, Instant};

fn backend(program: &Path) -> Backend {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        program.to_str().unwrap(),
        "unused",
    )
    .backend
}
fn flag_path(argv: &[String]) -> &Path {
    Path::new(
        argv.iter()
            .find_map(|value| value.strip_prefix("--flagfile="))
            .unwrap(),
    )
}
fn assert_cleaned(path: &Path) {
    assert!(!path.exists());
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn private_flags_are_exact_bounded_and_cleaned_after_success_or_failure() {
    let temp = native::private_tempdir().unwrap();
    let program = temp.path().join("unused");
    for success in [true, false] {
        let mut observed = PathBuf::new();
        let result = run_with(&backend(&program), &[], "00aaff", |_, argv, _| {
            observed = flag_path(argv).to_owned();
            let mut private = native::open_private_read(&observed).unwrap();
            let mut bytes = Vec::new();
            private.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"--pssh=00aaff\n");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                assert_eq!(
                    private.metadata().unwrap().permissions().mode() & 0o777,
                    0o600
                );
                assert_eq!(
                    std::fs::metadata(observed.parent().unwrap().parent().unwrap())
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
            }
            if success {
                Ok(b"complete".to_vec())
            } else {
                Err(ConversionError("media-tool-failed"))
            }
        });
        assert_eq!(result.is_ok(), success);
        assert_cleaned(&observed);
    }
}

#[test]
fn replacement_and_modified_flags_are_refused_or_blocked_by_the_read_guard() {
    for replace in [true, false] {
        let flags = PrivatePssh::new("00aaff", None, None).unwrap();
        let modified = if replace {
            let moved = flags.path.with_file_name("moved.flags");
            if std::fs::rename(&flags.path, moved).is_ok() {
                native::create_private_new(&flags.path)
                    .unwrap()
                    .write_all(b"--pssh=00aaff\n")
                    .unwrap();
                true
            } else {
                false
            }
        } else {
            std::fs::write(&flags.path, b"--pssh=00aafe\n").is_ok()
        };
        assert_eq!(flags.check("00aaff").is_err(), modified);
        let path = flags.path.clone();
        drop(flags);
        assert_cleaned(&path);
    }
}

#[test]
fn a_replaced_tool_visible_directory_cannot_delete_foreign_entries() {
    let temp = native::private_tempdir().unwrap();
    let foreign = temp.path().join("foreign");
    native::create_private_dir_all(&foreign).unwrap();
    native::create_private_new(&foreign.join("sentinel"))
        .unwrap()
        .write_all(b"foreign directory must survive")
        .unwrap();
    let flags = PrivatePssh::new("00aaff", Some(temp.path()), None).unwrap();
    let visible = flags.stage.path().to_owned();
    let held = temp.path().join("held-media");
    let moved = std::fs::rename(&visible, &held);
    if cfg!(windows) && moved.is_err() {
        // An OS sharing denial also prevents substitution of the tool-visible directory.
        flags.check("00aaff").unwrap();
        drop(flags);
        assert_eq!(
            std::fs::read(foreign.join("sentinel")).unwrap(),
            b"foreign directory must survive"
        );
        return;
    }
    moved.unwrap();
    std::fs::rename(&foreign, &visible).unwrap();
    assert_eq!(
        std::fs::read(visible.join("sentinel")).unwrap(),
        b"foreign directory must survive"
    );
    assert_eq!(flags.check("00aaff"), Err(INVALID));
    drop(flags);
    assert_eq!(
        std::fs::read(visible.join("sentinel")).unwrap(),
        b"foreign directory must survive"
    );
    assert_eq!(
        std::fs::read(held.join("protection.flags")).unwrap(),
        b"--pssh=00aaff\n"
    );
}

#[test]
fn constructor_failure_preserves_a_replaced_workspace_before_the_owner_exists() {
    let temp = native::private_tempdir().unwrap();
    let foreign = temp.path().join("foreign");
    native::create_private_dir_all(&foreign).unwrap();
    native::create_private_new(&foreign.join("sentinel"))
        .unwrap()
        .write_all(b"constructor must preserve foreign entries")
        .unwrap();
    let mut visible = PathBuf::new();
    let mut blocked = false;
    let result = PrivatePssh::new_observed("00aaff", Some(temp.path()), None, |stage| {
        visible = stage.path().to_owned();
        let moved = std::fs::rename(&visible, temp.path().join("held-media"));
        if cfg!(windows) && moved.is_err() {
            blocked = true;
            return;
        }
        moved.unwrap();
        std::fs::rename(&foreign, &visible).unwrap();
        assert!(visible.join("sentinel").is_file());
    });
    if blocked {
        drop(result.unwrap());
        assert!(foreign.join("sentinel").is_file());
    } else {
        assert_eq!(result.err(), Some(INVALID));
        assert_eq!(
            std::fs::read(visible.join("sentinel")).ok().as_deref(),
            Some(b"constructor must preserve foreign entries".as_slice())
        );
        assert_eq!(
            std::fs::read(temp.path().join("held-media/protection.flags")).unwrap(),
            b"--pssh=00aaff\n"
        );
    }
}

#[test]
fn flagfile_separators_are_refused_without_changing_the_temporary_parent() {
    let temp = native::private_tempdir().unwrap();
    for name in ["comma,parent", "parent with spaces"] {
        let parent = temp.path().join(name);
        native::create_private_dir_all(&parent).unwrap();
        let result = PrivatePssh::new("00aaff", Some(&parent), None);
        if name.contains(',') {
            assert_eq!(result.err(), Some(ConversionError("unsupported-path")));
        } else {
            let flags = result.unwrap();
            assert!(argument(&flags.path).unwrap().contains(name));
            flags.check("00aaff").unwrap();
        }
        assert_eq!(std::fs::read_dir(parent).unwrap().count(), 0);
    }
    for value in ["00\n--flagfile=other", "gg", "0", ""] {
        assert_eq!(PrivatePssh::new(value, None, None).err(), Some(INVALID));
    }
}

#[test]
fn timeout_and_cancellation_reap_the_tool_before_deleting_private_flags() {
    let temp = native::private_tempdir().unwrap();
    for cancel in [false, true] {
        let ready = temp.path().join(format!("ready-{cancel}"));
        let audit = temp.path().join(format!("argv-{cancel}"));
        let program = drmr_test_support::install(
            &temp.path().join(format!("response-{cancel}")),
            &drmr_test_support::ToolSpec {
                sleep_ms: 60_000,
                ..drmr_test_support::ToolSpec::default()
            }
            .with("ready_file", ready.to_str().unwrap())
            .with("argv_file", audit.to_str().unwrap()),
        )
        .unwrap();
        let mut backend = backend(&program);
        backend.timeout = if cancel {
            Duration::from_secs(10)
        } else {
            Duration::from_millis(300)
        };
        let stopped = backend.stop.clone();
        let cancel_thread = cancel.then(|| {
            let ready = ready.clone();
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(5);
                while !ready.is_file() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                assert!(ready.is_file(), "fixture must start before cancellation");
                stopped.set();
            })
        });
        let result = run(&backend, &[program.to_str().unwrap().into()], "00aaff", &[]);
        if let Some(thread) = cancel_thread {
            thread.join().unwrap();
        }
        assert_eq!(
            result.err(),
            Some(ConversionError(if cancel {
                "cancelled"
            } else {
                "media-tool-timeout"
            }))
        );
        assert!(ready.is_file());
        let capture = std::fs::read_to_string(audit).unwrap();
        let arguments = capture.lines().map(str::to_owned).collect::<Vec<_>>();
        assert_cleaned(flag_path(&arguments));
    }
}

#[test]
fn unconfirmed_cleanup_retains_the_flags_and_preserves_the_callers_owner_and_sinks() {
    let temp = native::private_tempdir().unwrap();
    let ready = temp.path().join("ready");
    let program = drmr_test_support::install(
        &temp.path().join("response"),
        &drmr_test_support::ToolSpec {
            sleep_ms: 60_000,
            ..drmr_test_support::ToolSpec::default()
        }
        .with("ready_file", ready.to_str().unwrap()),
    )
    .unwrap();
    let owner = Arc::new(CleanupOwner::new());
    let sink = Arc::new(Collector::default());
    let observer = Arc::new(Collector::default());
    let runner = ToolRunner::new(Arc::new(Secrets::new()), sink.clone())
        .with_cleanup_owner(owner.clone())
        .with_path(temp.path().as_os_str())
        .observed(observer.clone());
    let backend = Converter::new(Arc::new(runner), "unused", "unused").backend;
    let mut path = PathBuf::new();
    let result = run_with(
        &backend,
        &[program.to_str().unwrap().into()],
        "00aaff",
        |protected, argv, receipt| {
            path = flag_path(argv).to_owned();
            assert_eq!(protected.runner.path(), Some(temp.path().as_os_str()));
            let (_, actual_owner) = protected.runner.ensuring_cleanup_owner();
            assert!(Arc::ptr_eq(&owner, &actual_owner));
            let failed = protected.runner.run_tool_injected_cleanup_with_receipt(
                argv,
                &RunOptions {
                    timeout: Some(Duration::from_millis(100)),
                    ..RunOptions::default()
                },
                receipt,
                |_| {
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while !ready.is_file() && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    assert!(ready.is_file());
                    assert!(native::open_private_read(&path).is_ok());
                    Err(std::io::Error::other("injected cleanup uncertainty"))
                },
            );
            assert!(owner.has_pending());
            assert!(path.exists());
            failed
                .map(|result| result.stdout)
                .map_err(|error| tool_error(&error))
        },
    );
    assert_eq!(
        result.err(),
        Some(ConversionError("media-cleanup-unconfirmed"))
    );
    assert!(!owner.has_pending());
    assert_cleaned(&path);
    assert!(sink.records().len() >= 2);
    assert_eq!(sink.records(), observer.records());
}

#[test]
fn denied_shared_admission_returns_without_waiting_for_an_unrelated_active_call() {
    let temp = native::private_tempdir().unwrap();
    let ready = temp.path().join("other-ready");
    let release = temp.path().join("other-release");
    let other = drmr_test_support::install(
        &temp.path().join("other"),
        &drmr_test_support::ToolSpec::default()
            .with("ready_file", ready.to_str().unwrap())
            .with("release_file", release.to_str().unwrap()),
    )
    .unwrap();
    let marker = temp.path().join("must-not-start");
    let denied = drmr_test_support::install(
        &temp.path().join("denied"),
        &drmr_test_support::ToolSpec::default().with("ready_file", marker.to_str().unwrap()),
    )
    .unwrap();
    let owner = Arc::new(CleanupOwner::new());
    let runner = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
            .with_cleanup_owner(owner.clone()),
    );
    let other_runner = runner.clone();
    let active = std::thread::spawn(move || {
        other_runner.run_tool(
            &[other.to_str().unwrap().into()],
            "unrelated",
            &RunOptions::default(),
        )
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready.is_file() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(ready.is_file() && owner.has_pending());
    let backend = Converter::new(runner, "unused", "unused").backend;
    let (sent, received) = std::sync::mpsc::channel();
    let operation = std::thread::spawn(move || {
        let result = run(&backend, &[denied.to_str().unwrap().into()], "00aaff", &[]);
        sent.send(result).unwrap();
    });
    let prompt = received.recv_timeout(Duration::from_secs(2));
    let still_owned = owner.has_pending();
    // Release the unrelated call before asserting, even on the deadlocking regression.
    std::fs::write(release, b"release").unwrap();
    active.join().unwrap().unwrap();
    operation.join().unwrap();
    assert_eq!(
        prompt.ok(),
        Some(Err(ConversionError("media-cleanup-unconfirmed"))),
        "Denied admission must return while the unrelated caller is still waiting"
    );
    assert!(still_owned);
    assert!(!marker.exists());
}

#[test]
fn denied_shared_admission_does_not_reap_an_unrelated_retained_tree() {
    let temp = native::private_tempdir().unwrap();
    let ready = temp.path().join("ready");
    let program = drmr_test_support::install(
        &temp.path().join("response"),
        &drmr_test_support::ToolSpec {
            sleep_ms: 60_000,
            ..drmr_test_support::ToolSpec::default()
        }
        .with("ready_file", ready.to_str().unwrap()),
    )
    .unwrap();
    let owner = Arc::new(CleanupOwner::new());
    let runner = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
            .with_cleanup_owner(owner.clone()),
    );
    let mut started = false;
    let failed = runner.run_tool_injected_cleanup(
        &[program.to_str().unwrap().into()],
        &RunOptions {
            timeout: Some(Duration::from_millis(100)),
            ..RunOptions::default()
        },
        |_| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready.is_file() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            started = ready.is_file();
            Err(std::io::Error::other("injected cleanup uncertainty"))
        },
    );
    assert_eq!(failed.unwrap_err().kind, "cleanup-unconfirmed");
    let backend = Converter::new(runner, "unused", "unused").backend;
    let result = run(&backend, &[program.to_str().unwrap().into()], "00aaff", &[]);
    let still_owned = owner.has_pending();
    owner.retry_finish().unwrap();
    assert!(started);
    assert_eq!(
        result.err(),
        Some(ConversionError("media-cleanup-unconfirmed"))
    );
    assert!(
        still_owned,
        "A denied call must leave caller-owned cleanup unchanged"
    );
}
