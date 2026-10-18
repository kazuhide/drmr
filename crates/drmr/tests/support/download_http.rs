//! A bounded, owned localhost server for real download command fixtures.
use std::{
    collections::BTreeMap,
    io::{self, Read as _, Write as _},
    net::{TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const QUERY: &str = "download-query-sentinel";
pub const AUTH: &str = "Bearer download-auth-sentinel";

#[derive(Debug, Default)]
pub struct Counts {
    pub served: usize,
    pub refused: usize,
    pub routes: Vec<usize>,
}

pub struct Server {
    pub origin: String,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<io::Result<Counts>>>,
}

impl Server {
    pub fn start(files: BTreeMap<String, Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            let mut counts = Counts::default();
            while !stopping.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if let Err(error) = serve(stream, &files, &mut counts) {
                            eprintln!(
                                "download fixture: io-kind={:?} served={} refused={}",
                                error.kind(),
                                counts.served,
                                counts.refused
                            );
                            return Err(error);
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(counts)
        });
        Self {
            origin,
            stop,
            worker: Some(worker),
        }
    }

    pub fn stop(mut self) -> Counts {
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap().unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            match worker.join() {
                Ok(Ok(counts)) if thread::panicking() => eprintln!(
                    "download fixture: served={} refused={} routes={:?}",
                    counts.served, counts.refused, counts.routes
                ),
                Ok(Ok(_)) => {}
                Ok(Err(error)) => eprintln!("download fixture: joined io-kind={:?}", error.kind()),
                Err(_) => eprintln!("download fixture: worker panicked"),
            }
        }
    }
}

fn serve(
    mut stream: TcpStream,
    files: &BTreeMap<String, Vec<u8>>,
    counts: &mut Counts,
) -> io::Result<()> {
    prepare(&stream)?;
    let mut request = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(2);
    while !request.ends_with(b"\r\n\r\n") {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "fixture request deadline",
            ));
        }
        stream.set_read_timeout(Some(remaining))?;
        let mut byte = [0];
        if stream.read(&mut byte)? == 0 || request.len() == 16 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "incomplete fixture request",
            ));
        }
        request.push(byte[0]);
    }
    let request = std::str::from_utf8(&request)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "fixture request encoding"))?;
    let mut lines = request.split("\r\n");
    let first = lines.next().unwrap();
    let target = first
        .strip_prefix("GET ")
        .and_then(|s| s.strip_suffix(" HTTP/1.1"));
    let authorized = lines.any(|line| {
        line.split_once(':').is_some_and(|(name, value)| {
            name.eq_ignore_ascii_case("authorization") && value.trim() == AUTH
        })
    });
    let path = target.and_then(|target| target.strip_suffix(&format!("?token={QUERY}")));
    let body = path.and_then(|path| files.get(path));
    if let Some(body) = body.filter(|_| authorized) {
        counts.served += 1;
        if counts.routes.len() < 32 {
            counts.routes.push(
                files
                    .keys()
                    .position(|name| Some(name.as_str()) == path)
                    .unwrap(),
            );
        }
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        stream.write_all(body)?;
    } else {
        counts.refused += 1;
        eprintln!(
            "download fixture: refused authorized={authorized} valid-query={} known-target={}",
            path.is_some(),
            body.is_some()
        );
        stream.write_all(
            b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )?;
    }
    Ok(())
}

fn prepare(stream: &TcpStream) -> io::Result<()> {
    // Some systems inherit listener mode; timed reads require a blocking socket.
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))
}
#[cfg(unix)]
#[test]
fn accepted_stream_mode_is_explicit_even_when_the_listener_is_nonblocking() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (stream, _) = listener.accept().unwrap();
    let inherited = rustix::fs::fcntl_getfl(&stream)
        .unwrap()
        .contains(rustix::fs::OFlags::NONBLOCK);
    eprintln!("download fixture: actual accepted nonblocking={inherited}");
    stream.set_nonblocking(true).unwrap();
    prepare(&stream).unwrap();
    assert!(
        !rustix::fs::fcntl_getfl(&stream)
            .unwrap()
            .contains(rustix::fs::OFlags::NONBLOCK)
    );
    drop(peer);
}
