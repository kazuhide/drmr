//! Credential-safe HTTP fetching against deterministic local fixture servers.
// Keep related operations together so the test exposes the
// correspondence.
#![allow(clippy::too_many_lines)]

#[path = "source_http/budgets.rs"]
mod budgets;
mod support;

use drmr_core::source::platform::{Platform, Stream, SystemPlatform, TlsContext};
use drmr_core::source::{ByteRange, Failure, FetchPolicy, Fetcher};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use support::server::{Protocol, TestServer};
use support::{FreeSpace, Resolves, remote_policy, timing, token, ts_packet};

const PLAYLIST: &[u8] =
    b"#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:1,\nsegment.ts\n#EXT-X-ENDLIST\n";

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

/// Observe timeout resets on the real TCP connection without depending on CI scheduling.
struct SocketFault {
    operation: &'static str,
    kind: io::ErrorKind,
    code: Option<i32>,
}
struct FaultPlatform {
    inner: Arc<dyn Platform>,
    faults: Arc<Mutex<Vec<SocketFault>>>,
}
struct FaultStream {
    inner: Box<dyn Stream>,
    faults: Arc<Mutex<Vec<SocketFault>>>,
}
impl FaultStream {
    fn observe<T>(&self, operation: &'static str, result: io::Result<T>) -> io::Result<T> {
        if let Err(error) = &result {
            self.faults.lock().unwrap().push(SocketFault {
                operation,
                kind: error.kind(),
                code: error.raw_os_error(),
            });
        }
        result
    }
}
impl Read for FaultStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let result = self.inner.read(bytes);
        self.observe("read", result)
    }
}
impl Write for FaultStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = self.inner.write(bytes);
        self.observe("write", result)
    }
    fn flush(&mut self) -> io::Result<()> {
        let result = self.inner.flush();
        self.observe("flush", result)
    }
}
impl Stream for FaultStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.observe("read-timeout", self.inner.set_read_timeout(timeout))
    }
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.observe("write-timeout", self.inner.set_write_timeout(timeout))
    }
}
impl Platform for FaultPlatform {
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        self.inner.resolve(host, port)
    }
    fn connect(&self, address: SocketAddr, timeout: Duration) -> io::Result<Box<dyn Stream>> {
        Ok(Box::new(FaultStream {
            inner: self.inner.connect(address, timeout)?,
            faults: Arc::clone(&self.faults),
        }))
    }
    fn tls_context(&self) -> io::Result<TlsContext> {
        self.inner.tls_context()
    }
}
fn observed_failure(error: &Failure, faults: &Mutex<Vec<SocketFault>>) -> String {
    let faults: Vec<_> = faults
        .lock()
        .unwrap()
        .iter()
        .map(|fault| (fault.operation, fault.kind, fault.code))
        .collect();
    format!("{error:?}; socket classifications: {faults:?}")
}

#[test]
fn close_all_drops_every_kept_connection() {
    let tmp = tempfile::tempdir().unwrap();
    let private = tmp.path().join("private");
    drmr_core::native::create_private_dir_all(&private).unwrap();
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
        &private,
        Arc::new(drmr_core::source::platform::SystemPlatform),
    );
    fetcher
        .fetch(
            &format!("{}/a.ts", server.origin),
            &private.join("a.part"),
            None,
        )
        .unwrap();
    fetcher
        .fetch(
            &format!("{}/b.ts", server.origin),
            &private.join("b.part"),
            None,
        )
        .unwrap();
    assert!(fetcher.kept() > 0 && server.connection_count() == 1);
    fetcher.close_all();
    assert_eq!(fetcher.kept(), 0);
    fetcher
        .fetch(
            &format!("{}/a.ts", server.origin),
            &private.join("a2.part"),
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
    let faults = Arc::new(Mutex::new(Vec::new()));
    let fetcher = Fetcher::new(
        &policy,
        tmp.path(),
        Arc::new(FaultPlatform {
            inner: Arc::new(LoopbackTls { address, context }),
            faults: Arc::clone(&faults),
        }),
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
        .unwrap_or_else(|error| panic!("{}", observed_failure(&error, &faults)));
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

#[test]
fn requests_keep_origin_scoped_headers_and_exact_identity_encoding() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let server = TestServer::routes(vec![(
        "/manifest?token=sentinel",
        200,
        vec![],
        PLAYLIST.to_vec(),
    )]);
    let policy = FetchPolicy {
        headers_by_origin: vec![(
            server.origin.clone(),
            vec![
                ("Authorization".into(), "sentinel-auth".into()),
                ("X-One".into(), "1".into()),
            ],
        )],
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(&policy, root.path(), Arc::new(SystemPlatform));
    let (body, effective) = fetcher
        .read(
            &format!("{}/manifest?token=sentinel", server.origin),
            true,
            None,
        )
        .unwrap();
    assert_eq!(body, PLAYLIST);
    assert_eq!(
        effective,
        format!("{}/manifest?token=sentinel", server.origin)
    );
    assert_eq!(server.paths(), ["/manifest?token=sentinel"]);
    let request = &server.requests()[0];
    assert_eq!(request.header("Authorization"), Some("sentinel-auth"));
    assert_eq!(request.header("Accept-Encoding"), Some("identity"));
    assert!(request.header("User-Agent").is_none());
    assert!(!format!("{policy:?} {fetcher:?}").contains("sentinel"));
}

#[test]
fn redirects_recheck_allowed_origins_and_never_forward_another_origins_auth() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let destination = TestServer::routes(vec![("/entry", 200, vec![], PLAYLIST.to_vec())]);
    let location = format!("{}/entry", destination.origin);
    let origin = TestServer::routes(vec![(
        "/start",
        302,
        vec![("Location", &location)],
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
    let fetcher = Fetcher::new(&policy, root.path(), Arc::new(SystemPlatform));
    assert_eq!(
        fetcher
            .read(&format!("{}/start", origin.origin), true, None)
            .unwrap()
            .0,
        PLAYLIST
    );
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
    let denied = Fetcher::new(
        &remote_policy(&origin.origin),
        root.path(),
        Arc::new(SystemPlatform),
    );
    assert_eq!(
        token(denied.read(&format!("{}/start", origin.origin), true, None)),
        "source-not-allowed"
    );
    assert_eq!(destination.paths(), ["/entry"]);
}

#[test]
fn redirect_loops_and_missing_locations_have_fixed_failures() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let looped = TestServer::start(Protocol::Http10, |exchange| {
        exchange.send_response(302);
        exchange.send_header("Location", "/again");
        exchange.end_headers();
    });
    let fetcher = Fetcher::new(
        &remote_policy(&looped.origin),
        root.path(),
        Arc::new(SystemPlatform),
    );
    assert_eq!(
        token(fetcher.read(&format!("{}/start", looped.origin), true, None)),
        "redirect-budget-exceeded"
    );
    assert_eq!(looped.paths().len(), 6);
    let bare = TestServer::routes(vec![("/start", 302, vec![], Vec::new())]);
    let fetcher = Fetcher::new(
        &remote_policy(&bare.origin),
        root.path(),
        Arc::new(SystemPlatform),
    );
    assert_eq!(
        token(fetcher.read(&format!("{}/start", bare.origin), true, None)),
        "invalid-source-redirect"
    );
}

#[test]
fn private_dns_and_unapproved_http_are_refused_before_a_request() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let policy = FetchPolicy {
        allowed_origins: vec!["https://media.example".into()],
        ..FetchPolicy::default()
    };
    let fetcher = Fetcher::new(
        &policy,
        root.path(),
        Arc::new(Resolves(vec!["169.254.169.254:443".parse().unwrap()])),
    );
    let error = fetcher
        .read("https://media.example/manifest?token=private", true, None)
        .unwrap_err();
    assert_eq!(error.token(), Some("source-not-allowed"));
    assert!(!format!("{error} {error:?}").contains("private"));
    let server = TestServer::routes(vec![("/entry", 200, vec![], PLAYLIST.to_vec())]);
    for policy in [
        FetchPolicy {
            allow_private_network: false,
            ..remote_policy(&server.origin)
        },
        FetchPolicy {
            allow_http: false,
            ..remote_policy(&server.origin)
        },
    ] {
        let fetcher = Fetcher::new(&policy, root.path(), Arc::new(SystemPlatform));
        assert_eq!(
            token(fetcher.read(&format!("{}/entry", server.origin), true, None)),
            "source-not-allowed"
        );
    }
    assert!(server.paths().is_empty());
}

#[test]
fn truncated_resources_retry_without_leaving_a_partial_file() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let server = TestServer::routes(vec![(
        "/segment",
        200,
        vec![("Content-Length", "999")],
        ts_packet("", b'x'),
    )]);
    let policy = FetchPolicy {
        attempts: 2,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(&policy, root.path(), Arc::new(SystemPlatform));
    let target = root.path().join("segment.part");
    assert_eq!(
        token(fetcher.fetch(&format!("{}/segment", server.origin), &target, None)),
        "source-fetch-failed"
    );
    assert_eq!(server.paths().len(), 2);
    assert!(!target.exists());
}

#[test]
fn retry_policy_distinguishes_server_errors_and_missing_resources() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let server = TestServer::start(Protocol::Http11, |exchange| {
        exchange.send_response(503);
        exchange.send_header("Content-Length", "0");
        exchange.end_headers();
    });
    let policy = FetchPolicy {
        attempts: 3,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(&policy, root.path(), Arc::new(SystemPlatform));
    assert_eq!(
        token(fetcher.read(&format!("{}/entry", server.origin), true, None)),
        "source-http-failed"
    );
    assert_eq!(server.paths().len(), 3);
    let missing = TestServer::routes11(vec![]);
    let fetcher = Fetcher::new(
        &remote_policy(&missing.origin),
        root.path(),
        Arc::new(SystemPlatform),
    );
    assert_eq!(
        token(fetcher.read(&format!("{}/missing", missing.origin), true, None)),
        "source-http-failed"
    );
    assert_eq!(missing.paths().len(), 1);
}

#[test]
fn unexpected_compression_and_inexact_content_ranges_are_refused() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let compressed = TestServer::routes(vec![(
        "/entry",
        200,
        vec![("Content-Encoding", "gzip")],
        PLAYLIST.to_vec(),
    )]);
    let fetcher = Fetcher::new(
        &remote_policy(&compressed.origin),
        root.path(),
        Arc::new(SystemPlatform),
    );
    assert_eq!(
        token(fetcher.read(&format!("{}/entry", compressed.origin), true, None)),
        "unsupported-content-encoding"
    );
    let range = ByteRange {
        offset: 2,
        length: 4,
    };
    for (header, expected) in [
        ("bytes 0-3/6", None),
        ("bytes 2-5/6", Some(b"abcd".to_vec())),
    ] {
        let server = TestServer::routes(vec![(
            "/data",
            206,
            vec![("Content-Range", header)],
            b"abcd".to_vec(),
        )]);
        let fetcher = Fetcher::new(
            &remote_policy(&server.origin),
            root.path(),
            Arc::new(SystemPlatform),
        );
        let result = fetcher.read(&format!("{}/data", server.origin), false, Some(range));
        match expected {
            Some(body) => assert_eq!(result.unwrap().0, body),
            None => assert_eq!(token(result), "invalid-byte-range"),
        }
        assert_eq!(server.requests()[0].header("Range"), Some("bytes=2-5"));
    }
}

#[test]
fn declared_lengths_respect_disk_headroom_before_writing_media() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let body = ts_packet("", b'x').repeat(50);
    let server = TestServer::routes(vec![(
        "/segment",
        200,
        vec![("Content-Length", "9400")],
        body,
    )]);
    let policy = FetchPolicy {
        attempts: 1,
        min_free_disk_bytes: 1_000_000,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(
        &policy,
        root.path(),
        Arc::new(FreeSpace(|_: &Path| 1_005_000)),
    );
    let target = root.path().join("segment.part");
    assert_eq!(
        token(fetcher.fetch(&format!("{}/segment", server.origin), &target, None)),
        "disk-budget-exceeded"
    );
    assert!(!target.exists());
}
