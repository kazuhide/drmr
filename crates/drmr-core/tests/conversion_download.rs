//! Remote acquisition preserves source bytes and publishes only a complete local graph.
use drmr_core::{
    concurrency::Stop,
    conversion::{ConversionRequest, Converter, HlsInputKeys, InputKeys},
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    source::FetchPolicy,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    collections::BTreeMap,
    io::{Read as _, Write as _},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

struct Server {
    address: std::net::SocketAddr,
    calls: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    held: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Server {
    fn new(resources: BTreeMap<&'static str, Vec<u8>>) -> Self {
        Self::redirecting(resources, BTreeMap::new())
    }
    fn redirecting(
        resources: BTreeMap<&'static str, Vec<u8>>,
        redirects: BTreeMap<&'static str, &'static str>,
    ) -> Self {
        Self::scripted(
            resources
                .into_iter()
                .map(|(name, body)| (name, vec![body]))
                .collect(),
            redirects,
        )
    }
    fn scripted(
        mut resources: BTreeMap<&'static str, Vec<Vec<u8>>>,
        redirects: BTreeMap<&'static str, &'static str>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let held = Arc::new(AtomicBool::new(false));
        let ready = held.clone();
        let (ending, recorded) = (stop.clone(), calls.clone());
        let thread = std::thread::spawn(move || {
            for socket in listener.incoming() {
                if ending.load(Ordering::Acquire) {
                    break;
                }
                let mut socket = socket.unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    if socket.read(&mut byte).unwrap_or(0) == 0 {
                        break;
                    }
                    request.push(byte[0]);
                    assert!(request.len() <= 8192);
                }
                let target = std::str::from_utf8(&request)
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                recorded.lock().unwrap().push(target.clone());
                if let Some(location) = redirects.get(target.as_str()) {
                    write!(socket, "HTTP/1.1 302 Found\r\nContent-Length: 0\r\nLocation: {location}\r\nConnection: close\r\n\r\n").unwrap();
                    continue;
                }
                let (status, body) = resources.get_mut(target.as_str()).map_or(
                    ("404 Not Found", Vec::new()),
                    |bodies| {
                        let body = if bodies.len() > 1 {
                            bodies.remove(0)
                        } else {
                            bodies[0].clone()
                        };
                        ("200 OK", body)
                    },
                );
                write!(
                    socket,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                if target == "/held.ts" {
                    socket.flush().unwrap();
                    ready.store(true, Ordering::Release);
                    while !ending.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    break;
                }
                socket.write_all(&body).unwrap();
            }
        });
        Self {
            address,
            calls,
            stop,
            held,
            thread: Some(thread),
        }
    }
    fn url(&self) -> String {
        format!("http://{}/source.m3u8", self.address)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn converter(stop: Stop) -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(NoSink),
        )),
        "unused-packager",
        "unused-ffmpeg",
    )
    .with_stop(stop)
    .with_max_workers(Some(1))
    .unwrap()
}
fn policy() -> FetchPolicy {
    FetchPolicy {
        allow_http: true,
        allow_private_network: true,
        attempts: 1,
        ..FetchPolicy::default()
    }
}
#[test]
fn raw_manifest_and_media_are_retained_beside_a_distinct_playable_projection() {
    let manifest = b"#EXTM3U\r\n#EXT-X-TARGETDURATION:3\r\n#EXT-X-MEDIA-SEQUENCE:37\r\n#EXTINF:2.000000000,\r\nsegment.ts?public=value\r\n#EXT-X-ENDLIST\r\n";
    let media = vec![0x47; 188];
    let server = Server::new(BTreeMap::from([
        ("/source.m3u8", manifest.to_vec()),
        ("/segment.ts?public=value", media.clone()),
    ]));
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().canonicalize().unwrap().join("download");
    let source = converter(Stop::new())
        .download_hls(
            &server.url(),
            &directory,
            &parent.path().join("output"),
            &policy(),
        )
        .unwrap();
    assert_eq!(source.entrypoint(), directory.join("input.m3u8"));
    assert_eq!(
        std::fs::read(directory.join("raw/r000000.m3u8")).unwrap(),
        manifest
    );
    assert_eq!(
        std::fs::read(directory.join("media/r000000.ts")).unwrap(),
        media
    );
    let projected = std::fs::read_to_string(source.entrypoint()).unwrap();
    assert_eq!(
        projected,
        std::str::from_utf8(manifest)
            .unwrap()
            .replace("segment.ts?public=value", "media/r000000.ts")
    );
    let debug = format!("{source:?}");
    assert!(!debug.contains(&server.url()) && !debug.contains(directory.to_str().unwrap()));
}
#[test]
fn existing_destination_is_refused_without_a_network_request_or_changes() {
    let server = Server::new(BTreeMap::new());
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().canonicalize().unwrap().join("download");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("sentinel"), b"original").unwrap();
    assert!(
        converter(Stop::new())
            .download_hls(
                &server.url(),
                &directory,
                &parent.path().join("output"),
                &policy()
            )
            .is_err()
    );
    assert!(server.calls.lock().unwrap().is_empty());
    assert_eq!(
        std::fs::read(directory.join("sentinel")).unwrap(),
        b"original"
    );
}
#[test]
fn encrypted_playlists_are_refused_before_any_key_or_media_request() {
    let manifest=b"#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n";
    let server = Server::new(BTreeMap::from([("/source.m3u8", manifest.to_vec())]));
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().canonicalize().unwrap().join("download");
    assert!(
        converter(Stop::new())
            .download_hls(
                &server.url(),
                &directory,
                &parent.path().join("output"),
                &policy()
            )
            .is_err()
    );
    assert_eq!(*server.calls.lock().unwrap(), ["/source.m3u8"]);
    assert!(!directory.exists());
}

#[test]
fn cancellation_drains_a_real_held_http_body_and_removes_only_the_partial_download() {
    let manifest = b"#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:2,\nheld.ts\n#EXT-X-ENDLIST\n";
    let server = Server::new(BTreeMap::from([
        ("/source.m3u8", manifest.to_vec()),
        ("/held.ts", vec![0x47; 188]),
    ]));
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().canonicalize().unwrap().join("download");
    let stop = Stop::new();
    let c = converter(stop.clone());
    let url = server.url();
    let target = directory.clone();
    let (send, receive) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = c
            .download_hls(&url, &target, &target.with_extension("output"), &policy())
            .map(|_| ());
        send.send(result).unwrap();
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !server.held.load(Ordering::Acquire) {
        assert!(std::time::Instant::now() < deadline, "HTTP body readiness");
        std::thread::sleep(Duration::from_millis(5));
    }
    stop.set();
    let result = receive.recv_timeout(Duration::from_secs(2));
    drop(server);
    worker.join().unwrap();
    assert_eq!(result.unwrap().unwrap_err().0, "cancelled");
    assert!(!directory.exists());
    assert_eq!(std::fs::read_dir(parent.path()).unwrap().count(), 0);
}

#[test]
fn graph_resource_budget_counts_queued_media_before_any_media_request() {
    let manifest =
        b"#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n";
    let server = Server::new(BTreeMap::from([("/source.m3u8", manifest.to_vec())]));
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("download");
    let mut policy = policy();
    policy.max_total_resources = Some(2);
    assert!(
        converter(Stop::new())
            .download_hls(
                &server.url(),
                &directory,
                &parent.path().join("output"),
                &policy
            )
            .is_err()
    );
    assert_eq!(*server.calls.lock().unwrap(), ["/source.m3u8"]);
    assert!(!directory.exists());
}

#[test]
fn cached_requested_or_effective_master_cannot_be_used_as_its_own_media_child() {
    for reference in ["source.m3u8", "effective.m3u8"] {
        let master = format!("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\n{reference}\n");
        let server = Server::redirecting(
            BTreeMap::from([("/effective.m3u8", master.into_bytes())]),
            BTreeMap::from([("/source.m3u8", "/effective.m3u8")]),
        );
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join("download");
        assert!(
            converter(Stop::new())
                .download_hls(
                    &server.url(),
                    &directory,
                    &parent.path().join("output"),
                    &policy()
                )
                .is_err()
        );
        assert_eq!(
            *server.calls.lock().unwrap(),
            ["/source.m3u8", "/effective.m3u8"]
        );
        assert!(!directory.exists());
    }
}

#[test]
fn redirected_root_aliases_do_not_consume_extra_playlist_budget() {
    let master = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\nchild.m3u8?query=kept\n";
    let child = b"#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n";
    let server = Server::redirecting(
        BTreeMap::from([
            ("/effective.m3u8", master.to_vec()),
            ("/child.m3u8?query=kept", child.to_vec()),
            ("/segment.ts", vec![0x47; 188]),
        ]),
        BTreeMap::from([("/source.m3u8", "/effective.m3u8")]),
    );
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("download");
    let mut policy = policy();
    policy.max_playlists = 2;
    policy.max_total_resources = Some(3);
    let source = converter(Stop::new())
        .download_hls(
            &server.url(),
            &directory,
            &parent.path().join("output"),
            &policy,
        )
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(source.entrypoint()).unwrap(),
        std::str::from_utf8(master)
            .unwrap()
            .replace("child.m3u8?query=kept", "p000001.m3u8")
    );
    assert_eq!(
        *server.calls.lock().unwrap(),
        [
            "/source.m3u8",
            "/effective.m3u8",
            "/child.m3u8?query=kept",
            "/segment.ts"
        ]
    );
}

#[test]
fn existing_output_and_physical_overlap_are_refused_before_any_request() {
    let server = Server::new(BTreeMap::new());
    let parent = tempfile::tempdir().unwrap();
    let output = parent.path().join("output");
    std::fs::write(&output, b"sentinel").unwrap();
    let directory = parent.path().join("download");
    let c = converter(Stop::new());
    assert_eq!(
        c.download_hls(&server.url(), &directory, &output, &policy())
            .unwrap_err()
            .0,
        "output-exists-or-unsafe"
    );
    assert!(!directory.exists());
    assert_eq!(std::fs::read(&output).unwrap(), b"sentinel");
    assert_eq!(
        c.download_hls(&server.url(), &directory, &directory, &policy())
            .unwrap_err()
            .0,
        "download-output-overlap"
    );
    assert!(!directory.exists());
    assert!(server.calls.lock().unwrap().is_empty());
}

#[test]
fn changed_or_same_bytes_replaced_originals_are_rejected_without_losing_the_saved_source() {
    for replace in [false, true] {
        let manifest =
            b"#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n";
        let server = Server::new(BTreeMap::from([
            ("/source.m3u8", manifest.to_vec()),
            ("/segment.ts", vec![0x47; 188]),
        ]));
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join("download");
        let output = parent.path().join("output");
        let c = converter(Stop::new());
        let source = c
            .download_hls(&server.url(), &directory, &output, &policy())
            .unwrap();
        let original = directory.join("media/r000000.ts");
        if replace {
            std::fs::rename(&original, directory.join("media/moved.ts")).unwrap();
            drmr_core::native::create_private_new(&original)
                .unwrap()
                .write_all(&[0x47; 188])
                .unwrap();
            std::fs::remove_file(directory.join("media/moved.ts")).unwrap();
        } else {
            std::fs::write(&original, b"changed bytes").unwrap();
        }
        let keys = InputKeys::default();
        let hls = HlsInputKeys::default();
        let error = c
            .convert_downloaded(
                &source,
                ConversionRequest {
                    input: source.entrypoint(),
                    output: &output,
                    format: OutputFormat::Hls,
                    target: TargetProtection::Clear,
                    input_keys: &keys,
                    hls_input_keys: &hls,
                    output_key: None,
                    hls_key_uri: None,
                    hls_output_key: None,
                },
            )
            .unwrap_err();
        assert_eq!(error.0, "download-source-changed");
        assert!(!output.exists());
        assert_eq!(
            std::fs::read(directory.join("raw/r000000.m3u8")).unwrap(),
            manifest
        );
        assert!(directory.is_dir());
    }
}

#[test]
fn effective_aliases_must_agree_on_original_bytes_before_the_graph_is_admitted() {
    let master = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000\na.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=1001\nb.m3u8\n";
    let child = b"#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n";
    for changed in [false, true] {
        let second = if changed {
            std::str::from_utf8(child)
                .unwrap()
                .replace("EXTINF:2,", "EXTINF:2.1,")
                .into_bytes()
        } else {
            child.to_vec()
        };
        let server = Server::scripted(
            BTreeMap::from([
                ("/source.m3u8", vec![master.to_vec()]),
                ("/effective.m3u8", vec![child.to_vec(), second]),
                ("/segment.ts", vec![vec![0x47; 188]]),
            ]),
            BTreeMap::from([
                ("/a.m3u8", "/effective.m3u8"),
                ("/b.m3u8", "/effective.m3u8"),
            ]),
        );
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join("download");
        let result = converter(Stop::new()).download_hls(
            &server.url(),
            &directory,
            &parent.path().join("output"),
            &policy(),
        );
        if changed {
            assert_eq!(result.unwrap_err().0, "download-source-not-supported");
            assert!(!directory.exists());
            assert!(
                !server
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|uri| uri == "/segment.ts")
            );
        } else {
            let source = result.unwrap();
            assert_eq!(
                std::fs::read_to_string(source.entrypoint()).unwrap(),
                std::str::from_utf8(master)
                    .unwrap()
                    .replace("a.m3u8", "p000001.m3u8")
                    .replace("b.m3u8", "p000001.m3u8")
            );
            assert_eq!(
                std::fs::read(directory.join("raw/r000001.m3u8")).unwrap(),
                child
            );
            assert_eq!(
                server
                    .calls
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|uri| uri.as_str() == "/segment.ts")
                    .count(),
                1
            );
        }
    }
}

#[test]
fn custom_resource_capacity_is_honored_before_any_network_or_directory_writes() {
    let server = Server::new(BTreeMap::new());
    let parent = tempfile::tempdir().unwrap();
    let directory = parent.path().join("download");
    let runner = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink)).with_limiter(
        Arc::new(drmr_core::resources::Limiter::new(1, 128 * 1024 * 1024)),
    );
    let c = Converter::new(Arc::new(runner), "unused", "unused")
        .with_max_workers(Some(1))
        .unwrap();
    assert_eq!(
        c.download_hls(
            &server.url(),
            &directory,
            &parent.path().join("output"),
            &policy()
        )
        .unwrap_err()
        .0,
        "resource-budget"
    );
    assert!(server.calls.lock().unwrap().is_empty());
    assert!(!directory.exists());
}

#[test]
fn live_and_byte_range_profiles_fail_before_any_referenced_resource_is_read() {
    for manifest in [
        "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:2,\nsegment.ts\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXTINF:2,\n#EXT-X-BYTERANGE:188@0\nsegment.ts\n#EXT-X-ENDLIST\n",
        "#EXTM3U\n#EXT-X-TARGETDURATION:3\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"32@0\"\n#EXTINF:2,\nsegment.m4s\n#EXT-X-ENDLIST\n",
    ] {
        let server = Server::new(BTreeMap::from([(
            "/source.m3u8",
            manifest.as_bytes().to_vec(),
        )]));
        let parent = tempfile::tempdir().unwrap();
        let directory = parent.path().join("download");
        assert_eq!(
            converter(Stop::new())
                .download_hls(
                    &server.url(),
                    &directory,
                    &parent.path().join("output"),
                    &policy()
                )
                .unwrap_err()
                .0,
            "download-source-not-supported"
        );
        assert_eq!(*server.calls.lock().unwrap(), ["/source.m3u8"]);
        assert!(!directory.exists());
    }
}
