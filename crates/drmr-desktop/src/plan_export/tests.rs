use super::*;
use std::net::TcpListener;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Instant;

static TEMP_ID: AtomicU64 = AtomicU64::new(0);
const ENDPOINT: &str = "/api/common/plans/0123456789abcdef0123456789abcdef/export";

struct Folders {
    root: PathBuf,
    input: PathBuf,
    output: PathBuf,
    private: PathBuf,
}

impl Folders {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "drmr-native-plan-{}-{}",
            std::process::id(),
            TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        assert!(!root.exists());
        drmr_core::native::create_private_dir_all(&root).unwrap();
        let this = Self {
            input: root.join("input"),
            output: root.join("drmr-output"),
            private: root.join("private"),
            root,
        };
        for path in [&this.input, &this.output, &this.private] {
            drmr_core::native::create_private_dir_new(path).unwrap();
        }
        this
    }

    fn service(&self, origin: ConsoleOrigin) -> (Arc<PlanExports>, Receiver<Notice>) {
        self.service_with_budget(origin, Duration::from_secs(2))
    }

    fn service_with_budget(
        &self,
        origin: ConsoleOrigin,
        budget: Duration,
    ) -> (Arc<PlanExports>, Receiver<Notice>) {
        let (sender, receiver) = mpsc::channel();
        let service = PlanExports::start_with_budget(
            origin,
            &self.input,
            &self.output,
            &self.private,
            move |notice| {
                let _ = sender.send(notice);
            },
            |_| {},
            budget,
        )
        .unwrap();
        (Arc::new(service), receiver)
    }

    fn owned(&self, path: &Path) -> OwnedFile {
        let media = MediaFolders::capture(&self.input, &self.output).unwrap();
        Destination::capture(path, &media)
            .unwrap()
            .create(&media)
            .unwrap()
    }
}

impl Drop for Folders {
    fn drop(&mut self) {
        let cleanup = std::fs::remove_dir_all(&self.root);
        if std::thread::panicking() {
            if cleanup.is_err() {
                eprintln!("fixture cleanup failed during an earlier test panic");
            }
        } else {
            cleanup.unwrap();
        }
    }
}

fn origin() -> ConsoleOrigin {
    ConsoleOrigin::new("127.0.0.1:4000".parse().unwrap()).unwrap()
}

fn serve(response: Vec<u8>, delay: Duration) -> (ConsoleOrigin, JoinHandle<Vec<u8>>, Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = ConsoleOrigin::new(listener.local_addr().unwrap()).unwrap();
    listener.set_nonblocking(true).unwrap();
    let (started, ready) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "missing owned export request");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("fixture accept failed: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut request = Vec::new();
        let mut chunk = [0; 512];
        while !request.windows(4).any(|part| part == b"\r\n\r\n") {
            let size = stream.read(&mut chunk).unwrap();
            assert!(size > 0 && request.len() < 8192);
            request.extend_from_slice(&chunk[..size]);
        }
        let _ = started.send(());
        std::thread::sleep(delay);
        let _ = stream.write_all(&response);
        request
    });
    (origin, worker, ready)
}

fn response(body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()
    ).into_bytes();
    response.extend_from_slice(body);
    response
}

fn submit(service: &PlanExports, origin: &ConsoleOrigin, path: PathBuf) {
    let selection = service
        .reserve(&origin.url().join(ENDPOINT).unwrap())
        .unwrap();
    service.selected(selection, Some(path));
}

#[test]
fn native_export_refuses_input_and_served_output_destinations() {
    let folders = Folders::new();
    let media = MediaFolders::capture(&folders.input, &folders.output).unwrap();
    for parent in [&folders.input, &folders.output] {
        assert!(Destination::capture(&parent.join("private.plan.json"), &media).is_err());
        assert!(!parent.join("private.plan.json").exists());
    }
    assert!(Destination::capture(&folders.private.join("private.plan.json"), &media).is_ok());
}

#[test]
fn native_export_refuses_ancestor_parent_for_a_selected_file() {
    let folders = Folders::new();
    let media = MediaFolders::capture(&folders.input, &folders.output).unwrap();
    let target = folders.root.join("ancestor.plan.json");
    assert!(Destination::capture(&target, &media).is_err());
    assert!(!target.exists());
}

#[test]
fn native_export_refuses_ancestor_directory_for_private_plan_preparation() {
    let folders = Folders::new();
    let media = MediaFolders::capture(&folders.input, &folders.output).unwrap();
    assert!(media.prepare_directory(&folders.root).is_err());
}

#[test]
fn native_export_checks_and_cleanup_share_the_actual_private_writer() {
    let folders = Folders::new();
    let media = MediaFolders::capture(&folders.input, &folders.output).unwrap();
    let target = folders.private.join("held-writer.plan.json");
    let mut owned = Destination::capture(&target, &media)
        .unwrap()
        .create(&media)
        .unwrap();
    let identity = drmr_core::native::FileIdentity::read(owned.file.as_ref().unwrap()).unwrap();
    #[cfg(windows)]
    assert_eq!(
        drmr_core::native::open_private_read(&target)
            .unwrap_err()
            .raw_os_error(),
        Some(32),
        "an immutable reader cannot share the actual writable creation handle"
    );
    owned.check(&media).unwrap();
    owned
        .file
        .as_mut()
        .unwrap()
        .write_all(b"original plan bytes")
        .unwrap();
    owned.file.as_ref().unwrap().sync_all().unwrap();
    owned.check(&media).unwrap();
    let mut copied = Vec::new();
    drmr_core::native::open_private_shared_read(&target)
        .unwrap()
        .read_to_end(&mut copied)
        .unwrap();
    assert_eq!(copied, b"original plan bytes");
    assert!(
        drmr_core::native::FileIdentity::read(owned.file.as_ref().unwrap()).unwrap() == identity
    );
    owned.cleanup().unwrap();
    assert!(!target.exists());
    // Cleanup starts with the actual writer held, then closes it only after
    // admitting deletion of the matching object inside the private quarantine.
    assert!(owned.file.is_none());
    drop(owned);
    assert!(!target.try_exists().unwrap());
}

#[test]
fn native_export_close_fence_refuses_new_choosers() {
    let folders = Folders::new();
    let origin = origin();
    let (service, _) = folders.service(origin.clone());
    service.request_stop();
    let selection = service.reserve(&origin.url().join(ENDPOINT).unwrap());
    let incorrectly_admitted = selection.is_some();
    if let Some(selection) = selection {
        service.selected(selection, None);
    }
    assert!(service.shutdown());
    assert!(!incorrectly_admitted);
}

#[test]
fn native_export_admission_and_stop_do_not_wait_for_an_io_owner() {
    let folders = Folders::new();
    let origin = origin();
    let (service, _) = folders.service(origin.clone());
    let selection = service
        .reserve(&origin.url().join(ENDPOINT).unwrap())
        .unwrap();
    let target = folders.private.join("stalled-owner.json");
    let partial = folders.owned(&target);
    let mut state = service.shared.lock();
    state.current.as_mut().unwrap().phase = Phase::Working;
    state.partial = Some(partial);
    // Retain the actual writer and its I/O guard while both GUI entry points run.
    let (started, starts) = mpsc::channel();
    let (returned, returns) = mpsc::channel();
    let requester = service.clone();
    let url = origin.url().join(ENDPOINT).unwrap();
    let request_started = started.clone();
    let request_returned = returned.clone();
    let request = std::thread::spawn(move || {
        request_started.send(()).unwrap();
        request_returned
            .send((false, requester.request(&url)))
            .unwrap();
    });
    let stopper = service.clone();
    let stop = std::thread::spawn(move || {
        started.send(()).unwrap();
        returned.send((true, stopper.request_stop())).unwrap();
    });
    for _ in 0..2 {
        starts.recv_timeout(Duration::from_secs(2)).unwrap();
    }
    let mut immediate = Vec::new();
    for _ in 0..2 {
        if let Ok(result) = returns.recv_timeout(Duration::from_millis(200)) {
            immediate.push(result);
        }
    }
    assert!(state.partial.is_some(), "the stalled writer remains owned");
    drop(state);
    request.join().unwrap();
    stop.join().unwrap();
    assert_eq!(
        complete(&service.shared, selection.id, Err(ExportError::Cancelled)),
        None
    );
    assert!(service.shutdown());
    assert!(!target.exists());
    assert_eq!(immediate.len(), 2, "GUI entry points waited for file I/O");
    assert!(immediate.iter().all(|(_, admitted)| !admitted));
}

#[test]
fn native_export_stop_during_final_sync_guard_cannot_publish_the_copy() {
    let folders = Folders::new();
    let origin = origin();
    let (service, _) = folders.service(origin.clone());
    let selection = service
        .reserve(&origin.url().join(ENDPOINT).unwrap())
        .unwrap();
    let target = folders.private.join("stop-after-sync.json");
    {
        let mut state = service.shared.lock();
        state.current.as_mut().unwrap().phase = Phase::Working;
        state.partial = Some(folders.owned(&target));
    }
    let (start, started) = mpsc::channel();
    let (done, completed) = mpsc::channel();
    let stopper = service.clone();
    let stop = std::thread::spawn(move || {
        started.recv_timeout(Duration::from_secs(2)).unwrap();
        stopper.request_stop();
        done.send(()).unwrap();
    });
    let mut stopped_inside_guard = false;
    let result = write_plan_with(
        &service.shared,
        selection.id,
        b"actual synced bytes",
        || {
            start.send(()).unwrap();
            stopped_inside_guard = completed.recv_timeout(Duration::from_millis(200)).is_ok();
        },
    );
    stop.join().unwrap();
    assert!(service.shared.lock().partial.is_some());
    assert_eq!(std::fs::read(&target).unwrap(), b"actual synced bytes");
    assert_eq!(complete(&service.shared, selection.id, result), None);
    assert!(service.shutdown());
    assert!(!target.exists());
    assert!(stopped_inside_guard);
    assert!(matches!(result, Err(ExportError::Cancelled)));
}

#[test]
fn native_export_stop_token_survives_the_idle_wait_gap_until_actual_join() {
    let folders = Folders::new();
    let (service, _) = folders.service(origin());
    let (entered, ready) = mpsc::channel();
    let release = Arc::new(AtomicBool::new(false));
    let released = release.clone();
    *service.shared.before_park.lock().unwrap() = Some(Box::new(move || {
        entered.send(()).unwrap();
        // Blocking channel receives may consume this thread's park token.
        // Pause exactly the production gap without introducing another park.
        let deadline = Instant::now() + Duration::from_secs(3);
        while !released.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }));
    service.shared.wake();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    service.request_stop();
    let (done, completed) = mpsc::channel();
    let closer = service.clone();
    let shutdown = std::thread::spawn(move || {
        done.send(closer.shutdown()).unwrap();
    });
    assert!(completed.recv_timeout(Duration::from_millis(30)).is_err());
    release.store(true, Ordering::Release);
    assert!(completed.recv_timeout(Duration::from_secs(2)).unwrap());
    shutdown.join().unwrap();
    assert!(service.worker.lock().unwrap().is_none());
}

#[test]
fn native_export_retains_chooser_until_callback_and_discards_late_path() {
    let folders = Folders::new();
    let origin = origin();
    let (service, _) = folders.service(origin.clone());
    let selection = service
        .reserve(&origin.url().join(ENDPOINT).unwrap())
        .unwrap();
    assert!(service.request_stop());
    let (done, completed) = mpsc::channel();
    let owner = service.clone();
    let closer = std::thread::spawn(move || {
        done.send(owner.shutdown()).unwrap();
    });
    assert!(completed.recv_timeout(Duration::from_millis(30)).is_err());
    let target = folders.private.join("late.plan.json");
    service.selected(selection, Some(target.clone()));
    assert!(completed.recv_timeout(Duration::from_secs(2)).unwrap());
    closer.join().unwrap();
    assert!(!target.exists());
    assert!(service.worker.lock().unwrap().is_none());
}

#[test]
fn native_export_duplicate_and_stale_dialog_callbacks_cannot_replace_owner() {
    let folders = Folders::new();
    let origin = origin();
    let (service, _) = folders.service(origin.clone());
    let url = origin.url().join(ENDPOINT).unwrap();
    let first = service.reserve(&url).unwrap();
    assert!(service.reserve(&url).is_none());
    let stale = Selection {
        id: first.id,
        directory: first.directory.clone(),
    };
    service.selected(first, None);
    let second = service.reserve(&url).unwrap();
    service.selected(stale, Some(folders.private.join("stale.plan.json")));
    assert_eq!(
        service.shared.lock().current.as_ref().unwrap().id,
        second.id
    );
    service.selected(second, None);
    assert!(service.shutdown());
}

#[test]
fn native_export_preserves_original_bytes_and_creates_a_private_file() {
    let folders = Folders::new();
    let body = b"{\n  \"version\": 2, \"source\": \"private-source\"\n}\n";
    let (origin, server, _) = serve(response(body), Duration::ZERO);
    let (service, notices) = folders.service(origin.clone());
    let target = folders.private.join("conversion.plan.json");
    submit(&service, &origin, target.clone());
    assert_eq!(
        notices.recv_timeout(Duration::from_secs(3)).unwrap(),
        Notice::Saved
    );
    assert!(service.shutdown());
    assert_eq!(std::fs::read(&target).unwrap(), body);
    let file = drmr_core::native::open_private_read(&target).unwrap();
    assert!(file.metadata().unwrap().is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
    let request = server.join().unwrap();
    assert!(request.starts_with(format!("GET {ENDPOINT} HTTP/1.1\r\n").as_bytes()));
}

#[test]
fn native_export_existing_file_is_never_overwritten_or_deleted() {
    let folders = Folders::new();
    let target = folders.private.join("existing.json");
    let mut existing = drmr_core::native::create_private_new(&target).unwrap();
    existing.write_all(b"keep these bytes").unwrap();
    drop(existing);
    let (origin, server, _) = serve(response(b"{}"), Duration::ZERO);
    let (service, notices) = folders.service(origin.clone());
    submit(&service, &origin, target.clone());
    assert_eq!(
        notices.recv_timeout(Duration::from_secs(3)).unwrap(),
        Notice::Failed
    );
    assert!(service.shutdown());
    server.join().unwrap();
    assert_eq!(std::fs::read(target).unwrap(), b"keep these bytes");
}

#[test]
fn native_export_http_failures_do_not_create_a_target() {
    let folders = Folders::new();
    let cases = [
        (b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/foreign\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(), Duration::ZERO),
        (b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(), Duration::ZERO),
        (b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec(), Duration::ZERO),
        (format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", MAX_PLAN_BYTES + 1).into_bytes(), Duration::ZERO),
        (b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 20\r\nConnection: close\r\n\r\n{}".to_vec(), Duration::ZERO),
        (response(b"{}"), Duration::from_millis(800)),
    ];
    for (index, (response, delay)) in cases.into_iter().enumerate() {
        let (origin, server, _) = serve(response, delay);
        let (service, notices) = folders.service_with_budget(
            origin.clone(),
            if delay.is_zero() {
                Duration::from_secs(2)
            } else {
                Duration::from_millis(500)
            },
        );
        let target = folders.private.join(format!("failure-{index}.json"));
        submit(&service, &origin, target.clone());
        assert_eq!(
            notices.recv_timeout(Duration::from_secs(3)).unwrap(),
            Notice::Failed,
            "case {index}"
        );
        assert!(service.shutdown());
        server.join().unwrap();
        assert!(!target.exists(), "case {index}");
    }
}

#[test]
fn native_export_close_cancels_a_bounded_read_and_joins_the_worker() {
    let folders = Folders::new();
    let (origin, server, ready) = serve(response(b"{}"), Duration::from_millis(350));
    let (service, notices) = folders.service(origin.clone());
    let target = folders.private.join("cancelled.json");
    submit(&service, &origin, target.clone());
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    service.request_stop();
    assert!(service.shutdown());
    assert!(service.worker.lock().unwrap().is_none());
    assert!(!target.exists());
    assert!(notices.try_recv().is_err());
    server.join().unwrap();
}

#[test]
fn native_export_partial_io_failure_removes_only_its_created_file() {
    let folders = Folders::new();
    let origin = origin();
    let (service, _) = folders.service(origin.clone());
    let selection = service
        .reserve(&origin.url().join(ENDPOINT).unwrap())
        .unwrap();
    let target = folders.private.join("partial.json");
    let mut partial = folders.owned(&target);
    partial.file = Some(drmr_core::native::open_private_shared_read(&target).unwrap());
    {
        let mut state = service.shared.lock();
        state.current.as_mut().unwrap().phase = Phase::Working;
        state.partial = Some(partial);
    }
    let result = write_plan(
        &service.shared,
        selection.id,
        b"cannot write a read-only handle",
    );
    assert!(matches!(result, Err(ExportError::Failed)));
    assert_eq!(
        complete(&service.shared, selection.id, result),
        Some(Notice::Failed)
    );
    assert!(service.shutdown());
    assert!(!target.exists());
}

#[test]
fn native_export_cancellation_fence_prevents_another_partial_write() {
    let folders = Folders::new();
    let origin = origin();
    let (service, _) = folders.service(origin.clone());
    let selection = service
        .reserve(&origin.url().join(ENDPOINT).unwrap())
        .unwrap();
    let target = folders.private.join("cancelled-partial.json");
    let partial = folders.owned(&target);
    {
        let mut state = service.shared.lock();
        state.current.as_mut().unwrap().phase = Phase::Working;
        state.partial = Some(partial);
    }
    service.request_stop();
    let result = write_plan(&service.shared, selection.id, b"must not be written");
    assert!(matches!(result, Err(ExportError::Cancelled)));
    assert_eq!(std::fs::read(&target).unwrap(), [] as [u8; 0]);
    assert_eq!(complete(&service.shared, selection.id, result), None);
    assert!(service.shutdown());
    assert!(!target.exists());
}

#[test]
fn native_export_failed_cleanup_retains_identity_and_can_be_retried() {
    let folders = Folders::new();
    let origin = origin();
    let (service, _) = folders.service(origin.clone());
    let selection = service
        .reserve(&origin.url().join(ENDPOINT).unwrap())
        .unwrap();
    let target = folders.private.join("partial.json");
    let partial = folders.owned(&target);
    let moved = folders.private.join("owned-moved.json");
    std::fs::rename(&target, &moved).unwrap();
    let mut foreign = drmr_core::native::create_private_new(&target).unwrap();
    foreign.write_all(b"foreign bytes").unwrap();
    drop(foreign);
    {
        let mut state = service.shared.lock();
        state.current.as_mut().unwrap().phase = Phase::Working;
        state.partial = Some(partial);
    }
    assert_eq!(
        complete(&service.shared, selection.id, Err(ExportError::Failed)),
        Some(Notice::CleanupUnconfirmed)
    );
    assert!(!service.shutdown());
    assert!(service.shared.lock().partial.is_some());
    assert_eq!(std::fs::read(&target).unwrap(), b"foreign bytes");
    let foreign_copy = folders.private.join("preserved-foreign.json");
    std::fs::rename(&target, &foreign_copy).unwrap();
    std::fs::rename(&moved, &target).unwrap();
    assert!(service.shutdown());
    assert!(!target.exists());
    assert_eq!(std::fs::read(foreign_copy).unwrap(), b"foreign bytes");
}

#[test]
fn native_export_shutdown_waits_for_the_actual_service_join() {
    let folders = Folders::new();
    let origin = origin();
    let (chosen, chooser) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let service = Arc::new(
        PlanExports::start_with_budget(
            origin.clone(),
            &folders.input,
            &folders.output,
            &folders.private,
            |_| {},
            move |selection| {
                chosen.send(selection).unwrap();
                released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(3))
                    .unwrap();
            },
            Duration::from_millis(200),
        )
        .unwrap(),
    );
    assert!(service.request(&origin.url().join(ENDPOINT).unwrap()));
    let selection = chooser.recv_timeout(Duration::from_secs(2)).unwrap();
    service.selected(selection, None);
    let (done, completed) = mpsc::channel();
    let closers: Vec<_> = (0..2)
        .map(|_| {
            let owner = service.clone();
            let done = done.clone();
            std::thread::spawn(move || {
                done.send(owner.shutdown()).unwrap();
            })
        })
        .collect();
    assert!(completed.recv_timeout(Duration::from_millis(30)).is_err());
    release.send(()).unwrap();
    assert!(completed.recv_timeout(Duration::from_secs(2)).unwrap());
    assert!(completed.recv_timeout(Duration::from_secs(2)).unwrap());
    for closer in closers {
        closer.join().unwrap();
    }
}

#[test]
fn native_export_streamed_bytes_obey_the_limit_without_content_length() {
    assert!(matches!(
        read_plan(&b"12345"[..], || false, 4),
        Err(ExportError::Failed)
    ));
    assert_eq!(read_plan(&b"1234"[..], || false, 4).ok().unwrap(), b"1234");
    assert!(matches!(
        read_plan(&b"1234"[..], || true, 4),
        Err(ExportError::Cancelled)
    ));
}

#[test]
fn native_export_prepares_a_private_default_outside_the_served_media() {
    let folders = Folders::new();
    let media = MediaFolders::capture(&folders.input, &folders.output).unwrap();
    let default = folders.root.join("drmr-plans");
    let actual = media.prepare_directory(&default).unwrap();
    assert_eq!(actual, std::fs::canonicalize(&default).unwrap());
    assert!(Destination::capture(&default.join("plan.json"), &media).is_ok());
    let bad = folders.input.join("drmr-plans");
    assert!(media.prepare_directory(&bad).is_err());
    assert!(!bad.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&default, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(media.prepare_directory(&default).is_err());
        assert_eq!(
            std::fs::metadata(&default).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}

#[cfg(windows)]
#[test]
fn native_export_existing_windows_directory_is_not_hardened_on_failure() {
    let folders = Folders::new();
    // The existing media helper explicitly installs inheritable owner ACEs.
    // Strict secret-file parents require protected ACEs without these flags.
    drmr_core::native::create_media_dir_all(&folders.private).unwrap();
    let probe = folders.private.join("must-not-be-created.json");
    let refused = match drmr_core::native::create_private_new(&probe) {
        Ok(file) => {
            drop(file);
            std::fs::remove_file(&probe).unwrap();
            false
        }
        Err(_) => true,
    };
    assert!(
        refused,
        "media ACL must not satisfy the strict private policy"
    );
    let media = MediaFolders::capture(&folders.input, &folders.output).unwrap();
    assert!(media.prepare_directory(&folders.private).is_err());
    assert!(Destination::capture(&probe, &media).is_err());
    assert!(drmr_core::native::create_private_new(&probe).is_err());
    assert!(!probe.exists());
}

#[test]
fn native_export_cleanup_gap_leaf_replacement_is_preserved() {
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let moved = folders.private.join("owned-moved.json");
    let mut partial = folders.owned(&target);
    let result = partial.cleanup_with(|point| {
        if point == files::CleanupPoint::BeforeMove {
            std::fs::rename(&target, &moved)?;
            drmr_core::native::create_private_new(&target)?.write_all(b"gap replacement")?;
        }
        Ok(())
    });
    assert!(
        result.is_err(),
        "a replaced entry is never confirmed as owned cleanup"
    );
    assert_eq!(std::fs::read(&target).unwrap(), b"gap replacement");
    assert!(moved.exists());
}

#[cfg(unix)]
#[test]
fn native_export_cleanup_gap_parent_replacement_is_preserved() {
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let moved = folders.root.join("moved-private");
    let mut partial = folders.owned(&target);
    let result = partial.cleanup_with(|point| {
        if point == files::CleanupPoint::BeforeMove {
            std::fs::rename(&folders.private, &moved)?;
            drmr_core::native::create_private_dir_new(&folders.private)?;
            drmr_core::native::create_private_new(&target)?.write_all(b"new parent")?;
        }
        Ok(())
    });
    assert!(result.is_err(), "a replaced parent remains unconfirmed");
    assert_eq!(std::fs::read(&target).unwrap(), b"new parent");
    let quarantine_name = partial.quarantine_path().unwrap().file_name().unwrap();
    assert!(moved.join(quarantine_name).join("entry").exists());
    let displaced = folders.root.join("preserved-new-parent");
    std::fs::rename(&folders.private, &displaced).unwrap();
    std::fs::rename(&moved, &folders.private).unwrap();
    partial.cleanup().unwrap();
    assert_eq!(
        std::fs::read(displaced.join("partial.json")).unwrap(),
        b"new parent"
    );
}

#[test]
fn native_export_new_original_name_after_move_is_not_deleted() {
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let mut partial = folders.owned(&target);
    partial
        .cleanup_with(|point| {
            if point == files::CleanupPoint::AfterMove {
                drmr_core::native::create_private_new(&target)?.write_all(b"new original slot")?;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"new original slot");
    assert!(partial.file.is_none());
    assert!(partial.quarantine_path().is_none());
    assert_eq!(std::fs::read_dir(&folders.private).unwrap().count(), 1);
}

#[test]
fn native_export_unknown_moved_entry_and_occupied_restore_remain_owned() {
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let mut partial = folders.owned(&target);
    let result = partial.cleanup_with(|point| {
        match point {
            files::CleanupPoint::AfterMove => {
                drmr_core::native::create_private_new(&target)?.write_all(b"occupied original")?;
            }
            files::CleanupPoint::Inspect => return Err(io::ErrorKind::Interrupted.into()),
            _ => {}
        }
        Ok(())
    });
    assert!(result.is_err());
    let quarantine = partial.quarantine_path().unwrap().to_path_buf();
    assert!(quarantine.join("entry").exists());
    assert!(partial.file.is_some());
    assert_eq!(std::fs::read(&target).unwrap(), b"occupied original");
    // Retry inspects the retained moved entry, never the new original slot.
    partial.cleanup().unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"occupied original");
    assert!(!quarantine.exists());
    assert!(partial.file.is_none());
}

#[test]
fn native_export_failed_restore_preserves_foreign_quarantine_entry() {
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let moved = folders.private.join("actual-writer.json");
    let mut partial = folders.owned(&target);
    let result = partial.cleanup_with(|point| {
        if point == files::CleanupPoint::AfterMove {
            let quarantine = std::fs::read_dir(&folders.private)?
                .map(|entry| entry.unwrap().path())
                .find(|path| {
                    path.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".drmr-plan-cleanup-")
                })
                .unwrap();
            std::fs::rename(quarantine.join("entry"), &moved)?;
            drmr_core::native::create_private_new(&quarantine.join("entry"))?
                .write_all(b"foreign quarantine entry")?;
        }
        if point == files::CleanupPoint::Restore {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Ok(())
    });
    assert!(result.is_err());
    let quarantine = partial.quarantine_path().unwrap().to_path_buf();
    assert_eq!(
        std::fs::read(quarantine.join("entry")).unwrap(),
        b"foreign quarantine entry"
    );
    assert!(moved.exists());
    assert!(partial.file.is_some());
    drmr_core::native::create_private_new(&target)
        .unwrap()
        .write_all(b"occupied slot")
        .unwrap();
    assert!(partial.cleanup().is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"occupied slot");
    assert_eq!(
        std::fs::read(quarantine.join("entry")).unwrap(),
        b"foreign quarantine entry"
    );
    assert!(moved.exists());
}

#[test]
fn native_export_missing_original_does_not_discard_a_linked_writer() {
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let moved = folders.private.join("moved-writer.json");
    let mut partial = folders.owned(&target);
    std::fs::rename(&target, &moved).unwrap();
    assert!(partial.cleanup().is_err());
    assert!(partial.file.is_some());
    assert!(moved.exists());
    std::fs::rename(&moved, &target).unwrap();
    partial.cleanup().unwrap();
    assert!(!target.exists());
}

#[cfg(unix)]
#[test]
fn native_export_cleanup_gap_symlink_is_restored_without_following_it() {
    use std::os::unix::fs::symlink;
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let moved = folders.private.join("actual-writer.json");
    let foreign = folders.private.join("foreign.json");
    drmr_core::native::create_private_new(&foreign)
        .unwrap()
        .write_all(b"foreign content")
        .unwrap();
    let mut partial = folders.owned(&target);
    assert!(
        partial
            .cleanup_with(|point| {
                if point == files::CleanupPoint::BeforeMove {
                    std::fs::rename(&target, &moved)?;
                    symlink(&foreign, &target)?;
                }
                Ok(())
            })
            .is_err()
    );
    assert_eq!(std::fs::read_link(&target).unwrap(), foreign);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign content");
    assert!(moved.exists());
    assert!(partial.file.is_some());
}

#[test]
fn native_export_quarantine_creation_never_adopts_an_existing_folder() {
    let folders = Folders::new();
    let parent = files::Directory::capture(&folders.private).unwrap();
    let mut quarantine = quarantine::Quarantine::planned(&parent);
    let collision = quarantine.path().to_path_buf();
    drmr_core::native::create_private_dir_new(&collision).unwrap();
    drmr_core::native::create_private_new(&collision.join("sentinel"))
        .unwrap()
        .write_all(b"keep existing folder")
        .unwrap();
    assert_eq!(
        quarantine.initialize(&parent).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert!(!quarantine.created());
    drop(quarantine);
    assert_eq!(
        std::fs::read(collision.join("sentinel")).unwrap(),
        b"keep existing folder"
    );
}

// rustix exposes mkfifoat on Linux; the macOS check covers no-follow symlinks.
#[cfg(target_os = "linux")]
#[test]
fn native_export_cleanup_gap_fifo_is_restored_without_blocking_or_deleting_it() {
    use std::os::unix::fs::FileTypeExt;
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let moved = folders.private.join("actual-writer.json");
    let mut partial = folders.owned(&target);
    let result = partial.cleanup_with(|point| {
        if point == files::CleanupPoint::BeforeMove {
            std::fs::rename(&target, &moved)?;
            rustix::fs::mkfifoat(
                rustix::fs::CWD,
                &target,
                rustix::fs::Mode::from_raw_mode(0o600),
            )?;
        }
        Ok(())
    });
    assert!(result.is_err());
    assert!(
        std::fs::symlink_metadata(&target)
            .unwrap()
            .file_type()
            .is_fifo()
    );
    assert!(moved.exists());
    assert!(partial.file.is_some());
}

#[cfg(unix)]
#[test]
fn native_export_cleanup_refuses_a_parent_that_lost_private_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let mut partial = folders.owned(&target);
    std::fs::set_permissions(&folders.private, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(partial.cleanup().is_err());
    assert!(target.exists());
    assert_eq!(std::fs::read_dir(&folders.private).unwrap().count(), 1);
    std::fs::set_permissions(&folders.private, std::fs::Permissions::from_mode(0o700)).unwrap();
    partial.cleanup().unwrap();
    assert!(!target.exists());
}

#[cfg(unix)]
#[test]
fn native_export_unlinked_writer_with_empty_quarantine_can_retry_cleanup() {
    use std::os::unix::fs::MetadataExt;
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let mut partial = folders.owned(&target);
    let result = partial.cleanup_with(|point| {
        if point == files::CleanupPoint::BeforeMove {
            std::fs::remove_file(&target)?;
        }
        Ok(())
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
    let quarantine = partial.quarantine_path().unwrap().to_path_buf();
    assert!(quarantine.exists());
    assert_eq!(std::fs::read_dir(&quarantine).unwrap().count(), 0);
    assert_eq!(
        partial.file.as_ref().unwrap().metadata().unwrap().nlink(),
        0
    );
    partial.cleanup().unwrap();
    assert!(partial.file.is_none());
    assert!(partial.quarantine_path().is_none());
    assert!(!quarantine.exists());
    assert!(!target.exists());
    assert_eq!(std::fs::read_dir(&folders.private).unwrap().count(), 0);
    partial.cleanup().unwrap();
}

#[cfg(unix)]
#[test]
fn native_export_unlinked_writer_preserves_nonempty_quarantine_until_safe_retry() {
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let mut partial = folders.owned(&target);
    assert!(
        partial
            .cleanup_with(|point| {
                if point == files::CleanupPoint::BeforeMove {
                    std::fs::remove_file(&target)?;
                }
                Ok(())
            })
            .is_err()
    );
    let quarantine = partial.quarantine_path().unwrap().to_path_buf();
    let foreign = quarantine.join("foreign.json");
    drmr_core::native::create_private_new(&foreign)
        .unwrap()
        .write_all(b"preserve foreign content")
        .unwrap();
    assert!(partial.cleanup().is_err());
    assert!(partial.file.is_some());
    assert_eq!(partial.quarantine_path().unwrap(), quarantine);
    assert_eq!(
        std::fs::read(&foreign).unwrap(),
        b"preserve foreign content"
    );
    // The fixture's owner moves its sentinel; the exporter must not remove it.
    let preserved = folders.private.join("preserved-foreign.json");
    std::fs::rename(&foreign, &preserved).unwrap();
    partial.cleanup().unwrap();
    assert!(partial.file.is_none());
    assert!(!quarantine.exists());
    assert_eq!(
        std::fs::read(&preserved).unwrap(),
        b"preserve foreign content"
    );
}

#[cfg(unix)]
#[test]
fn native_export_unlinked_writer_does_not_adopt_a_replaced_quarantine() {
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let mut partial = folders.owned(&target);
    assert!(
        partial
            .cleanup_with(|point| {
                if point == files::CleanupPoint::BeforeMove {
                    std::fs::remove_file(&target)?;
                }
                Ok(())
            })
            .is_err()
    );
    let quarantine = partial.quarantine_path().unwrap().to_path_buf();
    let moved = folders.private.join("retained-owned-quarantine");
    std::fs::rename(&quarantine, &moved).unwrap();
    drmr_core::native::create_private_dir_new(&quarantine).unwrap();
    let foreign = quarantine.join("sentinel");
    drmr_core::native::create_private_new(&foreign)
        .unwrap()
        .write_all(b"unfamiliar folder")
        .unwrap();
    assert!(partial.cleanup().is_err());
    assert!(partial.file.is_some());
    assert!(moved.exists());
    assert_eq!(std::fs::read(&foreign).unwrap(), b"unfamiliar folder");
    // Restore only the fixture-owned namespace, then retry the same owner.
    let preserved = folders.private.join("preserved-replacement");
    std::fs::rename(&quarantine, &preserved).unwrap();
    std::fs::rename(&moved, &quarantine).unwrap();
    partial.cleanup().unwrap();
    assert!(partial.file.is_none());
    assert!(!quarantine.exists());
    assert_eq!(
        std::fs::read(preserved.join("sentinel")).unwrap(),
        b"unfamiliar folder"
    );
}

#[test]
fn native_export_replaced_leaf_is_preserved_during_cleanup() {
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let mut partial = folders.owned(&target);
    std::fs::rename(&target, folders.private.join("owned-moved.json")).unwrap();
    let mut foreign = drmr_core::native::create_private_new(&target).unwrap();
    foreign.write_all(b"foreign leaf").unwrap();
    assert!(partial.cleanup().is_err());
    assert_eq!(std::fs::read(target).unwrap(), b"foreign leaf");
}

#[test]
fn native_export_replaced_parent_is_preserved_or_pinned_against_rename() {
    let folders = Folders::new();
    let target = folders.private.join("partial.json");
    let mut partial = folders.owned(&target);
    let moved = folders.root.join("moved-private");
    #[cfg(windows)]
    {
        assert!(std::fs::rename(&folders.private, &moved).is_err());
        assert!(partial.cleanup().is_ok());
    }
    #[cfg(unix)]
    {
        std::fs::rename(&folders.private, &moved).unwrap();
        drmr_core::native::create_private_dir_new(&folders.private).unwrap();
        let mut foreign = drmr_core::native::create_private_new(&target).unwrap();
        foreign.write_all(b"foreign parent").unwrap();
        assert!(partial.cleanup().is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"foreign parent");
        assert!(moved.join("partial.json").exists());
    }
}

#[cfg(unix)]
#[test]
fn native_export_alias_into_media_and_retargeted_parent_are_refused() {
    use std::os::unix::fs::symlink;
    let folders = Folders::new();
    let media = MediaFolders::capture(&folders.input, &folders.output).unwrap();
    let alias = folders.root.join("alias");
    symlink(&folders.input, &alias).unwrap();
    assert!(Destination::capture(&alias.join("leak.json"), &media).is_err());
    std::fs::remove_file(&alias).unwrap();
    symlink(&folders.private, &alias).unwrap();
    let destination = Destination::capture(&alias.join("late.json"), &media).unwrap();
    std::fs::remove_file(&alias).unwrap();
    symlink(&folders.input, &alias).unwrap();
    assert!(destination.create(&media).is_err());
    assert!(!folders.input.join("late.json").exists());
}
