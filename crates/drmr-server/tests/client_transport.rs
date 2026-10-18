//! Interrupted response reads must never duplicate an already sent POST.
#[path = "support/client_transport.rs"]
mod client_transport;
use client_transport::{ContinueRead, ContinueReadConnector};
use std::{
    io::{self, Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use ureq::unversioned::{
    resolver::DefaultResolver,
    transport::{
        Buffers, ConnectionDetails, Connector, DefaultConnector, LazyBuffers, NextTimeout,
        Transport,
    },
};

#[derive(Debug)]
struct FaultConnector {
    reads: Arc<AtomicUsize>,
    connections: Arc<AtomicUsize>,
    sent: Arc<Mutex<Vec<u8>>>,
}
impl<T: Transport> Connector<T> for FaultConnector {
    type Out = Fault<T>;
    fn connect(
        &self,
        _: &ConnectionDetails,
        chained: Option<T>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        Ok(chained.map(|inner| {
            self.connections.fetch_add(1, Ordering::SeqCst);
            Fault {
                inner,
                reads: self.reads.clone(),
                sent: self.sent.clone(),
            }
        }))
    }
}
#[derive(Debug)]
struct Fault<T> {
    inner: T,
    reads: Arc<AtomicUsize>,
    sent: Arc<Mutex<Vec<u8>>>,
}
impl<T: Transport> Transport for Fault<T> {
    fn buffers(&mut self) -> &mut dyn Buffers {
        self.inner.buffers()
    }
    fn transmit_output(&mut self, amount: usize, timeout: NextTimeout) -> Result<(), ureq::Error> {
        let mut sent = self.sent.lock().unwrap();
        sent.extend_from_slice(&self.inner.buffers().output()[..amount]);
        assert!(sent.len() <= 4096);
        drop(sent);
        self.inner.transmit_output(amount, timeout)
    }
    fn await_input(&mut self, timeout: NextTimeout) -> Result<bool, ureq::Error> {
        if self.reads.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(io::Error::from(io::ErrorKind::Interrupted).into());
        }
        self.inner.await_input(timeout)
    }
    fn is_open(&mut self) -> bool {
        self.inner.is_open()
    }
}

#[test]
fn an_interrupted_response_read_keeps_one_connection_and_one_post() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "test connection deadline");
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("test accept failed: {:?}", error.kind()),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 1024];
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("test request deadline");
            stream.set_read_timeout(Some(remaining)).unwrap();
            let count = match stream.read(&mut chunk) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => result.unwrap(),
            };
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
            assert!(bytes.len() <= 4096);
            if let Some(header) = bytes.windows(4).position(|part| part == b"\r\n\r\n")
                && bytes.len() >= header + 4 + b"one-post".len()
            {
                break;
            }
        }
        assert!(bytes.starts_with(b"POST /convert HTTP/1.1\r\n"));
        assert_eq!(
            bytes.windows(8).filter(|part| *part == b"one-post").count(),
            1
        );
        stream
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .unwrap();
    });
    let reads = Arc::new(AtomicUsize::new(0));
    let connections = Arc::new(AtomicUsize::new(0));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let connector = DefaultConnector::default()
        .chain(FaultConnector {
            reads: reads.clone(),
            connections: connections.clone(),
            sent: sent.clone(),
        })
        .chain(ContinueReadConnector);
    let agent = ureq::Agent::with_parts(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(2)))
            .build(),
        connector,
        DefaultResolver::default(),
    );
    let result = agent
        .post(format!("http://{address}/convert"))
        .send(b"one-post");
    server.join().unwrap();
    assert_eq!(result.unwrap().status().as_u16(), 202);
    assert!(reads.load(Ordering::SeqCst) >= 2);
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    let sent = sent.lock().unwrap();
    for marker in [b"POST /convert ".as_slice(), b"one-post"] {
        assert_eq!(
            sent.windows(marker.len())
                .filter(|part| *part == marker)
                .count(),
            1
        );
    }
}

#[derive(Debug)]
struct Failure {
    buffers: LazyBuffers,
    reads: usize,
    kind: io::ErrorKind,
}
impl Transport for Failure {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }
    fn transmit_output(&mut self, _: usize, _: NextTimeout) -> Result<(), ureq::Error> {
        unreachable!()
    }
    fn await_input(&mut self, _: NextTimeout) -> Result<bool, ureq::Error> {
        self.reads += 1;
        Err(io::Error::from(self.kind).into())
    }
    fn is_open(&mut self) -> bool {
        true
    }
}
#[test]
fn a_noninterrupted_read_error_is_returned_without_retry() {
    let mut transport = ContinueRead(Failure {
        buffers: LazyBuffers::new(1024, 1024),
        reads: 0,
        kind: io::ErrorKind::ConnectionReset,
    });
    assert!(
        matches!(transport.await_input(NextTimeout{after:Duration::from_secs(1).into(),reason:ureq::Timeout::Global}),Err(ureq::Error::Io(error)) if error.kind()==io::ErrorKind::ConnectionReset)
    );
    assert_eq!(transport.0.reads, 1);
}
#[test]
fn an_exhausted_receive_deadline_never_delegates_the_zero_timeout() {
    let mut transport = ContinueRead(Failure {
        buffers: LazyBuffers::new(1024, 1024),
        reads: 0,
        kind: io::ErrorKind::Interrupted,
    });
    assert!(matches!(
        transport.await_input(NextTimeout {
            after: Duration::ZERO.into(),
            reason: ureq::Timeout::Global
        }),
        Err(ureq::Error::Timeout(ureq::Timeout::Global))
    ));
    assert_eq!(transport.0.reads, 0);
}

#[test]
fn interrupted_reads_spend_the_original_deadline_instead_of_restarting_it() {
    let mut inner = Failure {
        buffers: LazyBuffers::new(1024, 1024),
        reads: 0,
        kind: io::ErrorKind::Interrupted,
    };
    let mut elapsed = [Duration::ZERO, Duration::from_secs(2)].into_iter();
    let result = client_transport::receive_with_elapsed(
        &mut inner,
        NextTimeout {
            after: Duration::from_secs(2).into(),
            reason: ureq::Timeout::Global,
        },
        || elapsed.next().unwrap(),
    );
    assert!(matches!(
        result,
        Err(ureq::Error::Timeout(ureq::Timeout::Global))
    ));
    assert_eq!(inner.reads, 1);
}
