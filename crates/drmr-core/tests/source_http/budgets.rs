//! Shared fetch budgets and socket deadlines, without the retired analysis workflow.

use super::support::server::{Exchange, Protocol, TestServer};
use super::support::{FreeSpace, remote_policy, timing, token};
use drmr_core::source::platform::{Platform, Stream, SystemPlatform};
use drmr_core::source::{FetchPolicy, Fetcher};
use std::cell::Cell;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

#[derive(Clone, Default)]
struct HeldBody(Arc<(Mutex<bool>, Condvar)>);

impl HeldBody {
    fn wait(&self) {
        let (lock, changed) = &*self.0;
        let _guard = changed
            .wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(10), |released| {
                !*released
            })
            .unwrap();
    }

    fn release(&self) {
        let (lock, changed) = &*self.0;
        *lock.lock().unwrap() = true;
        changed.notify_all();
    }
}

fn chunk(exchange: &mut Exchange<'_>, bytes: &[u8]) -> io::Result<()> {
    let mut frame = format!("{:x}\r\n", bytes.len()).into_bytes();
    frame.extend(bytes);
    frame.extend(b"\r\n");
    exchange.write(&frame)
}

fn written_bytes(root: &Path) -> u64 {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum()
}

#[test]
fn concurrent_declared_bodies_are_claimed_against_the_disk_floor_together() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let held = HeldBody::default();
    let server = TestServer::start(Protocol::Http11, {
        let held = held.clone();
        move |exchange| {
            exchange.send_response(200);
            exchange.send_header("Content-Length", "9400");
            exchange.end_headers();
            held.wait();
            let _ = exchange.write(&vec![b'x'; 9400]);
        }
    });
    let policy = FetchPolicy {
        connections: 2,
        attempts: 1,
        min_free_disk_bytes: 1_000_000,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(
        &policy,
        root.path(),
        Arc::new(FreeSpace(|_: &Path| 1_012_000)),
    );
    let (first, second) = std::thread::scope(|scope| {
        let (sent, received) = mpsc::channel();
        for index in 0..2 {
            let sent = sent.clone();
            let fetcher = &fetcher;
            let url = format!("{}/segment{index}", server.origin);
            let output = root.path().join(format!("segment{index}.part"));
            scope.spawn(move || sent.send(fetcher.fetch(&url, &output, None)).unwrap());
        }
        // Neither response sends a body until the first reservation has been refused.
        let first = received.recv_timeout(Duration::from_secs(5));
        held.release();
        let second = received.recv_timeout(Duration::from_secs(5));
        (first, second)
    });
    assert_eq!(token(first.unwrap()), "disk-budget-exceeded");
    assert_eq!(second.unwrap().unwrap().size, 9400);
    assert_eq!(server.paths().len(), 2);
    assert_eq!(written_bytes(root.path()), 9400);
}

#[test]
fn a_landing_declared_body_releases_its_claim_before_another_body_is_admitted() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let held = HeldBody::default();
    let server = TestServer::start(Protocol::Http11, {
        let held = held.clone();
        move |exchange| {
            if exchange.path() == "/second" {
                return exchange.respond(&vec![b'b'; 4 * 1024]);
            }
            exchange.send_response(200);
            exchange.send_header("Content-Length", &(16 * 1024).to_string());
            exchange.end_headers();
            let _ = exchange.write(&vec![b'a'; 8 * 1024]);
            held.wait();
            let _ = exchange.write(&vec![b'a'; 8 * 1024]);
        }
    });
    let policy = FetchPolicy {
        connections: 2,
        attempts: 1,
        min_free_disk_bytes: 1_000_000,
        ..remote_policy(&server.origin)
    };
    let directory = root.path().to_path_buf();
    let fetcher = Fetcher::new(
        &policy,
        root.path(),
        Arc::new(FreeSpace(move |_: &Path| {
            (1_000_000 + 20 * 1024_u64).saturating_sub(written_bytes(&directory))
        })),
    );
    let first_path = root.path().join("first.part");
    let second_path = root.path().join("second.part");
    std::thread::scope(|scope| {
        let first =
            scope.spawn(|| fetcher.fetch(&format!("{}/first", server.origin), &first_path, None));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !first_path.metadata().is_ok_and(|m| m.len() >= 8 * 1024) && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(1));
        }
        let landed = first_path.metadata().is_ok_and(|m| m.len() == 8 * 1024);
        if !landed {
            held.release();
        }
        assert!(
            landed,
            "the first body must be held with half its bytes on disk"
        );
        let second = fetcher.fetch(&format!("{}/second", server.origin), &second_path, None);
        held.release();
        assert_eq!(first.join().unwrap().unwrap().size, 16 * 1024);
        assert_eq!(second.unwrap().size, 4 * 1024);
    });
    assert_eq!(written_bytes(root.path()), 20 * 1024);
    assert_eq!(std::fs::read(first_path).unwrap(), vec![b'a'; 16 * 1024]);
    assert_eq!(std::fs::read(second_path).unwrap(), vec![b'b'; 4 * 1024]);
}

#[test]
fn a_truncated_response_refunds_its_bytes_before_the_retry() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = TestServer::start(Protocol::Http11, {
        let calls = Arc::clone(&calls);
        move |exchange| {
            let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
            exchange.send_response(200);
            exchange.send_header("Content-Length", "188");
            exchange.end_headers();
            let _ = exchange.write(&vec![b'x'; if first { 100 } else { 188 }]);
            exchange.close_connection = first;
        }
    });
    let policy = FetchPolicy {
        attempts: 2,
        max_total_bytes: 376,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(&policy, root.path(), Arc::new(SystemPlatform));
    for index in 0..2 {
        let path = root.path().join(format!("segment{index}.part"));
        assert_eq!(
            fetcher
                .fetch(&format!("{}/segment{index}", server.origin), &path, None)
                .unwrap()
                .size,
            188
        );
        assert_eq!(std::fs::read(path).unwrap(), vec![b'x'; 188]);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(written_bytes(root.path()), 376);
}

#[test]
fn an_undeclared_body_is_checked_against_the_floor_every_eight_mebibytes() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let directory = root.path().to_path_buf();
    let checks = Arc::new(AtomicUsize::new(0));
    let platform = Arc::new(FreeSpace({
        let checks = Arc::clone(&checks);
        move |_: &Path| {
            checks.fetch_add(1, Ordering::SeqCst);
            let written = written_bytes(&directory);
            if (15 * 512 * 1024..17 * 512 * 1024).contains(&written) {
                999_999
            } else {
                1_000_000 + (1 << 30)
            }
        }
    }));
    let server = TestServer::start(Protocol::Http11, |exchange| {
        exchange.send_response(200);
        exchange.send_header("Transfer-Encoding", "chunked");
        exchange.end_headers();
        let bytes = vec![b'x'; 256 * 1024];
        for _ in 0..36 {
            if chunk(exchange, &bytes).is_err() {
                return;
            }
        }
        let _ = exchange.write(b"0\r\n\r\n");
    });
    let policy = FetchPolicy {
        attempts: 1,
        min_free_disk_bytes: 1_000_000,
        max_total_bytes: 32 * 1024 * 1024,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(&policy, root.path(), platform);
    let path = root.path().join("segment.part");
    assert_eq!(
        token(fetcher.fetch(&format!("{}/segment", server.origin), &path, None)),
        "disk-budget-exceeded"
    );
    assert!(!path.exists());
    assert!(checks.load(Ordering::SeqCst) >= 1);
}

fn paced_server(declared: bool) -> TestServer {
    TestServer::start(Protocol::Http11, move |exchange| {
        exchange.send_response(200);
        if declared {
            exchange.send_header("Content-Length", &(3 * 1024 * 1024).to_string());
        } else {
            exchange.send_header("Transfer-Encoding", "chunked");
        }
        exchange.end_headers();
        let bytes = vec![b'x'; 512 * 1024];
        for _ in 0..6 {
            let written = if declared {
                exchange.write(&bytes)
            } else {
                chunk(exchange, &bytes)
            };
            if written.is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        if !declared {
            let _ = exchange.write(b"0\r\n\r\n");
        }
    })
}

#[test]
fn declared_and_undeclared_bodies_earn_one_timeout_per_mebibyte() {
    let _timing = timing();
    for declared in [true, false] {
        let root = drmr_core::native::private_tempdir().unwrap();
        let server = paced_server(declared);
        let policy = FetchPolicy {
            timeout: 1.0,
            attempts: 1,
            max_total_bytes: 8 * 1024 * 1024,
            ..remote_policy(&server.origin)
        };
        let fetcher = Fetcher::new(&policy, root.path(), Arc::new(SystemPlatform));
        let path = root.path().join("segment.part");
        let started = Instant::now();
        let response = fetcher
            .fetch(&format!("{}/segment", server.origin), &path, None)
            .unwrap();
        assert!(started.elapsed() > Duration::from_secs(1));
        assert_eq!(response.size, 3 * 1024 * 1024);
        assert_eq!(std::fs::read(path).unwrap(), vec![b'x'; 3 * 1024 * 1024]);
        assert_eq!(server.paths(), ["/segment"]);
    }
}

#[test]
fn a_stalled_body_is_bounded_by_one_chunk_timeout() {
    let _timing = timing();
    let root = drmr_core::native::private_tempdir().unwrap();
    let held = HeldBody::default();
    let server = TestServer::start(Protocol::Http11, {
        let held = held.clone();
        move |exchange| {
            exchange.send_response(200);
            exchange.send_header("Content-Length", &(9 * 1024 * 1024).to_string());
            exchange.end_headers();
            let _ = exchange.write(b"first bytes");
            held.wait();
        }
    });
    let policy = FetchPolicy {
        timeout: 0.5,
        attempts: 1,
        max_total_bytes: 16 * 1024 * 1024,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(&policy, root.path(), Arc::new(SystemPlatform));
    let path = root.path().join("segment.part");
    let started = Instant::now();
    let result = fetcher.fetch(&format!("{}/segment", server.origin), &path, None);
    let elapsed = started.elapsed();
    held.release();
    assert_eq!(token(result), "source-fetch-failed");
    assert!(
        elapsed < Duration::from_millis(1800),
        "chunk timeout: {elapsed:?}"
    );
    assert!(!path.exists());
    assert_eq!(server.paths(), ["/segment"]);
}

#[test]
fn a_timeout_on_a_reused_connection_spends_the_attempt() {
    let _timing = timing();
    let root = drmr_core::native::private_tempdir().unwrap();
    let held = HeldBody::default();
    let server = TestServer::start(Protocol::Http11, {
        let held = held.clone();
        move |exchange| {
            if exchange.path() == "/slow" {
                held.wait();
            }
            exchange.respond(b"data");
        }
    });
    let policy = FetchPolicy {
        connections: 1,
        timeout: 0.5,
        attempts: 1,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(&policy, root.path(), Arc::new(SystemPlatform));
    assert_eq!(
        fetcher
            .read(&format!("{}/fast", server.origin), false, None)
            .unwrap()
            .0,
        b"data"
    );
    assert_eq!(fetcher.kept(), 1);
    let result = fetcher.read(&format!("{}/slow", server.origin), false, None);
    held.release();
    assert_eq!(token(result), "source-fetch-failed");
    assert_eq!(server.paths(), ["/fast", "/slow"]);
    assert_eq!(server.connection_count(), 1);
}

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
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let read = self.socket.read(bytes);
        self.fresh_read.set(false);
        self.fresh_write.set(false);
        self.writing = false;
        read
    }
}

impl Write for TimeoutStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if !self.writing {
            self.requests.lock().unwrap().push((
                self.connection,
                self.fresh_read.get(),
                self.fresh_write.get(),
            ));
            self.writing = true;
        }
        self.socket.write(bytes)
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
fn every_reused_request_resets_both_socket_timeouts() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let server = TestServer::routes11(vec![
        ("/one".into(), 200, b"one".to_vec()),
        ("/two".into(), 200, b"two".to_vec()),
    ]);
    let requests = Arc::new(Mutex::new(Vec::new()));
    let policy = FetchPolicy {
        connections: 1,
        timeout: 2.0,
        attempts: 1,
        ..remote_policy(&server.origin)
    };
    let fetcher = Fetcher::new(
        &policy,
        root.path(),
        Arc::new(TimeoutPlatform {
            connections: AtomicUsize::new(0),
            requests: Arc::clone(&requests),
        }),
    );
    for name in ["one", "two"] {
        assert_eq!(
            fetcher
                .read(&format!("{}/{name}", server.origin), false, None)
                .unwrap()
                .0,
            name.as_bytes()
        );
    }
    assert_eq!(
        *requests.lock().unwrap(),
        [(0, true, true), (0, true, true)]
    );
    assert_eq!(server.connection_count(), 1);
    assert_eq!(server.paths(), ["/one", "/two"]);
}
