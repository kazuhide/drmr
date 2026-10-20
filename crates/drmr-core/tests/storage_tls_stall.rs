use drmr_core::storage::{S3Access, S3StorageBackend, TransferPolicy};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[test]
fn a_stalled_tls_peer_respects_the_connection_timeout() {
    check_peer(false);
}

#[test]
fn a_trickling_tls_peer_respects_the_connection_timeout() {
    check_peer(true);
}

fn check_peer(trickle: bool) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("https://{}", listener.local_addr().unwrap());
    let stopped = Arc::new(AtomicBool::new(false));
    let stop = Arc::clone(&stopped);
    let peer = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut connections = Vec::new();
        let mut hellos = 0;
        while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_millis(100)))
                        .unwrap();
                    let mut header = [0; 5];
                    if stream.read_exact(&mut header).is_ok() && header[..2] == [22, 3] {
                        hellos += 1;
                    }
                    if trickle {
                        stream.write_all(&[22, 3, 3, 0x40, 0]).unwrap();
                        while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
                            if stream.write_all(&[0]).is_err() {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                    connections.push(stream);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("fixture accept failed: {error:?}"),
            }
        }
        (connections.len(), hellos)
    });
    let policy = TransferPolicy {
        per_object_timeout_s: 0.2,
        max_attempts: 1,
        backoff_base_s: 0.0,
        ..TransferPolicy::default()
    };
    let access = S3Access {
        endpoint,
        access_key: "AKIDSYNTHETICTEST".to_string(),
        secret_key: "sentinel-synthetic-secret".to_string(),
        session_token: None,
    };
    let started = Instant::now();
    let error = S3StorageBackend::with_access("b", "ap-northeast-1", policy, "output", &access)
        .unwrap_err();
    let elapsed = started.elapsed();
    stopped.store(true, Ordering::Release);
    let observed = peer.join().unwrap();
    assert_eq!(observed, (1, 1));
    assert_eq!(error.token(), "storage-transient");
    assert!(error.is_retryable());
    assert!(!format!("{error} {error:?}").contains("sentinel"));
    assert!(elapsed < Duration::from_millis(1200), "{elapsed:?}");
    println!("TLS peer trickle={trickle}: {observed:?}, elapsed={elapsed:?}");
}
