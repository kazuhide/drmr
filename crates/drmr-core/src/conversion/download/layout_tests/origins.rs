//! Public HTTP acquisition keeps origin authorization separate from path placement.
use super::*;
use crate::{
    events::NoSink,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    net::TcpListener,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

struct Server {
    origin: String,
    responses: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Server {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let responses = Arc::new(Mutex::new(BTreeMap::<String, Vec<u8>>::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let state = (responses.clone(), requests.clone(), stop.clone());
        let worker = thread::spawn(move || {
            while !state.2.load(Ordering::Acquire) {
                let (mut connection, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                };
                connection.set_nonblocking(false).unwrap();
                connection
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    assert!(request.len() < 8192);
                    let mut byte = [0];
                    connection.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                }
                let request = std::str::from_utf8(&request).unwrap();
                let target = request.split_whitespace().nth(1).unwrap();
                state.1.lock().unwrap().push(target.to_owned());
                let body = state.0.lock().unwrap().get(target).cloned();
                let status = if body.is_some() {
                    "200 OK"
                } else {
                    "404 Not Found"
                };
                let body = body.unwrap_or_default();
                write!(
                    connection,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                connection.write_all(&body).unwrap();
            }
        });
        Self {
            origin,
            responses,
            requests,
            stop,
            worker: Some(worker),
        }
    }
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }
    fn set(&self, path: &str, bytes: &[u8]) {
        self.responses
            .lock()
            .unwrap()
            .insert(path.into(), bytes.into());
    }
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let result = self.worker.take().unwrap().join();
        if !thread::panicking() {
            assert!(result.is_ok(), "HTTP fixture must finish cleanly");
        }
    }
}

fn command() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "must-not-run",
        "must-not-run",
    )
    .with_new_output_defaults()
    .with_max_workers(Some(2))
    .unwrap()
}
fn policy(servers: &[&Server]) -> FetchPolicy {
    FetchPolicy {
        allow_http: true,
        allow_private_network: true,
        allowed_origins: servers.iter().map(|server| server.origin.clone()).collect(),
        attempts: 1,
        timeout: 2.0,
        ..FetchPolicy::default()
    }
}
fn hls(root: &Server, other: &Server, media: &[String]) -> (Vec<u8>, Vec<u8>) {
    let master = format!(
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\n{}\n",
        other.url("/vod/video/720p/media.m3u8")
    )
    .into_bytes();
    let mut child = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n".to_vec();
    for url in media {
        child.extend_from_slice(format!("#EXTINF:6,\n{url}\n").as_bytes());
    }
    child.extend_from_slice(b"#EXT-X-ENDLIST\n");
    root.set("/vod/master.m3u8", &master);
    other.set("/vod/video/720p/media.m3u8", &child);
    (master, child)
}
fn assert_cleanup(parent: &Path, destination: &Path) {
    assert!(!destination.exists());
    assert_eq!(
        std::fs::read(parent.join("foreign.txt")).unwrap(),
        b"unrelated bytes"
    );
    assert_eq!(std::fs::read_dir(parent).unwrap().count(), 1);
}
fn assert_private(root: &Path) {
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_private(&path);
        } else {
            drop(crate::native::open_private_read(&path).unwrap());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let expected = if path.is_dir() { 0o700 } else { 0o600 };
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                expected
            );
        }
    }
}

#[test]
fn approved_other_origin_keeps_hls_hierarchy_raw_bytes_hashes_and_private_seal() {
    let parent = crate::native::private_tempdir().unwrap();
    let root = Server::start();
    let other = Server::start();
    let media_path = "/vod/video/720p/seg_0.ts";
    let media = b"retained transport bytes";
    let (master, child) = hls(&root, &other, &[other.url(media_path)]);
    other.set(media_path, media);
    let retained = command()
        .acquire_source(
            &root.url("/vod/master.m3u8"),
            &parent.path().join("source"),
            &policy(&[&root, &other]),
        )
        .unwrap();
    let directory = retained.directory();
    let relative = Path::new("video/720p/seg_0.ts");
    assert_eq!(std::fs::read(directory.join(relative)).unwrap(), media);
    assert_eq!(retained.seal.files[relative], bytes_digest(media));
    assert_eq!(
        std::fs::read(directory.join("raw/r000000.m3u8")).unwrap(),
        master
    );
    assert_eq!(
        std::fs::read(directory.join("raw/r000001.m3u8")).unwrap(),
        child
    );
    assert!(
        std::fs::read_to_string(retained.entrypoint())
            .unwrap()
            .contains("\nvideo/720p/media.m3u8\n")
    );
    let local = std::fs::read_to_string(directory.join("video/720p/media.m3u8")).unwrap();
    assert!(local.contains("\nseg_0.ts\n"));
    assert!(!local.contains("http:"));
    assert_private(directory);
    retained.check(&Stop::new()).unwrap();
    assert_eq!(root.requests(), ["/vod/master.m3u8"]);
    assert_eq!(other.requests(), ["/vod/video/720p/media.m3u8", media_path]);
    std::fs::write(directory.join(relative), b"modified transport bytes").unwrap();
    assert_eq!(retained.check(&Stop::new()).unwrap_err(), CHANGED);
}

#[test]
fn approved_other_origin_keeps_dash_resources_and_original_manifest() {
    let parent = crate::native::private_tempdir().unwrap();
    let root = Server::start();
    let other = Server::start();
    let manifest = format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT6S"><Period><AdaptationSet contentType="video"><Representation id="v" bandwidth="1000" mimeType="video/mp4" codecs="avc1.64001f"><BaseURL>{}/vod/</BaseURL><SegmentTemplate timescale="1" duration="6" initialization="video/720p/init.mp4" media="video/720p/seg_$Number$.m4s" startNumber="1"/></Representation></AdaptationSet></Period></MPD>"#,
        other.origin
    );
    root.set("/vod/input.mpd", manifest.as_bytes());
    let resources = [
        ("video/720p/init.mp4", b"initialization bytes".as_slice()),
        ("video/720p/seg_1.m4s", b"media bytes".as_slice()),
    ];
    for (path, bytes) in resources {
        other.set(&format!("/vod/{path}"), bytes);
    }
    let retained = command()
        .acquire_source(
            &root.url("/vod/input.mpd"),
            &parent.path().join("source"),
            &policy(&[&root, &other]),
        )
        .unwrap();
    let directory = retained.directory();
    for (path, bytes) in resources {
        assert_eq!(std::fs::read(directory.join(path)).unwrap(), bytes);
        assert_eq!(retained.seal.files[Path::new(path)], bytes_digest(bytes));
    }
    assert_eq!(
        std::fs::read(directory.join("raw/r000000.mpd")).unwrap(),
        manifest.as_bytes()
    );
    let local = std::fs::read_to_string(retained.entrypoint()).unwrap();
    assert!(local.contains("video/720p/init.mp4"));
    assert!(local.contains("video/720p/seg_1.m4s"));
    assert!(!local.contains(&other.origin));
    assert_private(directory);
    retained.check(&Stop::new()).unwrap();
    assert_eq!(root.requests(), ["/vod/input.mpd"]);
    let mut requested = other.requests();
    requested.sort();
    assert_eq!(
        requested,
        ["/vod/video/720p/init.mp4", "/vod/video/720p/seg_1.m4s"]
    );
}

#[test]
fn unapproved_other_origin_is_not_contacted_and_its_owned_download_is_cleaned() {
    for child_is_local in [false, true] {
        let parent = crate::native::private_tempdir().unwrap();
        std::fs::write(parent.path().join("foreign.txt"), b"unrelated bytes").unwrap();
        let destination = parent.path().join("source");
        let root = Server::start();
        let other = Server::start();
        let (_, child) = hls(&root, &other, &[other.url("/vod/video/720p/seg_0.ts")]);
        if child_is_local {
            root.set(
                "/vod/master.m3u8",
                b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nvideo/720p/media.m3u8\n",
            );
            root.set("/vod/video/720p/media.m3u8", &child);
        }
        assert_eq!(
            command()
                .acquire_source(
                    &root.url("/vod/master.m3u8"),
                    &destination,
                    &policy(&[&root])
                )
                .unwrap_err(),
            IO
        );
        assert_eq!(other.requests(), Vec::<String>::new());
        assert_eq!(root.requests().len(), if child_is_local { 2 } else { 1 });
        assert_cleanup(parent.path(), &destination);
    }
}

#[test]
fn different_origins_with_aliasing_paths_are_refused_before_media_download() {
    for first in ["seg_0.ts", "SEG_0.ts", "%73eg_0.ts"] {
        let parent = crate::native::private_tempdir().unwrap();
        std::fs::write(parent.path().join("foreign.txt"), b"unrelated bytes").unwrap();
        let destination = parent.path().join("source");
        let root = Server::start();
        let other = Server::start();
        hls(
            &root,
            &other,
            &[
                root.url(&format!("/vod/video/720p/{first}")),
                other.url("/vod/video/720p/seg_0.ts"),
            ],
        );
        assert_eq!(
            command()
                .acquire_source(
                    &root.url("/vod/master.m3u8"),
                    &destination,
                    &policy(&[&root, &other])
                )
                .unwrap_err(),
            ConversionError("source-layout-collision")
        );
        assert_eq!(root.requests(), ["/vod/master.m3u8"]);
        assert_eq!(other.requests(), ["/vod/video/720p/media.m3u8"]);
        assert_cleanup(parent.path(), &destination);
    }
}

#[test]
fn an_approved_origin_cannot_alias_a_query_variant_or_leave_partial_files() {
    let parent = crate::native::private_tempdir().unwrap();
    std::fs::write(parent.path().join("foreign.txt"), b"unrelated bytes").unwrap();
    let destination = parent.path().join("source");
    let root = Server::start();
    let other = Server::start();
    hls(
        &root,
        &other,
        &[
            other.url("/vod/video/720p/seg_0.ts?token=one"),
            other.url("/vod/video/720p/seg_0.ts?token=two"),
        ],
    );
    assert_eq!(
        command()
            .acquire_source(
                &root.url("/vod/master.m3u8"),
                &destination,
                &policy(&[&root, &other])
            )
            .unwrap_err(),
        ConversionError("source-layout-collision")
    );
    assert_eq!(other.requests(), ["/vod/video/720p/media.m3u8"]);
    assert_cleanup(parent.path(), &destination);
}

#[test]
fn an_approved_origin_still_cannot_store_paths_outside_the_entrypoint_base() {
    for outside_child in [false, true] {
        let parent = crate::native::private_tempdir().unwrap();
        std::fs::write(parent.path().join("foreign.txt"), b"unrelated bytes").unwrap();
        let destination = parent.path().join("source");
        let root = Server::start();
        let other = Server::start();
        let (_, child) = hls(
            &root,
            &other,
            &[other.url("/vod-other/video/720p/seg_0.ts")],
        );
        let requested = if outside_child {
            let path = "/outside/video/720p/media.m3u8";
            other.set(path, &child);
            root.set(
                "/vod/master.m3u8",
                format!(
                    "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\n{}\n",
                    other.url(path)
                )
                .as_bytes(),
            );
            path
        } else {
            "/vod/video/720p/media.m3u8"
        };
        assert_eq!(
            command()
                .acquire_source(
                    &root.url("/vod/master.m3u8"),
                    &destination,
                    &policy(&[&root, &other])
                )
                .unwrap_err(),
            ConversionError("source-layout-not-supported")
        );
        assert_eq!(other.requests(), [requested]);
        assert_cleanup(parent.path(), &destination);
    }
}

#[test]
fn retained_http_source_shrinks_memory_keeps_no_cpu_and_lives_through_converter_clones() {
    use crate::resources::{Claim, Limiter, ReservationError};
    let root = crate::native::private_tempdir().unwrap();
    let server = Server::start();
    server.set(
        "/vod/media.m3u8",
        b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nvideo/seg.ts\n#EXT-X-ENDLIST\n",
    );
    server.set("/vod/video/seg.ts", b"original encrypted resource bytes");
    let limiter = Arc::new(Limiter::new(1, 512 * 1024 * 1024));
    let runner = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)).with_limiter(limiter.clone()),
    );
    let parent = runner
        .retain_context(
            64 * 1024 * 1024,
            128 * 1024 * 1024,
            &Stop::new(),
            Duration::from_secs(1),
        )
        .unwrap();
    let nested = Arc::new(runner.within_context(64 * 1024 * 1024, 128 * 1024 * 1024));
    let baseline = 448 * 1024 * 1024;
    let mut command = Converter::new(nested, "must-not-run", "must-not-run")
        .with_new_output_defaults()
        .with_max_workers(Some(1))
        .unwrap();
    let source = command
        .acquire_source(
            &server.url("/vod/media.m3u8"),
            &root.path().join("source"),
            &policy(&[&server]),
        )
        .unwrap();
    let bytes = source.seal.retained_bytes;
    assert!(
        bytes > 0 && bytes < 512 * 1024,
        "ordinary sources release unused graph capacity"
    );
    assert_eq!(runner.retained_capacity(), (1, baseline - bytes));
    let cpu = runner
        .native_permit(
            Claim {
                cpu_threads: 1,
                memory_bytes: 1,
            },
            &Stop::new(),
            Duration::from_secs(1),
        )
        .unwrap();
    drop(cpu);
    let child = command.using_downloaded_source(&source).unwrap();
    let repeated = child.using_downloaded_source(&source).unwrap();
    assert_eq!(
        child.backend.runner.native_capacity(),
        repeated.backend.runner.native_capacity()
    );
    assert_eq!(child.backend.runner.native_capacity().1, baseline - bytes);
    let error = child
        .backend
        .runner
        .native_permit(
            Claim {
                cpu_threads: 1,
                memory_bytes: baseline,
            },
            &Stop::new(),
            Duration::from_secs(1),
        )
        .unwrap_err();
    assert_eq!(error, ReservationError::Insufficient);
    let other = command
        .acquire_source(
            &server.url("/vod/media.m3u8"),
            &root.path().join("other"),
            &policy(&[&server]),
        )
        .unwrap();
    assert_eq!(
        child.using_downloaded_source(&other).err().unwrap(),
        CHANGED
    );
    drop(other);
    drop(repeated);
    drop(child);
    source.check(&Stop::new()).unwrap();
    command.downloaded = Some(source.seal.clone());
    drop(source);
    assert_eq!(runner.retained_capacity().1, baseline - bytes);
    drop(command);
    assert_eq!(runner.retained_capacity(), (1, baseline));
    drop(parent);
    assert_eq!(runner.retained_capacity(), (1, 512 * 1024 * 1024));
}

#[test]
fn bound_analysis_and_single_plan_refuse_copied_bytes_that_differ_from_the_acquisition() {
    use crate::{
        conversion::{HlsInputKeys, InputKeys, VerificationSource, plan::ExecutionPlanRequest},
        inspection::{OutputFormat, TargetProtection},
    };
    let root = crate::native::private_tempdir().unwrap();
    let server = Server::start();
    server.set(
        "/vod/media.m3u8",
        b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nvideo/seg.ts\n#EXT-X-ENDLIST\n",
    );
    server.set("/vod/video/seg.ts", b"immutable acquired bytes");
    let original = command();
    let source = original
        .acquire_source(
            &server.url("/vod/media.m3u8"),
            &root.path().join("source"),
            &policy(&[&server]),
        )
        .unwrap();
    let bound = original.using_downloaded_source(&source).unwrap();
    std::fs::write(
        source.directory().join("video/seg.ts"),
        b"substituted copied bytes",
    )
    .unwrap();
    let input = VerificationSource {
        input: source.entrypoint(),
        input_keys: &InputKeys::default(),
        hls_keys: &HlsInputKeys::default(),
    };
    let analysis_error = bound.analyze_source(input).unwrap_err();
    let planning_error = bound
        .create_execution_plan(ExecutionPlanRequest {
            input,
            output: &root.path().join("output"),
            format: Some(OutputFormat::HlsTs),
            target: TargetProtection::Clear,
        })
        .unwrap_err();
    assert_eq!((analysis_error, planning_error), (CHANGED, CHANGED));
    assert!(!root.path().join("output").exists());
}

#[test]
fn cancellation_while_waiting_for_cpu_returns_the_source_context() {
    use crate::resources::{Claim, Limiter};
    let root = crate::native::private_tempdir().unwrap();
    let server = Server::start();
    let runner = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
            .with_limiter(Arc::new(Limiter::new(1, 256 * 1024 * 1024))),
    );
    let held_cpu = runner
        .native_permit(
            Claim {
                cpu_threads: 1,
                memory_bytes: 1,
            },
            &Stop::new(),
            Duration::from_secs(1),
        )
        .unwrap();
    let command = Converter::new(runner.clone(), "must-not-run", "must-not-run")
        .with_new_output_defaults()
        .with_max_workers(Some(1))
        .unwrap();
    let stop = command.backend.stop.clone();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            command.acquire_source(
                &server.url("/vod/media.m3u8"),
                &root.path().join("source"),
                &policy(&[&server]),
            )
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while runner.retained_capacity().1 == 256 * 1024 * 1024
            && std::time::Instant::now() < deadline
        {
            std::thread::yield_now();
        }
        let observed = runner.retained_capacity().1 < 256 * 1024 * 1024;
        stop.set();
        assert_eq!(
            worker.join().unwrap().unwrap_err(),
            ConversionError("cancelled")
        );
        assert!(
            observed,
            "the source context must precede the queued CPU claim"
        );
    });
    assert_eq!(runner.retained_capacity(), (1, 256 * 1024 * 1024));
    assert_eq!(server.requests(), [] as [String; 0]);
    assert!(!root.path().join("source").exists());
    drop(held_cpu);
}

#[test]
fn failed_and_cancelled_http_acquisition_release_retained_memory_and_cpu() {
    use crate::resources::{Claim, Limiter};
    let root = crate::native::private_tempdir().unwrap();
    let server = Server::start();
    let runner = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
            .with_limiter(Arc::new(Limiter::new(1, 256 * 1024 * 1024))),
    );
    for cancelled in [false, true] {
        let command = Converter::new(runner.clone(), "must-not-run", "must-not-run")
            .with_new_output_defaults()
            .with_max_workers(Some(1))
            .unwrap();
        if cancelled {
            command.backend.stop.set();
        }
        assert!(
            command
                .acquire_source(
                    &server.url("/vod/missing.m3u8"),
                    &root
                        .path()
                        .join(if cancelled { "cancelled" } else { "failed" }),
                    &policy(&[&server])
                )
                .is_err()
        );
        assert_eq!(runner.retained_capacity(), (1, 256 * 1024 * 1024));
        runner
            .native_permit(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: 256 * 1024 * 1024,
                },
                &Stop::new(),
                Duration::from_secs(1),
            )
            .unwrap();
    }
}
