use super::*;
use crate::storage::{S3Access, S3StorageBackend, TransferPolicy};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use ureq::unversioned::resolver::Resolver;

struct Fixture {
    endpoint: String,
    trust: ureq::tls::TlsConfig,
    worker: JoinHandle<usize>,
}

fn tls_fixture() -> Fixture {
    let certificate =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
            .unwrap();
    let der = certificate.cert.der().clone();
    let trust = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::new_with_certs(&[
            ureq::tls::Certificate::from_der(&der).to_owned(),
        ]))
        .build();
    let key =
        rustls::pki_types::PrivateKeyDer::Pkcs8(certificate.signing_key.serialize_der().into());
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![der], key)
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("https://{}", listener.local_addr().unwrap());
    let worker = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return 0;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("fixture accept failed: {error:?}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        let mut stream = rustls::StreamOwned::new(connection, stream);
        for index in 0..2 {
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                if stream.read_exact(&mut byte).is_err() {
                    return index;
                }
                request.push(byte[0]);
            }
            let connection = if index == 0 { "keep-alive" } else { "close" };
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: {connection}\r\n\r\n"
            )
            .unwrap();
            for byte in b"abcdefgh" {
                if index == 0 {
                    std::thread::sleep(Duration::from_millis(100));
                }
                stream.write_all(&[*byte]).unwrap();
                stream.flush().unwrap();
            }
        }
        stream.conn.send_close_notify();
        stream.flush().unwrap();
        2
    });
    Fixture {
        endpoint,
        trust,
        worker,
    }
}

#[test]
fn normal_tls_keeps_idle_progress_and_reuses_the_authenticated_connection() {
    let fixture = tls_fixture();
    let idle = Duration::from_millis(300);
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(idle))
        .max_redirects(0)
        .http_status_as_error(false)
        .proxy(None)
        .tls_config(fixture.trust)
        .build();
    let agent = with_config(config, idle);
    for _ in 0..2 {
        let mut response = agent.get(&fixture.endpoint).call().unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.body_mut().read_to_string().unwrap(), "abcdefgh");
    }
    assert_eq!(fixture.worker.join().unwrap(), 2);
}

#[test]
fn the_default_trust_store_rejects_a_self_signed_peer_without_retrying() {
    let fixture = tls_fixture();
    let access = S3Access {
        endpoint: fixture.endpoint,
        access_key: "AKIDSYNTHETICTEST".to_string(),
        secret_key: "sentinel-synthetic-secret".to_string(),
        session_token: None,
    };
    let policy = TransferPolicy {
        per_object_timeout_s: 0.3,
        max_attempts: 3,
        backoff_base_s: 0.0,
        ..TransferPolicy::default()
    };
    let error = S3StorageBackend::with_access("b", "ap-northeast-1", policy, "output", &access)
        .unwrap_err();
    assert_eq!(error.token(), "storage-failed");
    assert!(!error.is_retryable());
    assert!(!format!("{error} {error:?}").contains("sentinel"));
    assert_eq!(fixture.worker.join().unwrap(), 0);
}

#[test]
fn unsupported_schemes_are_refused_before_the_transport_is_used() {
    assert!(matches!(
        agent(Duration::from_secs(1))
            .get("ftp://127.0.0.1:1/sentinel")
            .call(),
        Err(ureq::Error::BadUri(_))
    ));
}

// Prepare TCP before the measured call so peer scheduling cannot consume the TLS budget.
// The production IdleConnector and FinishingTlsConnector still wrap a real TCP transport
// and a real rustls handshake. transport.rs separately checks pre-TCP/expired budgets.
type PreparedTcpTransport = <TcpConnector as Connector<()>>::Out;

#[derive(Debug, Default, Clone)]
struct PartialTlsProgress {
    client_hello_prefix: Vec<u8>,
    peer_client_hello_prefix: Vec<u8>,
    response_prefix: Vec<u8>,
    loaded_bytes: usize,
    consumed_bytes: usize,
    successful_reads: usize,
    paced_reads: usize,
}

impl PartialTlsProgress {
    fn exercised_partial_record(&self) -> bool {
        self.client_hello_prefix.len() == 6
            && self.client_hello_prefix[..2] == [22, 3]
            && self.client_hello_prefix[5] == 1
            && self.peer_client_hello_prefix == self.client_hello_prefix
            && self.response_prefix == [22, 3, 3, 0x40, 0, 0]
            && self.consumed_bytes >= 6
            && self.successful_reads >= 7
            && self.paced_reads > 0
    }
}

#[derive(Debug)]
struct ConsumingTlsBuffers {
    inner: ureq::unversioned::transport::LazyBuffers,
    progress: Arc<Mutex<PartialTlsProgress>>,
}

impl Buffers for ConsumingTlsBuffers {
    fn output(&mut self) -> &mut [u8] {
        self.inner.output()
    }

    fn input(&self) -> &[u8] {
        self.inner.input()
    }

    fn input_append_buf(&mut self) -> &mut [u8] {
        self.inner.input_append_buf()
    }

    fn input_appended(&mut self, amount: usize) {
        self.inner.input_appended(amount);
    }

    fn input_consume(&mut self, amount: usize) {
        // TransportAdapter calls this only after copying these bytes into rustls.
        // Merely filling a socket/input buffer cannot satisfy the handshake witness.
        let mut progress = self.progress.lock().unwrap();
        let remaining = 6 - progress.response_prefix.len();
        progress
            .response_prefix
            .extend_from_slice(&self.inner.input()[..amount.min(remaining)]);
        self.inner.input_consume(amount);
        progress.consumed_bytes += amount;
    }

    fn tmp_and_output(&mut self) -> (&mut [u8], &mut [u8]) {
        self.inner.tmp_and_output()
    }

    fn can_use_input(&self) -> bool {
        self.inner.can_use_input()
    }
}

#[derive(Debug)]
struct PartialTlsTransport {
    inner: PreparedTcpTransport,
    peer: std::net::TcpStream,
    buffers: ConsumingTlsBuffers,
    progress: Arc<Mutex<PartialTlsProgress>>,
}

impl Transport for PartialTlsTransport {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }

    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        let output = &self.buffers.output()[..amount];
        let prefix = output[..amount.min(6)].to_vec();
        self.inner.buffers().output()[..amount].copy_from_slice(output);
        self.inner.transmit_output(amount, timeout)?;
        let mut progress = self.progress.lock().unwrap();
        let remaining = 6 - progress.client_hello_prefix.len();
        progress
            .client_hello_prefix
            .extend_from_slice(&prefix[..prefix.len().min(remaining)]);
        // Observe only bytes just successfully written, while both sockets remain open.
        // This bounded drain cannot race timeout teardown or wait for a later write.
        let mut received = [0; 6];
        let amount = amount.min(6 - progress.peer_client_hello_prefix.len());
        if amount > 0 && self.peer.read_exact(&mut received[..amount]).is_ok() {
            progress
                .peer_client_hello_prefix
                .extend_from_slice(&received[..amount]);
        }
        Ok(())
    }

    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        // Deliver the header and first two payload bytes immediately. Subsequent real socket
        // reads make progress every 5ms, so a restarted 50ms timeout cannot bound the call.
        if self.progress.lock().unwrap().loaded_bytes >= 7 {
            std::thread::sleep(Duration::from_millis(5));
            self.progress.lock().unwrap().paced_reads += 1;
        }
        let received = self.inner.await_input(timeout)?;
        if received {
            let input = self.inner.buffers().input();
            let amount = input.len();
            self.buffers.input_append_buf()[..amount].copy_from_slice(input);
            self.buffers.input_appended(amount);
            self.inner.buffers().input_consume(amount);
            let mut progress = self.progress.lock().unwrap();
            progress.loaded_bytes += amount;
            progress.successful_reads += 1;
        }
        Ok(received)
    }

    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }
}

#[derive(Debug)]
struct PreparedTlsConnector(Mutex<Option<PartialTlsTransport>>);

impl Connector<()> for PreparedTlsConnector {
    type Out = PartialTlsTransport;

    fn connect(
        &self,
        _: &ConnectionDetails,
        _: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(Some(
            self.0
                .lock()
                .unwrap()
                .take()
                .expect("one fixture connection"),
        ))
    }
}

// Negative control: disable only the actual production absolute-deadline clamp.
#[derive(Debug)]
struct DisabledConnectionBudget;

impl<T: Transport> Connector<IdleTransport<T>> for DisabledConnectionBudget {
    type Out = IdleTransport<T>;

    fn connect(
        &self,
        _: &ConnectionDetails,
        chained: Option<IdleTransport<T>>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        if let Some(transport) = &chained {
            transport.connect.active.store(false, Ordering::Release);
        }
        Ok(chained)
    }
}

#[derive(Debug, Clone, Copy)]
enum HandshakeFixture {
    PartialRecord,
    NoPartialResponse,
    WithoutConnectionBudget,
}

#[derive(Debug)]
struct HandshakeObservation {
    result: Result<(), ureq::Error>,
    elapsed: Duration,
    peer_received_client_hello: bool,
    progress: PartialTlsProgress,
}

fn observe_handshake_budget(
    reason: ureq::Timeout,
    fixture: HandshakeFixture,
) -> HandshakeObservation {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let idle = Duration::from_millis(300);
    let short = Duration::from_millis(50);
    // No certificate is sent by this incomplete-record peer. Empty trust roots avoid
    // charging unrelated default-root initialization to the short handshake budget.
    let trust = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::new_with_certs(&[]))
        .build();
    let config = ureq::Agent::config_builder()
        .timeout_connect(Some(idle))
        .timeout_global((reason == ureq::Timeout::Global).then_some(short))
        .timeout_per_call((reason == ureq::Timeout::PerCall).then_some(short))
        .proxy(None)
        .input_buffer_size(1)
        .tls_config(trust)
        .build();
    // Use ureq's actual TCP connector before starting the timed Agent call.
    // These preparation details never replace the real call's timing or deadline.
    let uri = format!("https://{address}").parse().unwrap();
    let resolver = DefaultResolver::default();
    let preparation_timeout = NextTimeout {
        after: Duration::from_secs(2).into(),
        reason: ureq::Timeout::Connect,
    };
    let details = ConnectionDetails {
        uri: &uri,
        addrs: resolver
            .resolve(&uri, &config, preparation_timeout)
            .unwrap(),
        config: &config,
        request_level: false,
        resolver: &resolver,
        now: TransportInstant::Exact(Instant::now()),
        timeout: preparation_timeout,
        current_time: Arc::new(|| TransportInstant::Exact(Instant::now())),
        run_connector: Arc::new(|_| unreachable!("fixture has no proxy")),
    };
    let prepared = TcpConnector::default()
        .connect(&details, None::<()>)
        .unwrap()
        .unwrap();
    let (mut peer, _) = listener.accept().unwrap();
    peer.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    peer.set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    if !matches!(fixture, HandshakeFixture::NoPartialResponse) {
        let mut partial_record = [0; 133];
        partial_record[..5].copy_from_slice(&[22, 3, 3, 0x40, 0]);
        peer.write_all(&partial_record).unwrap();
    }
    let progress = Arc::new(Mutex::new(PartialTlsProgress::default()));
    let connector = PreparedTlsConnector(Mutex::new(Some(PartialTlsTransport {
        inner: prepared,
        peer,
        buffers: ConsumingTlsBuffers {
            inner: ureq::unversioned::transport::LazyBuffers::new(1, config.output_buffer_size()),
            progress: Arc::clone(&progress),
        },
        progress: Arc::clone(&progress),
    })));
    let agent = if matches!(fixture, HandshakeFixture::WithoutConnectionBudget) {
        ureq::Agent::with_parts(
            config,
            ().chain(connector)
                .chain(IdleConnector(idle))
                .chain(DisabledConnectionBudget)
                .chain(FinishingTlsConnector::default()),
            DefaultResolver::default(),
        )
    } else {
        ureq::Agent::with_parts(
            config,
            ().chain(connector)
                .chain(IdleConnector(idle))
                .chain(FinishingTlsConnector::default()),
            DefaultResolver::default(),
        )
    };
    let started = Instant::now();
    let result = agent.get(format!("https://{address}")).call().map(|_| ());
    let elapsed = started.elapsed();
    let progress = progress.lock().unwrap().clone();
    let peer_received_client_hello = progress.client_hello_prefix.len() == 6
        && progress.client_hello_prefix[..2] == [22, 3]
        && progress.client_hello_prefix[5] == 1
        && progress.peer_client_hello_prefix == progress.client_hello_prefix;
    let observation = HandshakeObservation {
        result,
        elapsed,
        peer_received_client_hello,
        progress,
    };
    println!("earliest TLS {reason:?} {fixture:?}: {observation:?}");
    observation
}

fn assert_earliest_handshake_timeout(observation: &HandshakeObservation, reason: ureq::Timeout) {
    assert!(
        matches!(&observation.result, Err(ureq::Error::Timeout(actual)) if *actual == reason),
        "{observation:?}"
    );
    assert!(
        observation.elapsed < Duration::from_millis(500),
        "{reason:?}: {observation:?}"
    );
}

fn check_earliest_handshake_budget(reason: ureq::Timeout) {
    let observation = observe_handshake_budget(reason, HandshakeFixture::PartialRecord);
    assert_earliest_handshake_timeout(&observation, reason);
    assert!(observation.peer_received_client_hello, "{observation:?}");
    assert!(
        observation.progress.exercised_partial_record(),
        "{observation:?}"
    );
}

#[test]
fn a_global_budget_bounds_partial_tls_records_during_connect() {
    check_earliest_handshake_budget(ureq::Timeout::Global);
}

#[test]
fn a_per_call_budget_bounds_partial_tls_records_during_connect() {
    check_earliest_handshake_budget(ureq::Timeout::PerCall);
}

#[test]
fn a_timeout_without_partial_tls_progress_is_not_handshake_evidence() {
    for reason in [ureq::Timeout::Global, ureq::Timeout::PerCall] {
        let observation = observe_handshake_budget(reason, HandshakeFixture::NoPartialResponse);
        assert_earliest_handshake_timeout(&observation, reason);
        assert!(observation.peer_received_client_hello, "{observation:?}");
        assert_eq!(observation.progress.consumed_bytes, 0, "{observation:?}");
        assert!(
            !observation.progress.exercised_partial_record(),
            "{observation:?}"
        );
    }
}

#[test]
fn repeated_partial_tls_progress_without_the_connection_budget_exceeds_the_limit() {
    for reason in [ureq::Timeout::Global, ureq::Timeout::PerCall] {
        let observation =
            observe_handshake_budget(reason, HandshakeFixture::WithoutConnectionBudget);
        assert!(
            matches!(&observation.result, Err(ureq::Error::Timeout(actual)) if *actual == reason),
            "{observation:?}"
        );
        assert!(observation.peer_received_client_hello, "{observation:?}");
        assert!(
            observation.progress.exercised_partial_record(),
            "{observation:?}"
        );
        assert!(
            observation.elapsed >= Duration::from_millis(500),
            "{observation:?}"
        );
        assert!(
            observation.elapsed < Duration::from_secs(5),
            "{observation:?}"
        );
    }
}
