use super::*;
use crate::storage::{S3Access, S3StorageBackend, TransferPolicy};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::thread::JoinHandle;

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
