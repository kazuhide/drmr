//! Key-free remote metadata uses bounded policy-controlled requests, never media bodies.
use drmr_core::{
    concurrency::Stop,
    conversion::Converter,
    events::{Collector, NoSink},
    inspection::{Evidence, InputFormat, Protection},
    source::FetchPolicy,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

fn atom(kind: [u8; 4], body: &[u8]) -> Vec<u8> {
    [
        &u32::try_from(body.len() + 8).unwrap().to_be_bytes()[..],
        &kind,
        body,
    ]
    .concat()
}
fn movie() -> Vec<u8> {
    let entry = atom(*b"avc1", &[0; 78]);
    let stsd = atom(
        *b"stsd",
        &[&[0; 4][..], &1_u32.to_be_bytes(), &entry].concat(),
    );
    atom(
        *b"moov",
        &atom(
            *b"trak",
            &atom(*b"mdia", &atom(*b"minf", &atom(*b"stbl", &stsd))),
        ),
    )
}
#[derive(Clone)]
enum Body {
    Bytes(Vec<u8>),
    Sparse { movie: Vec<u8>, offset: u64 },
    Truncated { bytes: Vec<u8>, declared: u64 },
}
impl Body {
    fn len(&self) -> u64 {
        match self {
            Self::Bytes(bytes) => bytes.len() as u64,
            Self::Sparse { movie, offset } => offset + movie.len() as u64,
            Self::Truncated { declared, .. } => *declared,
        }
    }
    fn range(&self, start: u64, end: u64) -> Vec<u8> {
        match self {
            Self::Bytes(bytes) => {
                bytes[usize::try_from(start).unwrap()..=usize::try_from(end).unwrap()].to_vec()
            }
            Self::Sparse { movie, offset } => {
                let header = [
                    &8_u32.to_be_bytes()[..],
                    b"ftyp",
                    &1_u32.to_be_bytes(),
                    b"mdat",
                    &(offset - 8).to_be_bytes(),
                ]
                .concat();
                if start < header.len() as u64 {
                    assert!(end < header.len() as u64);
                    header[usize::try_from(start).unwrap()..=usize::try_from(end).unwrap()].to_vec()
                } else {
                    assert!(start >= *offset, "media payload must never be requested");
                    movie[usize::try_from(start - offset).unwrap()
                        ..=usize::try_from(end - offset).unwrap()]
                        .to_vec()
                }
            }
            Self::Truncated { .. } => panic!("truncated fixture does not support ranges"),
        }
    }
}
type RequestCall = (String, Option<(u64, u64)>);
struct Server {
    address: std::net::SocketAddr,
    calls: Arc<Mutex<Vec<RequestCall>>>,
    headers: Arc<Mutex<Vec<String>>>,
    end: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Server {
    fn new(routes: BTreeMap<&'static str, Body>, ranged: bool, invalid_range: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let end = Arc::new(AtomicBool::new(false));
        let stop = end.clone();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        let headers = Arc::new(Mutex::new(Vec::new()));
        let seen_headers = headers.clone();
        let thread = std::thread::spawn(move || {
            for connection in listener.incoming() {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let mut socket = connection.unwrap();
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
                    assert!(request.len() < 8192);
                }
                let request = String::from_utf8(request).unwrap();
                seen_headers.lock().unwrap().push(request.clone());
                let target = request.split_whitespace().nth(1).unwrap().to_owned();
                let range = request
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("Range: bytes=")
                            .or_else(|| line.strip_prefix("range: bytes="))
                    })
                    .map(|value| {
                        let (a, b) = value.split_once('-').unwrap();
                        (a.parse::<u64>().unwrap(), b.parse::<u64>().unwrap())
                    });
                seen.lock().unwrap().push((target.clone(), range));
                let Some(body) = routes.get(target.split('?').next().unwrap()) else {
                    let _ = socket.write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    );
                    continue;
                };
                if ranged && let Some((start, end)) = range {
                    let bytes = body.range(start, end);
                    let reported = if invalid_range { start + 1 } else { start };
                    let _ = write!(
                        socket,
                        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {reported}-{end}/{}\r\nConnection: close\r\n\r\n",
                        bytes.len(),
                        body.len()
                    );
                    let _ = socket.write_all(&bytes);
                } else {
                    let _ = write!(
                        socket,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    if let Body::Bytes(bytes) | Body::Truncated { bytes, .. } = body {
                        let _ = socket.write_all(bytes);
                    }
                }
            }
        });
        Self {
            address,
            calls,
            headers,
            end,
            thread: Some(thread),
        }
    }
    fn url(&self) -> String {
        format!("http://{}/entry?source-secret=synthetic", self.address)
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.end.store(true, Ordering::Release);
        let _ = TcpStream::connect(self.address);
        self.thread.take().unwrap().join().unwrap();
    }
}
fn converter(stop: Stop, events: Arc<Collector>) -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(NoSink),
        )),
        "unused",
        "unused",
    )
    .with_stop(stop)
    .with_activity_sink(events)
}
fn policy() -> FetchPolicy {
    FetchPolicy {
        allow_http: true,
        allow_private_network: true,
        attempts: 1,
        ..FetchPolicy::default()
    }
}
fn progress(events: &Collector) -> Vec<drmr_core::events::Record> {
    events
        .records()
        .into_iter()
        .filter(|r| r.get("stage").and_then(serde_json::Value::as_str) == Some("acquisition"))
        .collect()
}
#[test]
fn remote_metadata_manifests_follow_only_playlists_without_keys_or_media() {
    let master = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100,CODECS=\"avc1\"\nchild.m3u8?signature=child-secret\n";
    let child =
        b"#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"secret-key\"\n#EXTINF:2,\nprivate-segment.ts\n";
    let dash=b"<MPD type='dynamic'><Period><AdaptationSet><ContentProtection schemeIdUri='urn:mpeg:dash:mp4protection:2011' value='cenc'/><Representation id='private-id'><SegmentTemplate initialization='private-init.mp4' media='private-segment.m4s'/></Representation></AdaptationSet></Period></MPD>";
    for (root, others, format, protection) in [
        (
            master.as_slice(),
            vec![("/child.m3u8", Body::Bytes(child.to_vec()))],
            InputFormat::Hls,
            Protection::HlsAes128,
        ),
        (dash.as_slice(), vec![], InputFormat::Dash, Protection::Cenc),
    ] {
        let mut routes = BTreeMap::from([("/entry", Body::Bytes(root.to_vec()))]);
        routes.extend(others);
        let server = Server::new(routes, false, false);
        let events = Arc::new(Collector::default());
        let mut settings = policy();
        settings
            .allowed_origins
            .push(format!("http://{}", server.address));
        settings.headers_by_origin.push((
            settings.allowed_origins[0].clone(),
            vec![("Authorization".into(), "Bearer auth-secret".into())],
        ));
        let result = converter(Stop::new(), events.clone())
            .inspect_url_metadata(&server.url(), &settings)
            .unwrap();
        assert_eq!(result.format, format);
        assert_eq!(result.protection, protection);
        assert_eq!(result.evidence, Evidence::Manifest);
        assert!(!result.media_verified);
        let calls = server.calls.lock().unwrap();
        assert_eq!(calls.len(), if format == InputFormat::Hls { 2 } else { 1 });
        let shown = serde_json::to_string(&result).unwrap()
            + &serde_json::to_string(&events.records()).unwrap();
        for private in [
            "source-secret",
            "secret-key",
            "private-segment",
            "private-init",
            "private-id",
            "child-secret",
            "auth-secret",
            &server.url(),
        ] {
            assert!(
                !shown.contains(private),
                "private source values must not be reported"
            );
        }
        assert!(server.headers.lock().unwrap().iter().all(|header| {
            header
                .to_ascii_lowercase()
                .contains("authorization: bearer auth-secret")
        }));
        assert!(
            result
                .tracks
                .iter()
                .enumerate()
                .all(|(index, track)| track.id
                    == format!(
                        "{}-{index:04}",
                        if format == InputFormat::Hls {
                            "hls"
                        } else {
                            "dash"
                        }
                    ))
        );
        let records = progress(&events);
        let last = records.last().unwrap();
        assert_eq!(last["resources_started"], calls.len() as u64);
        assert_eq!(last["resources_completed"], calls.len() as u64);
        assert_eq!(
            last["bytes_received"],
            root.len() as u64
                + if format == InputFormat::Hls {
                    child.len() as u64
                } else {
                    0
                }
        );
    }
}

#[test]
fn metadata_range_probe_accepts_bom_and_leading_whitespace_manifests() {
    for body in [
        b"\xef\xbb\xbf#EXTM3U\n#EXTINF:2,\nnever.ts\n".as_slice(),
        b"         <MPD><Period><AdaptationSet><Representation/></AdaptationSet></Period></MPD>"
            .as_slice(),
    ] {
        let server = Server::new(
            BTreeMap::from([("/entry", Body::Bytes(body.to_vec()))]),
            true,
            false,
        );
        let report = converter(Stop::new(), Arc::new(Collector::default()))
            .inspect_url_metadata(&server.url(), &policy())
            .unwrap();
        assert_eq!(report.evidence, Evidence::Manifest);
        assert!(!report.media_verified);
        assert!(
            server
                .calls
                .lock()
                .unwrap()
                .iter()
                .all(|(target, _)| target.starts_with("/entry"))
        );
    }
}
#[test]
fn metadata_ranges_skip_giant_mdat_and_accept_a_trailing_movie_box() {
    let movie = movie();
    let offset = 1_u64 << 34;
    let server = Server::new(
        BTreeMap::from([("/entry", Body::Sparse { movie, offset })]),
        true,
        false,
    );
    let events = Arc::new(Collector::default());
    let result = converter(Stop::new(), events.clone())
        .inspect_url_metadata(&server.url(), &policy())
        .unwrap();
    assert_eq!(result.format, InputFormat::Mp4);
    assert_eq!(result.evidence, Evidence::ContainerMetadata);
    assert_eq!(result.protection, Protection::Clear);
    assert!(!result.media_verified);
    assert!(server.calls.lock().unwrap().len() < 10);
    assert!(
        progress(&events).last().unwrap()["bytes_received"]
            .as_u64()
            .unwrap()
            < 1024
    );
}
#[test]
fn metadata_rejects_nonrange_binary_wrong_ranges_limits_and_private_origins() {
    for (ranged, invalid_range) in [(false, false), (true, true)] {
        let server = Server::new(
            BTreeMap::from([("/entry", Body::Bytes(movie()))]),
            ranged,
            invalid_range,
        );
        let events = Arc::new(Collector::default());
        assert!(
            converter(Stop::new(), events)
                .inspect_url_metadata(&server.url(), &policy())
                .is_err()
        );
    }
    let server = Server::new(
        BTreeMap::from([(
            "/entry",
            Body::Bytes(b"#EXTM3U\n#EXTINF:2,\nx.ts\n".to_vec()),
        )]),
        false,
        false,
    );
    let events = Arc::new(Collector::default());
    let mut settings = policy();
    settings.max_total_bytes = 8;
    assert!(
        converter(Stop::new(), events)
            .inspect_url_metadata(&server.url(), &settings)
            .is_err()
    );
    let events = Arc::new(Collector::default());
    let mut settings = policy();
    settings.allow_private_network = false;
    assert!(
        converter(Stop::new(), events)
            .inspect_url_metadata(&server.url(), &settings)
            .is_err()
    );
    assert_eq!(server.calls.lock().unwrap().len(), 1);
}

#[test]
fn mp4_metadata_does_not_inherit_the_manifest_byte_cap() {
    let server = Server::new(
        BTreeMap::from([("/entry", Body::Bytes(movie()))]),
        true,
        false,
    );
    let mut settings = policy();
    settings.max_playlist_bytes = 1;
    assert!(
        converter(Stop::new(), Arc::new(Collector::default()))
            .inspect_url_metadata(&server.url(), &settings)
            .is_ok()
    );
}

#[test]
fn remote_mp4_metadata_rejects_too_many_boxes_inside_the_movie() {
    let base = movie();
    let mut body = base[8..].to_vec();
    for _ in 7..16_384 {
        body.extend(atom(*b"free", &[]));
    }
    let server = Server::new(
        BTreeMap::from([("/entry", Body::Bytes(atom(*b"moov", &body)))]),
        true,
        false,
    );
    assert!(
        converter(Stop::new(), Arc::new(Collector::default()))
            .inspect_url_metadata(&server.url(), &policy())
            .is_ok()
    );
    body.extend(atom(*b"free", &[]));
    let server = Server::new(
        BTreeMap::from([("/entry", Body::Bytes(atom(*b"moov", &body)))]),
        true,
        false,
    );
    assert_eq!(
        converter(Stop::new(), Arc::new(Collector::default()))
            .inspect_url_metadata(&server.url(), &policy())
            .unwrap_err()
            .0,
        "mp4-box-count-limit"
    );
}

#[test]
fn metadata_playlist_and_origin_limits_refuse_children_before_a_request() {
    let other = Server::new(
        BTreeMap::from([(
            "/entry",
            Body::Bytes(b"#EXTM3U\n#EXTINF:2,\nnever.ts\n".to_vec()),
        )]),
        false,
        false,
    );
    for reference in [
        "child.m3u8".to_owned(),
        other.url(),
        "file:///private-child.m3u8".to_owned(),
    ] {
        let master = format!("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\n{reference}\n");
        let server = Server::new(
            BTreeMap::from([
                ("/entry", Body::Bytes(master.into_bytes())),
                (
                    "/child.m3u8",
                    Body::Bytes(b"#EXTM3U\n#EXTINF:2,\nnever.ts\n".to_vec()),
                ),
            ]),
            false,
            false,
        );
        let mut settings = policy();
        if reference == "child.m3u8" {
            settings.max_playlists = 1;
        }
        let result = converter(Stop::new(), Arc::new(Collector::default()))
            .inspect_url_metadata(&server.url(), &settings);
        assert!(result.is_err());
        assert_eq!(server.calls.lock().unwrap().len(), 1);
        assert!(other.calls.lock().unwrap().is_empty());
    }
}

#[test]
fn full_acquisition_progress_counts_received_bytes_and_never_completes_a_failed_resource() {
    let manifest =
        b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\na.ts\n#EXTINF:2,\nb.ts\n#EXT-X-ENDLIST\n";
    for complete in [true, false] {
        let mut routes = BTreeMap::from([
            ("/entry", Body::Bytes(manifest.to_vec())),
            ("/a.ts", Body::Bytes(vec![1; 101])),
        ]);
        if complete {
            routes.insert("/b.ts", Body::Bytes(vec![2; 203]));
        }
        let server = Server::new(routes, false, false);
        let events = Arc::new(Collector::default());
        let directory = tempfile::tempdir().unwrap();
        let result = converter(Stop::new(), events.clone())
            .with_max_workers(Some(1))
            .unwrap()
            .acquire_source(&server.url(), &directory.path().join("source"), &policy());
        assert_eq!(result.is_ok(), complete);
        assert_eq!(directory.path().join("source").exists(), complete);
        let records = progress(&events);
        let final_record = records.last().unwrap();
        assert_eq!(final_record["resources_started"], 3);
        assert_eq!(
            final_record["resources_completed"],
            if complete { 3 } else { 2 }
        );
        assert_eq!(
            final_record["bytes_received"],
            manifest.len() as u64 + 101 + if complete { 203 } else { 0 }
        );
        let mut total_known = false;
        for record in records {
            if let Some(total) = record.get("resources_total") {
                assert_eq!(total, 3);
                total_known = true;
            } else {
                assert!(!total_known, "a discovered total must remain available");
            }
            assert_eq!(record.len(), if total_known { 6 } else { 5 });
            assert_eq!(record["event"], "progress");
        }
        assert!(
            total_known,
            "resource discovery must publish its fixed total"
        );
    }
}

#[test]
fn full_mp4_and_dash_progress_count_exact_payloads() {
    let mp4 = movie();
    let dash = b"<MPD xmlns='urn:mpeg:dash:schema:mpd:2011' type='static' mediaPresentationDuration='PT2S'><Period><AdaptationSet><Representation id='v' bandwidth='10'><SegmentTemplate initialization='init.mp4' media='s$Number$.m4s' timescale='1'><SegmentTimeline><S d='2'/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet></Period></MPD>";
    for (body, children) in [
        (mp4.as_slice(), vec![]),
        (
            dash.as_slice(),
            vec![
                ("/init.mp4", Body::Bytes(vec![1; 17])),
                ("/s1.m4s", Body::Bytes(vec![2; 29])),
            ],
        ),
    ] {
        let mut routes = BTreeMap::from([("/entry", Body::Bytes(body.to_vec()))]);
        routes.extend(children);
        let expected_bytes = routes.values().map(Body::len).sum::<u64>();
        let expected_resources = routes.len() as u64;
        let server = Server::new(routes, false, false);
        let events = Arc::new(Collector::default());
        let directory = tempfile::tempdir().unwrap();
        let source = converter(Stop::new(), events.clone())
            .acquire_source(&server.url(), &directory.path().join("source"), &policy())
            .unwrap();
        source.check(&Stop::new()).unwrap();
        let records = progress(&events);
        let final_record = records.last().unwrap();
        assert_eq!(final_record["resources_started"], expected_resources);
        assert_eq!(final_record["resources_completed"], expected_resources);
        assert_eq!(final_record["bytes_received"], expected_bytes);
    }
}

#[test]
fn failed_retries_keep_all_received_payload_bytes_without_false_completion_or_partial_files() {
    let bytes = movie();
    let server = Server::new(
        BTreeMap::from([(
            "/entry",
            Body::Truncated {
                bytes: bytes.clone(),
                declared: 999,
            },
        )]),
        false,
        false,
    );
    let mut settings = policy();
    settings.attempts = 2;
    let events = Arc::new(Collector::default());
    let directory = tempfile::tempdir().unwrap();
    assert!(
        converter(Stop::new(), events.clone())
            .acquire_source(&server.url(), &directory.path().join("source"), &settings)
            .is_err()
    );
    assert!(!directory.path().join("source").exists());
    assert_eq!(server.calls.lock().unwrap().len(), 2);
    let records = progress(&events);
    let final_record = records.last().unwrap();
    assert_eq!(final_record["resources_started"], 1);
    assert_eq!(final_record["resources_completed"], 0);
    assert_eq!(final_record["bytes_received"], bytes.len() as u64 * 2);
}
#[test]
fn metadata_stop_flushes_partial_bytes_without_completing_the_resource() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (end_tx, end_rx) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() < 8192);
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1024\r\nConnection: close\r\n\r\n#EXTM3U\n",
            )
            .unwrap();
        socket.flush().unwrap();
        ready_tx.send(()).unwrap();
        let _ = end_rx.recv_timeout(Duration::from_secs(5));
    });
    let stop = Stop::new();
    let events = Arc::new(Collector::default());
    let command = converter(stop.clone(), events.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let _ =
            tx.send(command.inspect_url_metadata(&format!("http://{address}/entry"), &policy()));
    });
    let ready = ready_rx.recv_timeout(Duration::from_secs(2));
    stop.set();
    let result = rx.recv_timeout(Duration::from_secs(2));
    let _ = end_tx.send(());
    server.join().unwrap();
    worker.join().unwrap();
    ready.unwrap();
    assert_eq!(result.unwrap().unwrap_err().0, "cancelled");
    let records = progress(&events);
    let final_record = records.last().unwrap();
    assert_eq!(final_record["resources_started"], 1);
    assert_eq!(final_record["resources_completed"], 0);
    assert!(final_record["bytes_received"].as_u64().unwrap() <= 8);
}
#[test]
fn metadata_accepts_extended_and_size_zero_movies_and_refuses_duplicate_movies() {
    let ordinary = movie();
    let extended = [
        &1_u32.to_be_bytes()[..],
        b"moov",
        &(ordinary.len() as u64 + 8).to_be_bytes(),
        &ordinary[8..],
    ]
    .concat();
    let mut zero = ordinary.clone();
    zero[..4].copy_from_slice(&0_u32.to_be_bytes());
    for body in [ordinary.clone(), extended, zero] {
        let server = Server::new(BTreeMap::from([("/entry", Body::Bytes(body))]), true, false);
        assert!(
            converter(Stop::new(), Arc::new(Collector::default()))
                .inspect_url_metadata(&server.url(), &policy())
                .is_ok()
        );
    }
    let server = Server::new(
        BTreeMap::from([("/entry", Body::Bytes([ordinary.clone(), ordinary].concat()))]),
        true,
        false,
    );
    assert!(
        converter(Stop::new(), Arc::new(Collector::default()))
            .inspect_url_metadata(&server.url(), &policy())
            .is_err()
    );
}
