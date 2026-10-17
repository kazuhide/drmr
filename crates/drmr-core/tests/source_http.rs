//! Credential-safe HTTP fetching against deterministic local fixture servers.
// Keep related operations together so the test exposes the
// correspondence.
#![allow(clippy::too_many_lines)]

mod support;

use drmr_core::source::platform::{Platform, Stream, SystemPlatform, TlsContext};
use drmr_core::source::{Failure, FetchPolicy, Fetcher, SourceRequest};
use serde_json::{Value, json};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::server::{Protocol, TestServer};
use support::{
    Analysis, FnHooks, FreeSpace, Resolves, fixed, remote_policy, timing, token, ts_packet,
    ts_playlist, video,
};

const PLAYLIST: &[u8] =
    b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n";

fn aac() -> impl drmr_core::source::Prober {
    fixed(vec![
        json!({"index": 0, "codec_type": "audio", "codec_name": "aac"}),
    ])
}

fn analyze_remote(
    prober: &dyn drmr_core::source::Prober,
    url: &str,
    asset_id: &str,
    output_root: &Path,
    policy: &FetchPolicy,
) -> Result<drmr_core::source::SourceAnalysis, Failure> {
    Analysis::new(prober).run(&SourceRequest::new(url), asset_id, output_root, policy)
}

fn sha256_count(snapshot: &Path) -> usize {
    let data: Value = serde_json::from_slice(&std::fs::read(snapshot).unwrap()).unwrap();
    data["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|resource| resource["sha256"].as_str().unwrap().to_string())
        .collect::<HashSet<_>>()
        .len()
}

#[test]
fn remote_child_query_is_not_inherited_and_public_data_is_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::routes(vec![
        (
            "/master.m3u8?token=sentinel",
            200,
            vec![],
            PLAYLIST.to_vec(),
        ),
        ("/segment.ts", 200, vec![], ts_packet("", b'x')),
    ]);
    let policy = FetchPolicy {
        headers_by_origin: vec![(
            server.origin.clone(),
            vec![("Authorization".into(), "sentinel-auth".into())],
        )],
        ..remote_policy(&server.origin)
    };
    let result = analyze_remote(
        &aac(),
        &format!("{}/master.m3u8?token=sentinel", server.origin),
        "remote",
        tmp.path(),
        &policy,
    )
    .unwrap();
    assert_eq!(
        server.paths(),
        ["/master.m3u8?token=sentinel", "/segment.ts"]
    );
    assert!(
        server
            .requests()
            .iter()
            .all(|request| request.header("Authorization") == Some("sentinel-auth"))
    );
    assert!(
        !serde_json::to_string(&result.to_public_dict())
            .unwrap()
            .contains("sentinel")
    );
    assert!(!format!("{policy:?}").contains("sentinel"));
}

#[test]
fn the_request_is_the_one_http_client_writes() {
    // `Host` with the implementation (not 80), the policy's headers in order, then Accept-Encoding;
    // no User-Agent, no Accept.
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::routes(vec![
        ("/m.m3u8", 200, vec![], PLAYLIST.to_vec()),
        ("/segment.ts", 200, vec![], ts_packet("", b'x')),
    ]);
    let policy = FetchPolicy {
        headers_by_origin: vec![(
            server.origin.clone(),
            vec![("X-One".into(), "1".into()), ("X-Two".into(), "2".into())],
        )],
        ..remote_policy(&server.origin)
    };
    analyze_remote(
        &aac(),
        &format!("{}/m.m3u8", server.origin),
        "request",
        tmp.path(),
        &policy,
    )
    .unwrap();
    let request = &server.requests()[0];
    let port = server.origin.rsplit(':').next().unwrap();
    let names: Vec<(&str, &str)> = request
        .headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("Host", &*format!("127.0.0.1:{port}")),
            ("X-One", "1"),
            ("X-Two", "2"),
            ("Accept-Encoding", "identity")
        ]
    );
}

#[test]
fn http10_content_length_closes_socket_after_exact_read() {
    let tmp = tempfile::tempdir().unwrap();
    let segment = ts_packet("", b'x');
    let server = TestServer::routes(vec![
        (
            "/master",
            200,
            vec![("Content-Length", &*PLAYLIST.len().to_string())],
            PLAYLIST.to_vec(),
        ),
        ("/segment.ts", 200, vec![("Content-Length", "188")], segment),
    ]);
    let result = analyze_remote(
        &aac(),
        &format!("{}/master", server.origin),
        "length",
        tmp.path(),
        &remote_policy(&server.origin),
    )
    .unwrap();
    assert!(result.admitted && result.tracks.len() == 1);
}

#[test]
fn truncated_http_resource_is_retried_and_never_admitted() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::routes(vec![
        (
            "/master",
            200,
            vec![("Content-Length", &*PLAYLIST.len().to_string())],
            PLAYLIST.to_vec(),
        ),
        (
            "/segment.ts",
            200,
            vec![("Content-Length", "999")],
            ts_packet("", b'x'),
        ),
    ]);
    let policy = FetchPolicy {
        attempts: 2,
        ..remote_policy(&server.origin)
    };
    let result = analyze_remote(
        &aac(),
        &format!("{}/master", server.origin),
        "short",
        tmp.path(),
        &policy,
    );
    assert_eq!(token(result), "source-fetch-failed");
    assert_eq!(
        server
            .paths()
            .iter()
            .filter(|path| *path == "/segment.ts")
            .count(),
        2
    );
    assert!(!tmp.path().join("short/clear/source").exists());
}

#[test]
fn redirect_rechecks_origin_and_never_forwards_auth() {
    let tmp = tempfile::tempdir().unwrap();
    let destination = TestServer::routes(vec![
        ("/entry.m3u8", 200, vec![], PLAYLIST.to_vec()),
        ("/segment.ts", 200, vec![], ts_packet("", b'x')),
    ]);
    let location = format!("{}/entry.m3u8", destination.origin);
    let origin = TestServer::routes(vec![(
        "/start",
        302,
        vec![("Location", &*location)],
        Vec::new(),
    )]);
    let policy = FetchPolicy {
        allowed_origins: vec![origin.origin.clone(), destination.origin.clone()],
        headers_by_origin: vec![(
            origin.origin.clone(),
            vec![("Authorization".into(), "private-auth".into())],
        )],
        ..remote_policy(&origin.origin)
    };
    analyze_remote(
        &aac(),
        &format!("{}/start", origin.origin),
        "redirect",
        tmp.path(),
        &policy,
    )
    .unwrap();
    assert_eq!(
        origin.requests()[0].header("Authorization"),
        Some("private-auth")
    );
    assert!(
        destination
            .requests()
            .iter()
            .all(|request| request.header("Authorization").is_none())
    );
    let denied = remote_policy(&origin.origin);
    let result = analyze_remote(
        &aac(),
        &format!("{}/start", origin.origin),
        "denied",
        tmp.path(),
        &denied,
    );
    assert_eq!(token(result), "source-not-allowed");
}

#[test]
fn a_redirect_without_a_location_and_six_hops_are_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let hop = TestServer::start(Protocol::Http10, |exchange| {
        exchange.send_response(302);
        let next = format!("/hop{}", exchange.path().len());
        exchange.send_header("Location", &next);
        exchange.end_headers();
    });
    let result = analyze_remote(
        &aac(),
        &format!("{}/start", hop.origin),
        "loop",
        tmp.path(),
        &remote_policy(&hop.origin),
    );
    assert_eq!(token(result), "redirect-budget-exceeded");
    assert_eq!(hop.paths().len(), 6, "six requests, the first included");
    let bare = TestServer::routes(vec![("/start", 302, vec![], Vec::new())]);
    let result = analyze_remote(
        &aac(),
        &format!("{}/start", bare.origin),
        "bare",
        tmp.path(),
        &remote_policy(&bare.origin),
    );
    assert_eq!(token(result), "invalid-source-redirect");
}

#[test]
fn private_dns_default_rejected_and_error_redacted() {
    let tmp = tempfile::tempdir().unwrap();
    let platform = Arc::new(Resolves(vec!["169.254.169.254:443".parse().unwrap()]));
    let policy = FetchPolicy {
        allowed_origins: vec!["https://media.example".into()],
        ..FetchPolicy::default()
    };
    let error = Analysis::new(&aac())
        .platform(platform)
        .run(
            &SourceRequest::new("https://media.example/master?token=private"),
            "dns",
            tmp.path(),
            &policy,
        )
        .unwrap_err();
    assert_eq!(error.token(), Some("source-not-allowed"));
    assert!(!format!("{error} {error:?}").contains("private"));
}

#[test]
fn a_loopback_answer_is_private_unless_the_policy_allows_it() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::routes(vec![("/m.m3u8", 200, vec![], PLAYLIST.to_vec())]);
    let policy = FetchPolicy {
        allow_private_network: false,
        ..remote_policy(&server.origin)
    };
    let result = analyze_remote(
        &aac(),
        &format!("{}/m.m3u8", server.origin),
        "loopback",
        tmp.path(),
        &policy,
    );
    assert_eq!(token(result), "source-not-allowed");
    assert!(server.paths().is_empty(), "refused before any connection");
    let policy = FetchPolicy {
        allow_http: false,
        ..remote_policy(&server.origin)
    };
    let result = analyze_remote(
        &aac(),
        &format!("{}/m.m3u8", server.origin),
        "plain",
        tmp.path(),
        &policy,
    );
    assert_eq!(token(result), "source-not-allowed", "http needs allow_http");
}

/// A stream serving one canned response, as the test's fake `HTTPConnection` does.
struct Canned {
    response: io::Cursor<Vec<u8>>,
}

impl Read for Canned {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.response.read(buf)
    }
}

impl Write for Canned {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for Canned {
    fn set_read_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        Ok(())
    }

    fn set_write_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeTls {
    responses: Mutex<Vec<Vec<u8>>>,
    contexts: Mutex<Vec<TlsContext>>,
    wraps: Mutex<Vec<(usize, String)>>,
    pinned: Mutex<Vec<SocketAddr>>,
}

impl Platform for FakeTls {
    fn resolve(&self, _: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(vec!["8.8.8.8:443".parse().unwrap()])
    }

    fn connect(&self, address: SocketAddr, _: Duration) -> io::Result<Box<dyn Stream>> {
        self.pinned.lock().unwrap().push(address);
        let response = self.responses.lock().unwrap().remove(0);
        Ok(Box::new(Canned {
            response: io::Cursor::new(response),
        }))
    }

    fn tls_context(&self) -> io::Result<TlsContext> {
        let context = TlsContext::with_webpki_roots()?;
        self.contexts.lock().unwrap().push(context.clone());
        Ok(context)
    }

    fn wrap_tls(
        &self,
        context: &TlsContext,
        stream: Box<dyn Stream>,
        host: &str,
        _timeout: Duration,
    ) -> io::Result<Box<dyn Stream>> {
        let index = self
            .contexts
            .lock()
            .unwrap()
            .iter()
            .position(|known| Arc::ptr_eq(&known.0, &context.0))
            .expect("a context the platform made");
        self.wraps.lock().unwrap().push((index, host.to_string()));
        Ok(stream)
    }
}

#[test]
fn https_context_reused_per_fetcher_across_retries_and_redirects() {
    let tmp = tempfile::tempdir().unwrap();
    let response = |head: &str| {
        format!("HTTP/1.1 {head}\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndata")
            .into_bytes()
    };
    let platform = Arc::new(FakeTls {
        responses: Mutex::new(vec![
            response("503 Service Unavailable"),
            response("302 Found\r\nLocation: https://second.example/media"),
            response("200 OK"),
            response("200 OK"),
            response("200 OK"),
        ]),
        ..FakeTls::default()
    });
    let policy = FetchPolicy {
        allowed_origins: vec![
            "https://first.example".into(),
            "https://second.example".into(),
        ],
        ..FetchPolicy::default()
    };
    let fetcher = Fetcher::new(&policy, tmp.path(), platform.clone());
    assert!(
        platform.contexts.lock().unwrap().is_empty(),
        "local and HTTP-only sources never pay TLS initialization"
    );
    assert_eq!(
        fetcher
            .read("https://first.example/master", false, None)
            .unwrap()
            .0,
        b"data"
    );
    assert_eq!(
        fetcher
            .read("https://second.example/segment", false, None)
            .unwrap()
            .0,
        b"data"
    );
    assert_eq!(platform.contexts.lock().unwrap().len(), 1);
    let wraps = platform.wraps.lock().unwrap().clone();
    assert_eq!(
        wraps
            .iter()
            .map(|(_, host)| host.as_str())
            .collect::<Vec<_>>(),
        [
            "first.example",
            "first.example",
            "second.example",
            "second.example"
        ]
    );
    assert!(wraps.iter().all(|(context, _)| *context == 0));
    assert_eq!(
        *platform.pinned.lock().unwrap(),
        vec!["8.8.8.8:443".parse::<SocketAddr>().unwrap(); 4]
    );
    let separate = Fetcher::new(&policy, tmp.path(), platform.clone());
    separate
        .read("https://second.example/another-analysis", false, None)
        .unwrap();
    assert_eq!(platform.contexts.lock().unwrap().len(), 2);
    assert_eq!(platform.wraps.lock().unwrap().last().unwrap().0, 1);
}

#[test]
fn range_http_requires_exact_content_range() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::routes(vec![
        ("/master", 200, vec![], b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-BYTERANGE:4@2\n#EXTINF:1,\ndata\n#EXT-X-ENDLIST\n".to_vec()),
        ("/data", 206, vec![("Content-Range", "bytes 0-3/6")], b"abcd".to_vec()),
    ]);
    let result = analyze_remote(
        &aac(),
        &format!("{}/master", server.origin),
        "range",
        tmp.path(),
        &remote_policy(&server.origin),
    );
    assert_eq!(token(result), "invalid-byte-range");
    let range = server.requests()[1].header("Range").map(str::to_string);
    assert_eq!(range.as_deref(), Some("bytes=2-5"));
}

#[test]
fn a_matching_content_range_admits_the_range() {
    let tmp = tempfile::tempdir().unwrap();
    let mut segment = vec![0, 0];
    segment.extend(ts_packet("", b'r'));
    let server = TestServer::routes(vec![
        ("/master", 200, vec![], b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-BYTERANGE:188@2\n#EXTINF:1,\ndata\n#EXT-X-ENDLIST\n".to_vec()),
        ("/data", 206, vec![("Content-Range", "bytes 2-189/190")], segment[2..].to_vec()),
    ]);
    let result = analyze_remote(
        &aac(),
        &format!("{}/master", server.origin),
        "range",
        tmp.path(),
        &remote_policy(&server.origin),
    )
    .unwrap();
    assert_eq!(
        result.tracks[0].segments[0]
            .byte_range
            .map(|r| (r.offset, r.length)),
        Some((2, 188))
    );
}

#[test]
fn remote_playlist_cannot_use_operator_local_roots() {
    let tmp = tempfile::tempdir().unwrap();
    let local = tmp.path().join("staging");
    std::fs::create_dir(&local).unwrap();
    support::media(&local, None);
    let uri = drmr_core::source::pypath::as_uri(&local.join("segment.ts"));
    let playlist = format!("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\n{uri}\n#EXT-X-ENDLIST\n");
    let server = TestServer::routes(vec![("/master", 200, vec![], playlist.into_bytes())]);
    let policy = FetchPolicy {
        allowed_local_roots: vec![local.clone()],
        ..remote_policy(&server.origin)
    };
    let result = analyze_remote(
        &aac(),
        &format!("{}/master", server.origin),
        "local-escape",
        tmp.path(),
        &policy,
    );
    assert_eq!(token(result), "source-not-allowed");
}

#[test]
fn a_worker_keeps_its_connection_alive_across_the_resources_it_fetches() {
    let tmp = tempfile::tempdir().unwrap();
    let count = 10;
    let mut routes = vec![("/master.m3u8".to_string(), 200, ts_playlist(count, "seg"))];
    routes
        .extend((0..count).map(|i| (format!("/seg{i}.ts"), 200, ts_packet(&i.to_string(), b'x'))));
    let server = TestServer::routes11(routes);
    let policy = FetchPolicy {
        connections: 1,
        ..remote_policy(&server.origin)
    };
    let probe = fixed(vec![
        json!({"index": 0, "codec_type": "video", "codec_name": "h264"}),
    ]);
    let result = analyze_remote(
        &probe,
        &format!("{}/master.m3u8", server.origin),
        "keepalive",
        tmp.path(),
        &policy,
    )
    .unwrap();
    assert!(result.admitted && server.paths().len() == count + 1);
    assert!(
        server.connection_count() <= 2,
        "the playlist and every segment travel over a kept-alive connection, not one per resource: {}",
        server.connection_count()
    );
    assert_eq!(sha256_count(&result.snapshot_path), count + 1);
}

#[test]
fn a_failure_stops_the_remaining_downloads() {
    let tmp = tempfile::tempdir().unwrap();
    let count = 40;
    let mut routes = vec![("/master.m3u8".to_string(), 200, ts_playlist(count, "seg"))];
    routes
        .extend((0..count).map(|i| (format!("/seg{i}.ts"), 200, ts_packet(&i.to_string(), b'x'))));
    let mut bad = b"not a transport stream at all".to_vec();
    bad.resize(188, b'x');
    routes[4].2 = bad; // seg3
    let server = TestServer::routes11(routes);
    let started = Arc::new(Mutex::new(Vec::new()));
    let hooks = FnHooks {
        read: None,
        fetch: Some(Box::new({
            let started = Arc::clone(&started);
            move |fetcher: &Fetcher, url: &str, destination: &Path, range| {
                started
                    .lock()
                    .unwrap()
                    .push(url.rsplit('/').next().unwrap().to_string());
                if !url.ends_with("/seg3.ts") {
                    assert!(
                        fetcher.stop().wait(Some(Duration::from_secs(10))),
                        "every other download holds until the failure has been seen"
                    );
                    return Err(Failure::Cancelled);
                }
                fetcher.fetch(url, destination, range)
            }
        })),
    };
    let policy = FetchPolicy {
        connections: 4,
        ..remote_policy(&server.origin)
    };
    let probe = fixed(vec![
        json!({"index": 0, "codec_type": "video", "codec_name": "h264"}),
    ]);
    let started_at = Instant::now();
    let result = Analysis::new(&probe).hooks(&hooks).run(
        &SourceRequest::new(format!("{}/master.m3u8", server.origin)),
        "stop",
        tmp.path(),
        &policy,
    );
    assert_eq!(token(result), "unsupported-container");
    // The held downloads were released by the coordinator's stop, not by their own 10 s wait
    // (which the test cannot tell apart: its holders also give up after 10 s).
    assert!(
        started_at.elapsed() < Duration::from_secs(5),
        "{:?}",
        started_at.elapsed()
    );
    assert_eq!(
        server.paths(),
        ["/master.m3u8", "/seg3.ts"],
        "nothing is requested once the failure is seen"
    );
    // the four workers took segments 0-3; at most one more unit was handed to the freed
    // worker before the coordinator cancelled the queue, and it stopped without a request
    let started = started.lock().unwrap();
    let first: HashSet<&str> = started[..4].iter().map(String::as_str).collect();
    assert_eq!(
        first,
        HashSet::from(["seg0.ts", "seg1.ts", "seg2.ts", "seg3.ts"])
    );
    assert!(started.len() <= 5, "{started:?}");
    assert!(!tmp.path().join("stop/clear/source").exists());
}

#[test]
fn segments_landing_before_their_init_wait_for_it_before_inspection() {
    let tmp = tempfile::tempdir().unwrap();
    let count = 6;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let probe = {
        let seen = Arc::clone(&seen);
        move |path: &Path,
              init: Option<&Path>|
              -> Result<Vec<drmr_core::source::ProbeStream>, Failure> {
            let init = init.expect("an init");
            assert!(
                std::fs::read(init)
                    .unwrap()
                    .starts_with(b"\x00\x00\x00\x08ftyp")
            );
            seen.lock()
                .unwrap()
                .push(path.file_name().unwrap().to_string_lossy().into_owned());
            Ok(support::streams(&[
                json!({"index": 0, "codec_type": "video", "codec_name": "h264"}),
            ]))
        }
    };
    let landed = Arc::new(Mutex::new(0_usize));
    let all_landed = drmr_core::concurrency::Stop::new();
    let hooks = FnHooks {
        read: None,
        fetch: Some(Box::new({
            let (landed, all_landed) = (Arc::clone(&landed), all_landed.clone());
            move |fetcher: &Fetcher, url: &str, destination: &Path, range| {
                if url.ends_with("/init.mp4") {
                    assert!(
                        all_landed.wait(Some(Duration::from_secs(10))),
                        "the init is held until every media segment has landed"
                    );
                }
                let result = fetcher.fetch(url, destination, range);
                if !url.ends_with("/init.mp4") {
                    let mut landed = landed.lock().unwrap();
                    *landed += 1;
                    if *landed == count {
                        all_landed.set();
                    }
                }
                result
            }
        })),
    };
    let mut playlist =
        String::from("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MAP:URI=\"init.mp4\"\n");
    for i in 0..count {
        write!(playlist, "#EXTINF:6,\nseg{i}.m4s\n").expect("String never fails to write");
    }
    playlist.push_str("#EXT-X-ENDLIST\n");
    let mut routes = vec![
        ("/master.m3u8".to_string(), 200, playlist.into_bytes()),
        (
            "/init.mp4".to_string(),
            200,
            b"\x00\x00\x00\x08ftyp".to_vec(),
        ),
    ];
    routes.extend((0..count).map(|i| {
        let mut body = b"\x00\x00\x00\x08moof".to_vec();
        body.extend_from_slice(i.to_string().as_bytes());
        (format!("/seg{i}.m4s"), 200, body)
    }));
    let server = TestServer::routes11(routes);
    let collector = drmr_core::events::Collector::default();
    let policy = FetchPolicy {
        connections: 4,
        ..remote_policy(&server.origin)
    };
    let result = Analysis::new(&probe)
        .hooks(&hooks)
        .log(&collector)
        .run(
            &SourceRequest::new(format!("{}/master.m3u8", server.origin)),
            "late-init",
            tmp.path(),
            &policy,
        )
        .unwrap();
    assert_eq!(seen.lock().unwrap().len(), count);
    assert_eq!(result.tracks[0].container, "fmp4");
    assert_eq!(
        result.tracks[0]
            .init_path
            .as_ref()
            .unwrap()
            .file_name()
            .unwrap(),
        "r000001.mp4"
    );
    let names: Vec<String> = result.tracks[0].segments[..2]
        .iter()
        .map(|s| s.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["r000002.m4s", "r000003.m4s"]);
    let messages: Vec<String> = collector
        .records()
        .iter()
        .map(|e| e["message"].as_str().unwrap().to_string())
        .collect();
    let last_download = messages
        .iter()
        .rposition(|m| m == "segment-downloaded")
        .unwrap();
    let first_probe = messages.iter().position(|m| m == "segment-probed").unwrap();
    assert!(
        last_download < first_probe,
        "every segment had landed before the first inspection, which waited for the init"
    );
}

#[test]
fn playlist_budget_is_checked_before_any_media_download() {
    let tmp = tempfile::tempdir().unwrap();
    let mut master = String::from("#EXTM3U\n");
    for i in 0..3 {
        write!(master, "#EXT-X-STREAM-INF:BANDWIDTH={}\nv{i}.m3u8\n", i + 1)
            .expect("String never fails to write");
    }
    let mut routes = vec![("/master.m3u8".to_string(), 200, master.into_bytes())];
    routes.extend((0..3).map(|i| {
        (
            format!("/v{i}.m3u8"),
            200,
            ts_playlist(2, &format!("v{i}s")),
        )
    }));
    let server = TestServer::routes11(routes);
    let policy = FetchPolicy {
        max_playlists: 3,
        ..remote_policy(&server.origin)
    };
    let probe = fixed(vec![
        json!({"index": 0, "codec_type": "video", "codec_name": "h264"}),
    ]);
    let result = analyze_remote(
        &probe,
        &format!("{}/master.m3u8", server.origin),
        "budget",
        tmp.path(),
        &policy,
    );
    assert_eq!(token(result), "playlist-budget-exceeded");
    assert_eq!(server.paths(), ["/master.m3u8"]);
}

/// Observe timeout resets on the real TCP connection without depending on CI scheduling.
struct TimeoutPlatform {
    connections: AtomicUsize,
    requests: Arc<Mutex<Vec<(usize, bool, bool)>>>,
}

struct TimeoutStream {
    socket: TcpStream,
    connection: usize,
    timeout: Duration,
    fresh_read: Cell<bool>,
    fresh_write: Cell<bool>,
    writing: bool,
    requests: Arc<Mutex<Vec<(usize, bool, bool)>>>,
}

impl Platform for TimeoutPlatform {
    fn connect(&self, address: SocketAddr, timeout: Duration) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(TimeoutStream {
            socket: TcpStream::connect_timeout(&address, timeout)?,
            timeout,
            connection: self.connections.fetch_add(1, Ordering::SeqCst),
            fresh_read: Cell::new(false),
            fresh_write: Cell::new(false),
            writing: false,
            requests: Arc::clone(&self.requests),
        }))
    }
}

impl Read for TimeoutStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let result = self.socket.read(buf);
        // A following request must reset both bounds after the previous response's reads.
        self.fresh_read.set(false);
        self.fresh_write.set(false);
        self.writing = false;
        result
    }
}

impl Write for TimeoutStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if !self.writing {
            self.requests.lock().unwrap().push((
                self.connection,
                self.fresh_read.get(),
                self.fresh_write.get(),
            ));
            self.writing = true;
        }
        self.socket.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.socket.flush()
    }
}

impl Stream for TimeoutStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.fresh_read.set(timeout == Some(self.timeout));
        self.socket.set_read_timeout(timeout)
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.fresh_write.set(timeout == Some(self.timeout));
        self.socket.set_write_timeout(timeout)
    }
}

#[test]
fn a_kept_alive_connection_gives_the_next_request_the_whole_timeout() {
    // The former delayed-body fixture left only 0.2 s of scheduling headroom for the next
    // response. Observe both fresh timeout settings before each real request instead: a
    // missing reset fails even when the network is fast, without a wall-clock race.
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::routes11(vec![
        ("/m.m3u8".into(), 200, ts_playlist(2, "seg")),
        ("/seg0.ts".into(), 200, ts_packet("0", b'x')),
        ("/seg1.ts".into(), 200, ts_packet("1", b'x')),
    ]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let policy = FetchPolicy {
        connections: 1,
        timeout: 2.0,
        attempts: 1,
        ..remote_policy(&server.origin)
    };
    let result = Analysis::new(&fixed(video()))
        .platform(Arc::new(TimeoutPlatform {
            requests: Arc::clone(&requests),
            connections: AtomicUsize::new(0),
        }))
        .run(
            &SourceRequest::new(format!("{}/m.m3u8", server.origin)),
            "fresh-timeout",
            tmp.path(),
            &policy,
        )
        .unwrap();
    assert!(result.admitted);
    assert_eq!(server.paths(), ["/m.m3u8", "/seg0.ts", "/seg1.ts"]);
    assert_eq!(server.connection_count(), 2);
    assert_eq!(
        *requests.lock().unwrap(),
        [(0, true, true), (1, true, true), (1, true, true)],
        "both media requests reuse a socket and reset both timeout bounds"
    );
}

#[test]
fn a_body_may_take_one_timeout_per_mebibyte_so_shared_links_do_not_time_out() {
    let _timing = timing();
    let tmp = tempfile::tempdir().unwrap();
    let body: Vec<u8> = ts_packet("", b'x').repeat((3 * 1024 * 1024) / 188); // just under 3 MiB
    let server = TestServer::start(Protocol::Http11, move |exchange| {
        if exchange.path() == "/m.m3u8" {
            return exchange.respond(&ts_playlist(1, "seg"));
        }
        exchange.send_response(200);
        exchange.send_header("Content-Length", &body.len().to_string());
        exchange.end_headers();
        for piece in body.chunks(512 * 1024) {
            // about 1.5 s in all: longer than one timeout
            let _ = exchange.write(piece);
            std::thread::sleep(Duration::from_millis(250));
        }
    });
    let policy = FetchPolicy {
        connections: 1,
        timeout: 1.0,
        attempts: 1,
        max_total_bytes: 8 * 1024 * 1024,
        ..remote_policy(&server.origin)
    };
    let result = analyze_remote(
        &fixed(video()),
        &format!("{}/m.m3u8", server.origin),
        "allowance",
        tmp.path(),
        &policy,
    )
    .unwrap();
    assert!(result.admitted);
    assert_eq!(server.paths(), ["/m.m3u8", "/seg0.ts"]);
}

#[test]
fn a_stalled_body_fails_within_the_timeout_not_within_its_size_allowance() {
    // A declared 9 MiB body has an allowance of nine timeouts (4.5 s at 0.5 s); a chunk that
    // stalls must still fail after one timeout.
    let _timing = timing();
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::start(Protocol::Http11, |exchange| {
        if exchange.path() == "/m.m3u8" {
            return exchange.respond(&ts_playlist(1, "seg"));
        }
        exchange.send_response(200);
        exchange.send_header("Content-Length", &(9 * 1024 * 1024).to_string());
        exchange.end_headers();
        let _ = exchange.write(&ts_packet("", b'x'));
        std::thread::sleep(Duration::from_secs(5)); // longer than the whole allowance
    });
    let policy = FetchPolicy {
        connections: 1,
        timeout: 0.5,
        attempts: 1,
        max_total_bytes: 16 * 1024 * 1024,
        ..remote_policy(&server.origin)
    };
    let started = Instant::now();
    let result = analyze_remote(
        &fixed(video()),
        &format!("{}/m.m3u8", server.origin),
        "stall",
        tmp.path(),
        &policy,
    );
    let elapsed = started.elapsed();
    assert_eq!(token(result), "source-fetch-failed");
    assert!(
        elapsed < Duration::from_millis(1800),
        "the stall was detected by the per-chunk timeout: {elapsed:?}"
    );
}

#[test]
fn a_body_without_a_declared_length_earns_its_allowance_as_it_lands() {
    // Chunked transfer: no Content-Length, so the deadline grows by one timeout per MiB
    // received (never the resource cap's 256 timeouts up front).
    let _timing = timing();
    let tmp = tempfile::tempdir().unwrap();
    let body: Vec<u8> = ts_packet("", b'x').repeat((3 * 1024 * 1024) / 188);
    let length = body.len();
    let server = TestServer::start(Protocol::Http11, move |exchange| {
        if exchange.path() == "/m.m3u8" {
            return exchange.respond(&ts_playlist(1, "seg"));
        }
        exchange.send_response(200);
        exchange.send_header("Transfer-Encoding", "chunked");
        exchange.end_headers();
        for piece in body.chunks(512 * 1024) {
            let mut frame = format!("{:x}\r\n", piece.len()).into_bytes();
            frame.extend_from_slice(piece);
            frame.extend_from_slice(b"\r\n");
            let _ = exchange.write(&frame);
            std::thread::sleep(Duration::from_millis(250));
        }
        let _ = exchange.write(b"0\r\n\r\n");
    });
    let policy = FetchPolicy {
        connections: 1,
        timeout: 1.0,
        attempts: 1,
        max_total_bytes: 8 * 1024 * 1024,
        ..remote_policy(&server.origin)
    };
    let result = analyze_remote(
        &fixed(video()),
        &format!("{}/m.m3u8", server.origin),
        "chunked",
        tmp.path(),
        &policy,
    )
    .unwrap();
    assert!(result.admitted);
    assert_eq!(server.paths(), ["/m.m3u8", "/seg0.ts"]);
    assert_eq!(
        std::fs::metadata(&result.tracks[0].segments[0].path)
            .unwrap()
            .len(),
        length as u64
    );
}

#[test]
fn a_server_that_drops_the_idle_connection_costs_no_attempt() {
    let tmp = tempfile::tempdir().unwrap();
    let count = 5;
    let mut routes: HashMap<String, Vec<u8>> =
        HashMap::from([("/master.m3u8".to_string(), ts_playlist(count, "seg"))]);
    routes.extend((0..count).map(|i| (format!("/seg{i}.ts"), ts_packet(&i.to_string(), b'x'))));
    let server = TestServer::start(Protocol::Http11, move |exchange| {
        let body = routes[exchange.path()].clone();
        exchange.respond(&body);
        exchange.close_connection = true; // dropped after the response, without Connection: close
    });
    let policy = FetchPolicy {
        connections: 1,
        attempts: 1,
        ..remote_policy(&server.origin)
    };
    let result = analyze_remote(
        &fixed(video()),
        &format!("{}/master.m3u8", server.origin),
        "dropped",
        tmp.path(),
        &policy,
    )
    .unwrap();
    assert!(result.admitted && server.paths().len() == count + 1);
    assert_eq!(
        server.connection_count(),
        count + 1,
        "every request after the first found its pooled socket closed and reconnected for free"
    );
}

#[test]
fn a_truncated_response_refunds_its_bytes_before_the_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let count = 3;
    let playlist = ts_playlist(count, "seg");
    let total = playlist.len() + count * 188;
    let mut routes: HashMap<String, Vec<u8>> =
        HashMap::from([("/master.m3u8".to_string(), playlist)]);
    routes.extend((0..count).map(|i| (format!("/seg{i}.ts"), ts_packet(&i.to_string(), b'x'))));
    let serve_counts = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
    let server = TestServer::start(Protocol::Http11, {
        let serve_counts = Arc::clone(&serve_counts);
        move |exchange| {
            let path = exchange.path().to_string();
            let times = {
                let mut counts = serve_counts.lock().unwrap();
                *counts.entry(path.clone()).or_default() += 1;
                counts[&path]
            };
            let body = routes[&path].clone();
            exchange.send_response(200);
            exchange.send_header("Content-Length", &body.len().to_string());
            exchange.end_headers();
            if path == "/seg1.ts" && times == 1 {
                let _ = exchange.write(&body[..100]); // truncated once, then dropped
                exchange.close_connection = true;
                return;
            }
            let _ = exchange.write(&body);
        }
    });
    let policy = FetchPolicy {
        connections: 1,
        attempts: 2,
        max_total_bytes: total as u64,
        ..remote_policy(&server.origin)
    };
    let result = analyze_remote(
        &fixed(video()),
        &format!("{}/master.m3u8", server.origin),
        "truncated",
        tmp.path(),
        &policy,
    )
    .unwrap();
    assert!(result.admitted);
    assert_eq!(
        server.paths().iter().filter(|p| *p == "/seg1.ts").count(),
        2
    );
    assert_eq!(sha256_count(&result.snapshot_path), count + 1);
}

#[test]
fn close_all_drops_every_kept_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::routes11(vec![
        ("/a.ts".to_string(), 200, ts_packet("a", b'x')),
        ("/b.ts".to_string(), 200, ts_packet("b", b'x')),
    ]);
    let policy = FetchPolicy {
        connections: 1,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(
        &policy,
        tmp.path(),
        Arc::new(drmr_core::source::platform::SystemPlatform),
    );
    fetcher
        .fetch(
            &format!("{}/a.ts", server.origin),
            &tmp.path().join("a.part"),
            None,
        )
        .unwrap();
    fetcher
        .fetch(
            &format!("{}/b.ts", server.origin),
            &tmp.path().join("b.part"),
            None,
        )
        .unwrap();
    assert!(fetcher.kept() > 0 && server.connection_count() == 1);
    fetcher.close_all();
    assert_eq!(fetcher.kept(), 0);
    fetcher
        .fetch(
            &format!("{}/a.ts", server.origin),
            &tmp.path().join("a2.part"),
            None,
        )
        .unwrap();
    assert_eq!(
        server.connection_count(),
        2,
        "after close_all the next request opens a new connection"
    );
    fetcher.close_all();
    assert_eq!(server.paths(), ["/a.ts", "/b.ts", "/a.ts"]);
}

#[test]
fn the_stop_flag_is_honoured_before_every_attempt_including_a_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let holder: Arc<Mutex<Option<drmr_core::concurrency::Stop>>> = Arc::new(Mutex::new(None));
    let server = TestServer::start(Protocol::Http11, {
        let holder = Arc::clone(&holder);
        move |exchange| {
            // the analysis is being stopped while this request is in flight
            holder.lock().unwrap().as_ref().unwrap().set();
            exchange.send_response(503);
            exchange.send_header("Content-Length", "0");
            exchange.end_headers();
        }
    });
    let policy = FetchPolicy {
        connections: 1,
        attempts: 3,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(
        &policy,
        tmp.path(),
        Arc::new(drmr_core::source::platform::SystemPlatform),
    );
    *holder.lock().unwrap() = Some(fetcher.stop().clone());
    let result = fetcher.fetch(
        &format!("{}/a.ts", server.origin),
        &tmp.path().join("a.part"),
        None,
    );
    assert_eq!(result.unwrap_err(), Failure::Cancelled);
    assert_eq!(
        server.paths(),
        ["/a.ts"],
        "the retry of the 503 never left: the stop was checked before it"
    );
    let result = fetcher.fetch(
        &format!("{}/b.ts", server.origin),
        &tmp.path().join("b.part"),
        None,
    );
    assert_eq!(result.unwrap_err(), Failure::Cancelled);
    assert_eq!(
        server.paths(),
        ["/a.ts"],
        "a new resource is refused before any connection is made"
    );
    fetcher.close_all();
    assert!(!tmp.path().join("a.part").exists() && !tmp.path().join("b.part").exists());
}

#[test]
fn a_5xx_is_retried_while_attempts_remain_and_then_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::start(Protocol::Http11, |exchange| {
        exchange.send_response(503);
        exchange.send_header("Content-Length", "0");
        exchange.end_headers();
    });
    let policy = FetchPolicy {
        attempts: 3,
        ..remote_policy(&server.origin)
    };
    let result = analyze_remote(
        &fixed(video()),
        &format!("{}/m.m3u8", server.origin),
        "unavailable",
        tmp.path(),
        &policy,
    );
    assert_eq!(token(result), "source-http-failed");
    assert_eq!(
        server.paths().len(),
        3,
        "three attempts, the last one reported as it is"
    );
    let missing = TestServer::routes11(vec![]);
    let result = analyze_remote(
        &fixed(video()),
        &format!("{}/m.m3u8", missing.origin),
        "missing",
        tmp.path(),
        &remote_policy(&missing.origin),
    );
    assert_eq!(token(result), "source-http-failed");
    assert_eq!(missing.paths().len(), 1, "a 4xx is not retried");
}

#[test]
fn a_compressed_body_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::routes(vec![(
        "/m.m3u8",
        200,
        vec![("Content-Encoding", "gzip")],
        PLAYLIST.to_vec(),
    )]);
    let result = analyze_remote(
        &fixed(video()),
        &format!("{}/m.m3u8", server.origin),
        "gzip",
        tmp.path(),
        &remote_policy(&server.origin),
    );
    assert_eq!(token(result), "unsupported-content-encoding");
    assert_eq!(
        server.requests()[0].header("Accept-Encoding"),
        Some("identity")
    );
}

#[test]
fn the_disk_floor_is_checked_against_the_declared_length_before_the_body_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let body: Vec<u8> = ts_packet("", b'x').repeat(50); // 9,400 bytes declared up front
    let server = TestServer::start(Protocol::Http11, move |exchange| {
        if exchange.path() == "/m.m3u8" {
            return exchange.respond(&ts_playlist(1, "seg"));
        }
        exchange.send_response(200);
        exchange.send_header("Content-Length", &body.len().to_string());
        exchange.end_headers();
        let _ = exchange.write(&body); // the client may have closed first
    });
    let platform = Arc::new(FreeSpace(|_: &Path| 1_000_000 + 5000));
    let policy = FetchPolicy {
        connections: 1,
        attempts: 1,
        min_free_disk_bytes: 1_000_000,
        ..remote_policy(&server.origin)
    };
    let result = Analysis::new(&fixed(video())).platform(platform).run(
        &SourceRequest::new(format!("{}/m.m3u8", server.origin)),
        "floor",
        tmp.path(),
        &policy,
    );
    assert_eq!(token(result), "disk-budget-exceeded");
    assert_eq!(server.paths(), ["/m.m3u8", "/seg0.ts"]);
    let files = support::hls_fixture::files_under(tmp.path());
    assert!(
        !files
            .iter()
            .any(|f| f.extension().is_some_and(|e| e == "part" || e == "ts")),
        "refused before the first byte landed: {files:?}"
    );
}

#[test]
fn a_timeout_on_a_reused_connection_spends_an_attempt_unlike_a_dropped_one() {
    // seg0 is prompt and leaves the worker's connection kept alive; seg1's headers on that
    // reused connection take longer than the timeout: the one attempt, no hidden second GET.
    let _timing = timing();
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::start(Protocol::Http11, |exchange| {
        if exchange.path() == "/master.m3u8" {
            return exchange.respond(&ts_playlist(2, "seg"));
        }
        if exchange.path() == "/seg1.ts" {
            std::thread::sleep(Duration::from_millis(1500)); // later than the 0.5 s timeout
        }
        let digit = exchange.path()[4..5].to_string();
        exchange.respond(&ts_packet(&digit, b'x'));
    });
    let policy = FetchPolicy {
        connections: 1,
        timeout: 0.5,
        attempts: 1,
        ..remote_policy(&server.origin)
    };
    let result = analyze_remote(
        &fixed(video()),
        &format!("{}/master.m3u8", server.origin),
        "reused-timeout",
        tmp.path(),
        &policy,
    );
    assert_eq!(token(result), "source-fetch-failed");
    assert_eq!(
        server.paths(),
        ["/master.m3u8", "/seg0.ts", "/seg1.ts"],
        "the timeout was the one attempt; no free retry hid it"
    );
}

#[test]
fn concurrent_declared_bodies_are_claimed_against_the_disk_floor_together() {
    let tmp = tempfile::tempdir().unwrap();
    let body: Vec<u8> = ts_packet("", b'x').repeat(50); // 9,400 bytes each; 18,800 of 12,000 headroom
    let server = TestServer::start(Protocol::Http11, move |exchange| {
        if exchange.path() == "/master.m3u8" {
            return exchange.respond(&ts_playlist(2, "seg"));
        }
        exchange.send_response(200);
        exchange.send_header("Content-Length", &body.len().to_string());
        exchange.end_headers();
        std::thread::sleep(Duration::from_millis(500)); // both bodies in flight while claimed
        let _ = exchange.write(&body);
    });
    let platform = Arc::new(FreeSpace(|_: &Path| 1_000_000 + 12_000));
    let policy = FetchPolicy {
        connections: 2,
        attempts: 1,
        min_free_disk_bytes: 1_000_000,
        ..remote_policy(&server.origin)
    };
    let result = Analysis::new(&fixed(video())).platform(platform).run(
        &SourceRequest::new(format!("{}/master.m3u8", server.origin)),
        "floor-together",
        tmp.path(),
        &policy,
    );
    assert_eq!(token(result), "disk-budget-exceeded");
    let mut paths = server.paths();
    paths.sort();
    assert_eq!(paths, ["/master.m3u8", "/seg0.ts", "/seg1.ts"]);
}

#[test]
fn a_declared_body_s_bytes_are_not_counted_against_the_floor_twice_as_they_land() {
    // A declared 16 MiB body with 20 MiB above the floor: the claim is made before the first
    // byte and shrinks as the bytes land (they then leave the disk's own free space).
    let tmp = tempfile::tempdir().unwrap();
    let floor: u64 = 1_000_000;
    let root = tmp.path().to_path_buf();
    let platform = Arc::new(FreeSpace(move |_: &Path| {
        let written: u64 = support::hls_fixture::files_under(&root)
            .iter()
            .filter_map(|file| std::fs::metadata(file).ok())
            .map(|meta| meta.len())
            .sum();
        floor + 20 * 1024 * 1024 - written
    }));
    let piece = ts_packet("", b'x').repeat(5577); // just over 1 MiB per write
    let declared = 16 * 1024 * 1024;
    let server = TestServer::start(Protocol::Http11, move |exchange| {
        if exchange.path() == "/m.m3u8" {
            return exchange.respond(&ts_playlist(1, "seg"));
        }
        exchange.send_response(200);
        exchange.send_header("Content-Length", &declared.to_string());
        exchange.end_headers();
        let mut sent = 0;
        while sent < declared {
            let part = &piece[..piece.len().min(declared - sent)];
            if exchange.write(part).is_err() {
                return;
            }
            sent += part.len();
        }
    });
    let policy = FetchPolicy {
        connections: 1,
        attempts: 1,
        min_free_disk_bytes: floor,
        max_total_bytes: 32 * 1024 * 1024,
        ..remote_policy(&server.origin)
    };
    let result = Analysis::new(&fixed(video()))
        .platform(platform)
        .run(
            &SourceRequest::new(format!("{}/m.m3u8", server.origin)),
            "no-double-count",
            tmp.path(),
            &policy,
        )
        .unwrap();
    assert!(result.admitted);
    assert_eq!(server.paths(), ["/m.m3u8", "/seg0.ts"]);
    assert_eq!(
        std::fs::metadata(&result.tracks[0].segments[0].path)
            .unwrap()
            .len(),
        declared as u64
    );
}

#[test]
fn an_undeclared_body_is_checked_against_the_floor_every_eight_mebibytes() {
    // Chunked, 9 MiB. The free space dips below the floor only while 7.5–8.5 MiB of the body
    // are on disk (and recovers after): only the check made as the 8 MiB mark is crossed
    // can see it — the check after the body sees plenty.
    let tmp = tempfile::tempdir().unwrap();
    let floor: u64 = 1_000_000;
    let root = tmp.path().to_path_buf();
    let checks = Arc::new(AtomicUsize::new(0));
    let platform = Arc::new(FreeSpace({
        let checks = Arc::clone(&checks);
        move |_: &Path| {
            checks.fetch_add(1, Ordering::SeqCst);
            let written: u64 = support::hls_fixture::files_under(&root)
                .iter()
                .filter(|file| file.extension().is_some_and(|ext| ext == "part"))
                .filter_map(|file| std::fs::metadata(file).ok())
                .map(|meta| meta.len())
                .sum();
            if (15 * 512 * 1024..17 * 512 * 1024).contains(&written) {
                floor - 1
            } else {
                floor + (1 << 30)
            }
        }
    }));
    let body = vec![0x47_u8; 9 * 1024 * 1024];
    let server = TestServer::start(Protocol::Http11, move |exchange| {
        if exchange.path() == "/m.m3u8" {
            return exchange.respond(&ts_playlist(1, "seg"));
        }
        exchange.send_response(200);
        exchange.send_header("Transfer-Encoding", "chunked");
        exchange.end_headers();
        for piece in body.chunks(256 * 1024) {
            let mut frame = format!("{:x}\r\n", piece.len()).into_bytes();
            frame.extend_from_slice(piece);
            frame.extend_from_slice(b"\r\n");
            if exchange.write(&frame).is_err() {
                return;
            }
        }
        let _ = exchange.write(b"0\r\n\r\n");
    });
    let policy = FetchPolicy {
        connections: 1,
        attempts: 1,
        min_free_disk_bytes: floor,
        max_total_bytes: 32 * 1024 * 1024,
        ..remote_policy(&server.origin)
    };
    let result = Analysis::new(&fixed(video())).platform(platform).run(
        &SourceRequest::new(format!("{}/m.m3u8", server.origin)),
        "undeclared",
        tmp.path(),
        &policy,
    );
    assert_eq!(token(result), "disk-budget-exceeded");
}

// --- TLS over loopback: a self-signed certificate made at test time -----------------------

/// The system, with the host resolving to the loopback server and its certificate trusted.
struct LoopbackTls {
    address: SocketAddr,
    context: TlsContext,
}

impl Platform for LoopbackTls {
    fn resolve(&self, _: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(vec![self.address])
    }

    fn tls_context(&self) -> io::Result<TlsContext> {
        Ok(self.context.clone())
    }
}

fn tls_configs() -> (Arc<rustls::ServerConfig>, TlsContext) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let certificate = certified.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(certified.signing_key.serialize_der().into());
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let server = rustls::ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![certificate.clone()], key)
        .unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(certificate).unwrap();
    let client = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    (Arc::new(server), TlsContext(Arc::new(client)))
}

/// A TLS server answering one request per connection with `response`, then closing the TCP
/// connection without `close_notify` (a "ragged" EOF, as many servers close).
fn ragged_tls_server(
    config: Arc<rustls::ServerConfig>,
    response: Vec<u8>,
) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    std::thread::spawn(move || {
        for tcp in listener.incoming() {
            let Ok(tcp) = tcp else { return };
            count.fetch_add(1, Ordering::SeqCst);
            let connection = rustls::ServerConnection::new(Arc::clone(&config)).unwrap();
            let mut tls = rustls::StreamOwned::new(connection, tcp);
            let mut head = Vec::new();
            let mut byte = [0_u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match tls.read(&mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let _ = tls.write_all(&response);
            let _ = tls.flush();
            // dropped: the TCP connection closes without a TLS close_notify
        }
    });
    (address, accepted)
}

#[test]
fn a_close_delimited_https_body_ends_at_a_ragged_eof_as_python_s_ssl_reads_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (server, context) = tls_configs();
    let mut response = b"HTTP/1.0 200 OK\r\n\r\n".to_vec();
    response.extend_from_slice(PLAYLIST);
    let (address, _) = ragged_tls_server(server, response);
    let origin = format!("https://localhost:{}", address.port());
    let policy = FetchPolicy {
        allowed_origins: vec![origin.clone()],
        allow_private_network: true,
        ..FetchPolicy::default()
    };
    let fetcher = Fetcher::new(
        &policy,
        tmp.path(),
        Arc::new(LoopbackTls { address, context }),
    );
    let (body, effective) = fetcher
        .read(&format!("{origin}/m.m3u8"), true, None)
        .unwrap();
    assert_eq!(body, PLAYLIST);
    assert_eq!(effective, format!("{origin}/m.m3u8"));
}

#[test]
fn a_kept_alive_https_connection_dropped_without_close_notify_costs_no_attempt() {
    let tmp = tempfile::tempdir().unwrap();
    let (server, context) = tls_configs();
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
        PLAYLIST.len()
    )
    .into_bytes();
    response.extend_from_slice(PLAYLIST);
    let (address, accepted) = ragged_tls_server(server, response);
    let origin = format!("https://localhost:{}", address.port());
    let policy = FetchPolicy {
        allowed_origins: vec![origin.clone()],
        allow_private_network: true,
        attempts: 1,
        ..FetchPolicy::default()
    };
    let fetcher = Fetcher::new(
        &policy,
        tmp.path(),
        Arc::new(LoopbackTls { address, context }),
    );
    fetcher
        .read(&format!("{origin}/a.m3u8"), true, None)
        .unwrap();
    assert_eq!(
        fetcher.kept(),
        1,
        "an HTTP/1.1 response with a length keeps its connection"
    );
    std::thread::sleep(Duration::from_millis(100)); // the server has closed it by now
    fetcher
        .read(&format!("{origin}/b.m3u8"), true, None)
        .unwrap();
    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "the dropped connection was replaced for free"
    );
}

/// An HTTP/1.1 `200` with a declared length.
fn http11(body: &[u8]) -> Vec<u8> {
    let mut response =
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).into_bytes();
    response.extend_from_slice(body);
    response
}

/// A fetcher of `https://localhost:<port>` reaching the loopback server, the policy edited.
fn loopback_fetcher(
    directory: &Path,
    address: SocketAddr,
    context: TlsContext,
    edit: impl FnOnce(&mut FetchPolicy),
) -> (Fetcher, String) {
    let origin = format!("https://localhost:{}", address.port());
    let mut policy = FetchPolicy {
        allowed_origins: vec![origin.clone()],
        allow_private_network: true,
        ..FetchPolicy::default()
    };
    edit(&mut policy);
    let fetcher = Fetcher::new(
        &policy,
        directory,
        Arc::new(LoopbackTls { address, context }),
    );
    (fetcher, origin)
}

/// Every TLS byte the server connection has queued for the peer.
fn queued_tls(connection: &mut rustls::ServerConnection) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    while connection.wants_write() {
        connection.write_tls(&mut out)?;
    }
    Ok(out)
}

/// `bytes`, with `trickle` one at a time 10 ms apart: every read of the peer then finds a
/// byte well within the tests' timeouts, while a whole TLS record takes a second or more.
fn send_tls(tcp: &mut TcpStream, bytes: &[u8], trickle: bool) -> io::Result<()> {
    if !trickle {
        return tcp.write_all(bytes);
    }
    for byte in bytes {
        tcp.write_all(std::slice::from_ref(byte))?;
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

/// One connection of [`trickling_tls_server`]: the handshake, one request head, the response.
fn serve_trickled(
    config: Arc<rustls::ServerConfig>,
    mut tcp: TcpStream,
    response: &[u8],
    slow_handshake: bool,
) -> io::Result<()> {
    let mut connection = rustls::ServerConnection::new(config).map_err(io::Error::other)?;
    let mut head = Vec::new();
    loop {
        let queued = queued_tls(&mut connection)?;
        send_tls(&mut tcp, &queued, slow_handshake)?;
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") && matches!(connection.reader().read(&mut byte), Ok(1)) {
            head.push(byte[0]);
        }
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
        if connection.read_tls(&mut tcp)? == 0 {
            return Ok(());
        }
        connection.process_new_packets().map_err(io::Error::other)?;
    }
    connection.writer().write_all(response)?;
    let records = queued_tls(&mut connection)?;
    send_tls(&mut tcp, &records, !slow_handshake)
}

/// A TLS server answering one request per connection with `response`; either its handshake
/// flight or its response is sent one byte every 10 ms.
fn trickling_tls_server(
    config: Arc<rustls::ServerConfig>,
    response: Vec<u8>,
    slow_handshake: bool,
) -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for tcp in listener.incoming() {
            let Ok(tcp) = tcp else { return };
            let (config, response) = (Arc::clone(&config), response.clone());
            std::thread::spawn(move || serve_trickled(config, tcp, &response, slow_handshake));
        }
    });
    address
}

#[test]
fn a_tls_record_trickled_past_the_timeout_times_out_as_one_ssl_read_does() {
    // CPython's `_ssl` makes the socket timeout a deadline for a whole `SSL_read`, however
    // many socket reads its record takes: a response trickled one byte every 10 ms (each read
    // well within the 0.3 s timeout, the record whole only after a second) fails every attempt.
    let _timing = timing();
    let tmp = tempfile::tempdir().unwrap();
    let (server, context) = tls_configs();
    let address = trickling_tls_server(server, http11(PLAYLIST), false);
    let (fetcher, origin) = loopback_fetcher(tmp.path(), address, context, |policy| {
        policy.timeout = 0.3;
        policy.attempts = 2;
    });
    let started = Instant::now();
    assert_eq!(
        token(fetcher.read(&format!("{origin}/m.m3u8"), true, None)),
        "source-fetch-failed"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_tls_handshake_trickled_past_the_timeout_times_out_as_the_handshake_does() {
    // The same deadline bounds `do_handshake` as a whole: the server's flight trickled one
    // byte every 10 ms never completes within 0.3 s.
    let _timing = timing();
    let tmp = tempfile::tempdir().unwrap();
    let (server, context) = tls_configs();
    let address = trickling_tls_server(server, http11(PLAYLIST), true);
    let (fetcher, origin) = loopback_fetcher(tmp.path(), address, context, |policy| {
        policy.timeout = 0.3;
        policy.attempts = 2;
    });
    let started = Instant::now();
    assert_eq!(
        token(fetcher.read(&format!("{origin}/m.m3u8"), true, None)),
        "source-fetch-failed"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// One kept-alive connection of [`extra_record_tls_server`].
fn serve_with_extra_record(
    config: Arc<rustls::ServerConfig>,
    tcp: TcpStream,
    response: &[u8],
    extra: &[u8],
) -> io::Result<()> {
    let connection = rustls::ServerConnection::new(config).map_err(io::Error::other)?;
    let mut tls = rustls::StreamOwned::new(connection, tcp);
    let mut first = true;
    loop {
        let mut head = Vec::new();
        let mut byte = [0_u8; 1];
        while !head.ends_with(b"\r\n\r\n") {
            if tls.read(&mut byte)? == 0 {
                return Ok(());
            }
            head.push(byte[0]);
        }
        tls.conn.writer().write_all(response)?;
        if first {
            tls.conn.writer().write_all(extra)?; // its own record, in the same TCP write
            first = false;
        }
        let records = queued_tls(&mut tls.conn)?;
        tls.sock.write_all(&records)?;
    }
}

/// A TLS server keeping its connections alive, answering every request with `response`; the
/// first answer on each connection is followed by one more record, `extra`.
fn extra_record_tls_server(
    config: Arc<rustls::ServerConfig>,
    response: Vec<u8>,
    extra: &'static [u8],
) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    std::thread::spawn(move || {
        for tcp in listener.incoming() {
            let Ok(tcp) = tcp else { return };
            count.fetch_add(1, Ordering::SeqCst);
            let (config, response) = (Arc::clone(&config), response.clone());
            std::thread::spawn(move || serve_with_extra_record(config, tcp, &response, extra));
        }
    });
    (address, accepted)
}

#[test]
fn a_later_tls_record_stays_for_the_next_response_as_ssl_read_leaves_it() {
    // `SSL_read` returns one record at most, so the 8 KiB buffer of the first response never
    // takes in the record after it: on the reused connection the next status line starts with
    // it (`BadStatusLine`, an attempt); a second attempt gets a fresh connection.
    for (attempts, recovered, connections) in [(1, false, 1), (2, true, 2)] {
        let tmp = tempfile::tempdir().unwrap();
        let (server, context) = tls_configs();
        let (address, accepted) = extra_record_tls_server(server, http11(PLAYLIST), b"JUNK");
        let (fetcher, origin) = loopback_fetcher(tmp.path(), address, context, |policy| {
            policy.attempts = attempts;
        });
        let first = fetcher
            .read(&format!("{origin}/a.m3u8"), true, None)
            .unwrap();
        assert_eq!(first.0, PLAYLIST);
        let second = fetcher.read(&format!("{origin}/b.m3u8"), true, None);
        if recovered {
            assert_eq!(second.unwrap().0, PLAYLIST);
        } else {
            assert_eq!(token(second), "source-fetch-failed");
        }
        assert_eq!(
            accepted.load(Ordering::SeqCst),
            connections,
            "attempts {attempts}"
        );
    }
}

#[test]
fn a_negative_chunk_size_reads_on_to_the_end_of_the_stream_as_http_client_does() {
    // `int(b"-1", 16)` is a size http.client accepts; `_read1_chunked` then reads
    // `fp.read1(-1)` (8 KiB at a time) to the end of the stream, so the body's limits apply
    // to whatever follows: here the playlist cap, which is not retried.
    let tmp = tempfile::tempdir().unwrap();
    let server = TestServer::start(Protocol::Http11, |exchange| {
        exchange.send_response(200);
        exchange.send_header("Transfer-Encoding", "chunked");
        exchange.end_headers();
        let mut body = b"-1\r\n".to_vec();
        body.extend(std::iter::repeat_n(b'#', 5000));
        let _ = exchange.write(&body);
    });
    let policy = FetchPolicy {
        max_playlist_bytes: 1000,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(&policy, tmp.path(), Arc::new(SystemPlatform));
    assert_eq!(
        token(fetcher.read(&format!("{}/m.m3u8", server.origin), true, None)),
        "playlist-too-large"
    );
    assert_eq!(server.paths().len(), 1, "a cap is not retried");
}
