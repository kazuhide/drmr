//! Conditional requests use the same deterministic Platform/Stream boundary as full GETs.

use super::*;
use crate::events::Collector;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const URL: &str = "https://media.example/asset?private=source";
const ETAG: &str = "\"private-validator\"";

struct FakeHttp {
    response: io::Cursor<Vec<u8>>,
    request: Arc<Mutex<Vec<u8>>>,
    cancel: Option<Stop>,
}

impl Read for FakeHttp {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if let Some(stop) = self.cancel.take() {
            stop.set();
        }
        self.response.read(bytes)
    }
}

impl Write for FakeHttp {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        lock(&self.request).extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Stream for FakeHttp {
    fn set_read_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        Ok(())
    }

    fn set_write_timeout(&self, _: Option<Duration>) -> io::Result<()> {
        Ok(())
    }
}

struct FakePlatform {
    responses: Mutex<VecDeque<Vec<u8>>>,
    requests: Mutex<Vec<Arc<Mutex<Vec<u8>>>>>,
    resolved: Mutex<Vec<(String, u16)>>,
    connected: Mutex<Vec<SocketAddr>>,
    tls_hosts: Mutex<Vec<String>>,
    free: AtomicU64,
    private_dns: AtomicBool,
    cancel: Mutex<Option<Stop>>,
}

impl FakePlatform {
    fn new(responses: impl IntoIterator<Item = Vec<u8>>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().collect()),
            requests: Mutex::default(),
            resolved: Mutex::default(),
            connected: Mutex::default(),
            tls_hosts: Mutex::default(),
            free: AtomicU64::new(u64::MAX),
            private_dns: AtomicBool::new(false),
            cancel: Mutex::default(),
        })
    }

    fn requests(&self) -> Vec<String> {
        lock(&self.requests)
            .iter()
            .map(|bytes| String::from_utf8(lock(bytes).clone()).unwrap())
            .collect()
    }
}

impl Platform for FakePlatform {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        lock(&self.resolved).push((host.to_string(), port));
        let address = if self.private_dns.load(Ordering::SeqCst) {
            [127, 0, 0, 1]
        } else {
            [8, 8, 8, 8]
        };
        Ok(vec![SocketAddr::from((address, port))])
    }

    fn connect(&self, address: SocketAddr, _: Duration) -> io::Result<Box<dyn Stream>> {
        lock(&self.connected).push(address);
        let response = lock(&self.responses)
            .pop_front()
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let request = Arc::new(Mutex::new(Vec::new()));
        lock(&self.requests).push(Arc::clone(&request));
        Ok(Box::new(FakeHttp {
            response: io::Cursor::new(response),
            request,
            cancel: lock(&self.cancel).take(),
        }))
    }

    fn wrap_tls(
        &self,
        _: &TlsContext,
        stream: Box<dyn Stream>,
        host: &str,
        _: Duration,
    ) -> io::Result<Box<dyn Stream>> {
        lock(&self.tls_hosts).push(host.to_string());
        Ok(stream)
    }

    fn disk_free(&self, _: &Path) -> io::Result<u64> {
        Ok(self.free.load(Ordering::SeqCst))
    }
}

fn response(status: u16, headers: &str, body: &[u8]) -> Vec<u8> {
    let mut response =
        format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n{headers}\r\n").into_bytes();
    response.extend_from_slice(body);
    response
}

fn policy() -> FetchPolicy {
    FetchPolicy {
        allowed_origins: vec![
            "https://media.example".into(),
            "https://other.example".into(),
        ],
        attempts: 1,
        ..FetchPolicy::default()
    }
}

fn prior(url: &str, size: u64, byte_range: Option<ByteRange>) -> Revalidation {
    Revalidation {
        effective_url_sha256: hex::encode(Sha256::digest(url.as_bytes())),
        strong_etag: StrongEtag::parse(ETAG).unwrap(),
        cached_size: size,
        byte_range,
    }
}

#[test]
fn strong_etags_are_bounded_ascii_and_redacted() {
    for valid in ["\"\"", "\"abc!#~\\,\""] {
        assert_eq!(StrongEtag::parse(valid).unwrap().as_str(), valid);
    }
    for invalid in [
        "W/\"abc\"",
        "*",
        "abc",
        "\"a b\"",
        "\"a\nb\"",
        "\"é\"",
        "\"a\", \"b\"",
    ] {
        assert!(StrongEtag::parse(invalid).is_none());
    }
    assert!(StrongEtag::parse(&format!("\"{}\"", "a".repeat(1022))).is_some());
    assert!(StrongEtag::parse(&format!("\"{}\"", "a".repeat(1023))).is_none());
    assert!(!format!("{:?}", StrongEtag::parse(ETAG).unwrap()).contains("private-validator"));
    assert!(!format!("{:?}", prior(URL, 4, None)).contains("private"));
}

#[test]
fn matching_304_creates_no_file_and_counts_reuse_without_received_bytes() {
    let root = crate::native::private_tempdir().unwrap();
    let platform = FakePlatform::new([response(
        304,
        &format!("ETag: {ETAG}\r\nContent-Length: 4\r\n"),
        b"",
    )]);
    let collector = Arc::new(Collector::default());
    let progress = super::super::progress::Progress::new(collector.clone());
    let fetcher = Fetcher::new(&policy(), root.path(), platform.clone())
        .with_progress(Some(progress.clone()));
    let destination = root.path().join("new.part");
    let received = fetcher
        .fetch_revalidated(URL, &destination, None, Some(&prior(URL, 4, None)))
        .unwrap();
    assert!(matches!(received, RevalidatedFetch::NotModified { effective } if effective == URL));
    assert!(!destination.exists());
    assert_eq!(lock(&fetcher.shared).used, 4);
    assert!(platform.requests()[0].contains(&format!("If-None-Match: {ETAG}\r\n")));
    drop(fetcher);
    drop(progress);
    let records = collector.records();
    let last = records.last().unwrap();
    assert_eq!(last["resources_started"], 1);
    assert_eq!(last["resources_completed"], 1);
    assert_eq!(last["bytes_received"], 0);
}

#[test]
fn existing_reads_and_file_fetches_reject_unsolicited_304() {
    let root = crate::native::private_tempdir().unwrap();
    let platform =
        FakePlatform::new((0..3).map(|_| response(304, &format!("ETag: {ETAG}\r\n"), b"")));
    let fetcher = Fetcher::new(&policy(), root.path(), platform.clone());
    assert_eq!(
        fetcher.read(URL, false, None).unwrap_err().token(),
        Some("source-http-failed")
    );
    let destination = root.path().join("new.part");
    assert_eq!(
        fetcher.fetch(URL, &destination, None).unwrap_err().token(),
        Some("source-http-failed")
    );
    assert_eq!(
        fetcher
            .fetch_revalidated(URL, &destination, None, None)
            .unwrap_err()
            .token(),
        Some("source-http-failed")
    );
    assert!(!destination.exists());
    assert!(
        platform
            .requests()
            .iter()
            .all(|request| !request.contains("If-None-Match"))
    );
}

#[test]
fn invalid_304_metadata_never_reuses_cached_bytes() {
    let root = crate::native::private_tempdir().unwrap();
    for headers in [
        String::new(),
        "ETag: \"different\"\r\n".into(),
        format!("ETag: W/{ETAG}\r\n"),
        format!("ETag: {ETAG}\r\nETag: {ETAG}\r\n"),
        format!("ETag: {ETAG}\r\nContent-Length: 3\r\n"),
        format!("ETag: {ETAG}\r\nContent-Length: nonsense\r\n"),
        format!("ETag: {ETAG}\r\nTransfer-Encoding: chunked\r\n"),
        format!("ETag: {ETAG}\r\nContent-Range: bytes 0-3/4\r\n"),
        format!("ETag: {ETAG}\r\nContent-Encoding: gzip\r\n"),
    ] {
        let platform = FakePlatform::new([response(304, &headers, b"")]);
        let fetcher = Fetcher::new(&policy(), root.path(), platform);
        let destination = root.path().join("new.part");
        let error = fetcher
            .fetch_revalidated(URL, &destination, None, Some(&prior(URL, 4, None)))
            .unwrap_err();
        assert!(!format!("{error:?}").contains("private"));
        assert!(!destination.exists());
        assert_eq!(lock(&fetcher.shared).used, 0);
    }
}

#[test]
fn fresh_bodies_record_only_supported_strong_etags_and_creation_identity() {
    let root = crate::native::private_tempdir().unwrap();
    for etag in [Some(ETAG), Some("W/\"weak\""), None] {
        let headers = format!(
            "Content-Length: 4\r\n{}",
            etag.map_or_else(String::new, |etag| format!("ETag: {etag}\r\n"))
        );
        let platform = FakePlatform::new([response(200, &headers, b"new!")]);
        let fetcher = Fetcher::new(&policy(), root.path(), platform);
        let destination = root.path().join("new.part");
        let received = fetcher
            .fetch_revalidated(URL, &destination, None, Some(&prior(URL, 4, None)))
            .unwrap();
        let RevalidatedFetch::Fresh {
            fetched,
            created,
            etag: recorded,
        } = received
        else {
            panic!("fresh body required")
        };
        assert_eq!(fetched.size, 4);
        assert_eq!(fetched.sha256, hex::encode(Sha256::digest(b"new!")));
        assert_eq!(std::fs::read(&destination).unwrap(), b"new!");
        assert!(
            created
                == crate::native::FileIdentity::read(&File::open(&destination).unwrap()).unwrap()
        );
        assert_eq!(
            recorded.as_ref().map(StrongEtag::as_str),
            etag.filter(|etag| !etag.starts_with("W/"))
        );
        std::fs::remove_file(destination).unwrap();
    }
}

#[test]
fn redirects_bind_validator_to_full_effective_url_and_current_origin_auth() {
    let root = crate::native::private_tempdir().unwrap();
    let final_url = "https://other.example/body?private=changed";
    let platform = FakePlatform::new([
        response(302, &format!("Location: {final_url}\r\n"), b""),
        response(304, &format!("ETag: {ETAG}\r\n"), b""),
    ]);
    let mut policy = policy();
    policy.headers_by_origin = vec![
        (
            "https://media.example".into(),
            vec![("Authorization".into(), "first-credential".into())],
        ),
        (
            "https://other.example".into(),
            vec![("Authorization".into(), "second-credential".into())],
        ),
    ];
    let fetcher = Fetcher::new(&policy, root.path(), platform.clone());
    assert!(matches!(
        fetcher
            .fetch_revalidated(
                URL,
                &root.path().join("new.part"),
                None,
                Some(&prior(final_url, 4, None))
            )
            .unwrap(),
        RevalidatedFetch::NotModified { .. }
    ));
    let requests = platform.requests();
    assert!(!requests[0].contains("If-None-Match"));
    assert!(requests[0].contains("Authorization: first-credential"));
    assert!(!requests[0].contains("second-credential"));
    assert!(requests[1].contains(&format!("If-None-Match: {ETAG}")));
    assert!(requests[1].contains("Authorization: second-credential"));
    assert!(!requests[1].contains("first-credential"));
    assert_eq!(
        *lock(&platform.tls_hosts),
        ["media.example", "other.example"]
    );
    assert_eq!(lock(&platform.resolved).len(), 2);
    assert!(
        lock(&platform.connected)
            .iter()
            .all(|address| address.ip() == "8.8.8.8".parse::<std::net::IpAddr>().unwrap())
    );
}

#[test]
fn changed_redirect_targets_do_not_receive_or_accept_old_validators() {
    let root = crate::native::private_tempdir().unwrap();
    for final_url in [
        "https://media.example/other?private=source",
        "https://media.example/asset?private=changed",
        "https://other.example/asset?private=source",
    ] {
        let platform = FakePlatform::new([
            response(302, &format!("Location: {final_url}\r\n"), b""),
            response(304, &format!("ETag: {ETAG}\r\n"), b""),
        ]);
        let fetcher = Fetcher::new(&policy(), root.path(), platform.clone());
        assert_eq!(
            fetcher
                .fetch_revalidated(
                    URL,
                    &root.path().join("new.part"),
                    None,
                    Some(&prior(URL, 4, None))
                )
                .unwrap_err()
                .token(),
            Some("source-http-failed")
        );
        let requests = platform.requests();
        assert!(requests[0].contains("If-None-Match"));
        assert!(!requests[1].contains("If-None-Match"));
    }
}

#[test]
fn matching_validator_never_bypasses_current_network_policy() {
    let root = crate::native::private_tempdir().unwrap();
    let platform = FakePlatform::new([]);
    let fetcher = Fetcher::new(&FetchPolicy::default(), root.path(), platform.clone());
    assert_eq!(
        fetcher
            .fetch_revalidated(
                URL,
                &root.path().join("new.part"),
                None,
                Some(&prior(URL, 4, None))
            )
            .unwrap_err()
            .token(),
        Some("source-not-allowed")
    );
    assert_eq!(platform.requests(), Vec::<String>::new());
    assert!(lock(&platform.resolved).is_empty());
    let platform = FakePlatform::new([]);
    platform.private_dns.store(true, Ordering::SeqCst);
    let fetcher = Fetcher::new(&policy(), root.path(), platform.clone());
    assert_eq!(
        fetcher
            .fetch_revalidated(
                URL,
                &root.path().join("new.part"),
                None,
                Some(&prior(URL, 4, None)),
            )
            .unwrap_err()
            .token(),
        Some("source-not-allowed")
    );
    assert_eq!(platform.requests(), Vec::<String>::new());
    assert!(lock(&platform.connected).is_empty());
}

#[test]
fn range_304_binds_cached_slice_but_content_length_names_the_whole_object() {
    let root = crate::native::private_tempdir().unwrap();
    let range = ByteRange {
        offset: 8,
        length: 4,
    };
    for content_length in [None, Some(12), Some(100)] {
        let headers = format!(
            "ETag: {ETAG}\r\n{}",
            content_length.map_or_else(String::new, |length| format!(
                "Content-Length: {length}\r\n"
            ))
        );
        let platform = FakePlatform::new([response(304, &headers, b"")]);
        let fetcher = Fetcher::new(&policy(), root.path(), platform.clone());
        assert!(matches!(
            fetcher
                .fetch_revalidated(
                    URL,
                    &root.path().join("new.part"),
                    Some(range),
                    Some(&prior(URL, 4, Some(range)))
                )
                .unwrap(),
            RevalidatedFetch::NotModified { .. }
        ));
        assert!(platform.requests()[0].contains("Range: bytes=8-11\r\n"));
    }
    for cached in [
        prior(URL, 4, None),
        prior(URL, 3, Some(range)),
        prior(
            URL,
            4,
            Some(ByteRange {
                offset: 0,
                length: 4,
            }),
        ),
    ] {
        let platform = FakePlatform::new([]);
        let fetcher = Fetcher::new(&policy(), root.path(), platform.clone());
        assert_eq!(
            fetcher
                .fetch_revalidated(
                    URL,
                    &root.path().join("new.part"),
                    Some(range),
                    Some(&cached)
                )
                .unwrap_err()
                .token(),
            Some("invalid-byte-range")
        );
        assert_eq!(platform.requests(), Vec::<String>::new());
    }
}

#[test]
fn reuse_is_charged_against_current_resource_and_aggregate_caps() {
    let root = crate::native::private_tempdir().unwrap();
    for (resource_cap, total_cap, already_used, cached_size) in [
        (3, 10, 0, 4),
        (10, 3, 0, 4),
        (10, 5, 2, 4),
        (u64::MAX, u64::MAX, 1, u64::MAX),
    ] {
        let platform = FakePlatform::new([response(304, &format!("ETag: {ETAG}\r\n"), b"")]);
        let policy = FetchPolicy {
            max_resource_bytes: resource_cap,
            max_total_bytes: total_cap,
            ..policy()
        };
        let fetcher = Fetcher::new(&policy, root.path(), platform);
        fetcher.reserve(already_used).unwrap();
        assert_eq!(
            fetcher
                .fetch_revalidated(
                    URL,
                    &root.path().join("new.part"),
                    None,
                    Some(&prior(URL, cached_size, None))
                )
                .unwrap_err()
                .token(),
            Some("byte-budget-exceeded")
        );
        assert_eq!(lock(&fetcher.shared).used, already_used);
    }
}

#[test]
fn cache_copy_reservations_share_and_release_the_current_disk_floor() {
    let root = crate::native::private_tempdir().unwrap();
    let platform = FakePlatform::new([]);
    platform.free.store(110, Ordering::SeqCst);
    let policy = FetchPolicy {
        min_free_disk_bytes: 100,
        ..policy()
    };
    let fetcher = Fetcher::new(&policy, root.path(), platform);
    let first = fetcher.reserve_copy(6).unwrap();
    assert_eq!(lock(&fetcher.shared).inflight, 6);
    assert!(
        matches!(fetcher.reserve_copy(5), Err(error) if error.token() == Some("disk-budget-exceeded"))
    );
    {
        let _second = fetcher.reserve_copy(4).unwrap();
        assert_eq!(lock(&fetcher.shared).inflight, 10);
    }
    assert_eq!(lock(&fetcher.shared).inflight, 6);
    drop(first);
    assert_eq!(lock(&fetcher.shared).inflight, 0);
    assert_eq!(lock(&fetcher.shared).used, 0);
    fetcher.stop().set();
    assert!(matches!(fetcher.reserve_copy(1), Err(Failure::Cancelled)));
}

#[test]
fn fresh_range_keeps_206_content_range_and_length_requirements() {
    let root = crate::native::private_tempdir().unwrap();
    let range = ByteRange {
        offset: 8,
        length: 4,
    };
    for (status, content_range, length, expected) in [
        (206, "bytes 8-11/12", 4, None),
        (200, "bytes 8-11/12", 4, Some("source-http-failed")),
        (206, "bytes 0-3/12", 4, Some("invalid-byte-range")),
        (206, "bytes 8-11/12", 3, Some("invalid-byte-range")),
    ] {
        let platform = FakePlatform::new([response(
            status,
            &format!(
                "ETag: {ETAG}\r\nContent-Range: {content_range}\r\nContent-Length: {length}\r\n"
            ),
            &b"data"[..length],
        )]);
        let fetcher = Fetcher::new(&policy(), root.path(), platform);
        let destination = root.path().join("new.part");
        let result = fetcher.fetch_revalidated(
            URL,
            &destination,
            Some(range),
            Some(&prior(URL, 4, Some(range))),
        );
        if let Some(expected) = expected {
            assert_eq!(result.unwrap_err().token(), Some(expected));
            assert!(!destination.exists());
        } else {
            assert!(matches!(
                result.unwrap(),
                RevalidatedFetch::Fresh { etag: Some(_), .. }
            ));
            assert_eq!(std::fs::read(&destination).unwrap(), b"data");
            std::fs::remove_file(destination).unwrap();
        }
    }
    let platform = FakePlatform::new([response(
        304,
        &format!("ETag: {ETAG}\r\nContent-Length: 4\r\n"),
        b"",
    )]);
    let fetcher = Fetcher::new(&policy(), root.path(), platform);
    assert_eq!(
        fetcher
            .fetch_revalidated(
                URL,
                &root.path().join("new.part"),
                Some(range),
                Some(&prior(URL, 4, Some(range)))
            )
            .unwrap_err()
            .token(),
        Some("source-http-failed")
    );
}

#[test]
fn cancellation_before_request_or_during_304_headers_cannot_complete_reuse() {
    let root = crate::native::private_tempdir().unwrap();
    for cancel_during_headers in [false, true] {
        let platform = FakePlatform::new([response(304, &format!("ETag: {ETAG}\r\n"), b"")]);
        let stop = Stop::new();
        if cancel_during_headers {
            *lock(&platform.cancel) = Some(stop.clone());
        } else {
            stop.set();
        }
        let fetcher = Fetcher::new(&policy(), root.path(), platform).with_stop(stop);
        assert_eq!(
            fetcher
                .fetch_revalidated(
                    URL,
                    &root.path().join("new.part"),
                    None,
                    Some(&prior(URL, 4, None))
                )
                .unwrap_err(),
            Failure::Cancelled
        );
        assert_eq!(lock(&fetcher.shared).used, 0);
    }
}

#[test]
fn custom_preconditions_are_refused_only_in_the_opt_in_internal_mode() {
    let root = crate::native::private_tempdir().unwrap();
    for name in [
        "if-none-match",
        "If-Match",
        "IF-Modified-Since",
        "If-Unmodified-Since",
        "if-range",
    ] {
        let platform = FakePlatform::new([response(200, "Content-Length: 4\r\n", b"data")]);
        let mut policy = policy();
        policy.headers_by_origin = vec![(
            "https://media.example".into(),
            vec![(name.into(), "private-custom-value".into())],
        )];
        let fetcher = Fetcher::new(&policy, root.path(), platform.clone());
        for cached in [None, Some(prior(URL, 4, None))] {
            let error = fetcher
                .fetch_revalidated(URL, &root.path().join("new.part"), None, cached.as_ref())
                .unwrap_err();
            assert_eq!(error.token(), Some("invalid-fetch-policy"));
            assert!(!format!("{error:?}").contains("private"));
        }
        assert_eq!(platform.requests(), Vec::<String>::new());
        assert_eq!(fetcher.read(URL, false, None).unwrap().0, b"data");
        assert!(platform.requests()[0].contains(name));
    }
}

#[test]
fn unknown_format_entrypoint_records_fresh_etag_and_revalidates_without_a_body() {
    let root = crate::native::private_tempdir().unwrap();
    let body = b"\0\0\0\x18ftypisom\0\0\0\0isomiso2";
    let platform = FakePlatform::new([
        response(
            200,
            &format!("ETag: {ETAG}\r\nContent-Length: {}\r\n", body.len()),
            body,
        ),
        response(304, &format!("ETag: {ETAG}\r\n"), b""),
    ]);
    let fetcher = Fetcher::new(&policy(), root.path(), platform);
    let first = fetcher
        .fetch_entrypoint_revalidated(URL, &root.path().join("first.part"), None)
        .unwrap();
    assert!(matches!(
        first,
        RevalidatedFetch::Fresh { etag: Some(_), .. }
    ));
    assert!(matches!(
        fetcher
            .fetch_entrypoint_revalidated(
                URL,
                &root.path().join("second.part"),
                Some(&prior(URL, body.len() as u64, None))
            )
            .unwrap(),
        RevalidatedFetch::NotModified { .. }
    ));
    assert!(!root.path().join("second.part").exists());
}

#[test]
fn ordinary_recording_without_prior_sends_a_full_get_and_only_records_strong_etags() {
    let root = crate::native::private_tempdir().unwrap();
    for etag in [Some(ETAG), Some("W/\"weak\""), None] {
        let headers = format!(
            "Content-Length: 4\r\n{}",
            etag.map_or_else(String::new, |etag| format!("ETag: {etag}\r\n")),
        );
        let platform = FakePlatform::new([response(200, &headers, b"data")]);
        let fetcher = Fetcher::new(&policy(), root.path(), platform.clone());
        let destination = root.path().join("new.part");
        let RevalidatedFetch::Fresh { etag: recorded, .. } = fetcher
            .fetch_revalidated(URL, &destination, None, None)
            .unwrap()
        else {
            panic!("fresh response required");
        };
        assert_eq!(
            recorded.as_ref().map(StrongEtag::as_str),
            etag.filter(|etag| !etag.starts_with("W/")),
        );
        assert!(!platform.requests()[0].contains("If-None-Match"));
        std::fs::remove_file(destination).unwrap();
    }
}

#[test]
fn conditional_requests_keep_server_error_and_partial_body_retries() {
    let root = crate::native::private_tempdir().unwrap();
    for initial in [
        response(503, "Content-Length: 0\r\n", b""),
        response(200, "ETag: \"discarded\"\r\nContent-Length: 4\r\n", b"ab"),
    ] {
        let partial = initial.ends_with(b"ab");
        let platform =
            FakePlatform::new([initial, response(304, &format!("ETag: {ETAG}\r\n"), b"")]);
        let policy = FetchPolicy {
            attempts: 2,
            max_total_bytes: 4,
            ..policy()
        };
        let collector = Arc::new(Collector::default());
        let progress = super::super::progress::Progress::new(collector.clone());
        let fetcher = Fetcher::new(&policy, root.path(), platform.clone())
            .with_progress(Some(progress.clone()));
        assert!(matches!(
            fetcher
                .fetch_revalidated(
                    URL,
                    &root.path().join("new.part"),
                    None,
                    Some(&prior(URL, 4, None)),
                )
                .unwrap(),
            RevalidatedFetch::NotModified { .. }
        ));
        assert!(!root.path().join("new.part").exists());
        assert_eq!(lock(&fetcher.shared).used, 4);
        assert_eq!(platform.requests().len(), 2);
        assert!(
            platform
                .requests()
                .iter()
                .all(|request| request.contains(&format!("If-None-Match: {ETAG}\r\n")))
        );
        drop(fetcher);
        drop(progress);
        let records = collector.records();
        let last = records.last().unwrap();
        assert_eq!(last["resources_started"], 1);
        assert_eq!(last["resources_completed"], 1);
        assert_eq!(last["bytes_received"], if partial { 2 } else { 0 });
    }
}

#[test]
fn a_stale_kept_connection_retries_conditionally_without_spending_an_attempt() {
    let root = crate::native::private_tempdir().unwrap();
    let platform = FakePlatform::new([
        format!("HTTP/1.1 200 OK\r\nContent-Length: 4\r\nETag: {ETAG}\r\n\r\ndata").into_bytes(),
        response(304, &format!("ETag: {ETAG}\r\n"), b""),
    ]);
    let fetcher = Fetcher::new(&policy(), root.path(), platform.clone());
    assert!(matches!(
        fetcher
            .fetch_revalidated(URL, &root.path().join("first.part"), None, None)
            .unwrap(),
        RevalidatedFetch::Fresh { .. }
    ));
    assert_eq!(fetcher.kept(), 1);
    assert!(matches!(
        fetcher
            .fetch_revalidated(
                URL,
                &root.path().join("second.part"),
                None,
                Some(&prior(URL, 4, None))
            )
            .unwrap(),
        RevalidatedFetch::NotModified { .. }
    ));
    assert_eq!(lock(&platform.connected).len(), 2);
    assert_eq!(lock(&platform.resolved).len(), 1);
    assert_eq!(lock(&fetcher.shared).used, 8);
    let requests = platform.requests();
    assert_eq!(requests[0].matches("GET ").count(), 2);
    assert_eq!(requests[0].matches("If-None-Match").count(), 1);
    assert_eq!(requests[1].matches("If-None-Match").count(), 1);
    assert!(!root.path().join("second.part").exists());
}

#[test]
fn reuse_preserves_an_existing_destination_and_refuses_zero_ranges() {
    let root = crate::native::private_tempdir().unwrap();
    let destination = root.path().join("existing");
    crate::native::create_private_new(&destination)
        .unwrap()
        .write_all(b"sentinel")
        .unwrap();
    let platform = FakePlatform::new([response(304, &format!("ETag: {ETAG}\r\n"), b"")]);
    let fetcher = Fetcher::new(&policy(), root.path(), platform.clone());
    assert!(matches!(
        fetcher
            .fetch_revalidated(URL, &destination, None, Some(&prior(URL, 4, None)))
            .unwrap(),
        RevalidatedFetch::NotModified { .. }
    ));
    assert_eq!(std::fs::read(&destination).unwrap(), b"sentinel");
    assert_eq!(
        fetcher
            .fetch_revalidated(
                URL,
                &destination,
                Some(ByteRange {
                    offset: 0,
                    length: 0
                }),
                None
            )
            .unwrap_err()
            .token(),
        Some("invalid-byte-range")
    );
    assert_eq!(platform.requests().len(), 1);
}
