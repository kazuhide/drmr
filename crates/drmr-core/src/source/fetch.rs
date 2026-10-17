//! Policy-bound playlist and media fetching shared by analysis workers.
//!
//! A lock protects byte and disk budgets, address resolution, TLS state, and connection
//! bookkeeping. Each connection belongs to one worker and one validated origin/address
//! identity; sockets are never used by multiple workers at once.

use super::http::{self, HttpError, Response};
use super::platform::{Platform, Stream, TlsContext};
use super::pypath::{as_uri, is_relative_to, resolve};
use super::pyurl::{SplitResult, digits_value, unquote, urlsplit, urlunsplit};
use super::{Escaped, Failure, FetchPolicy, SourceError, child_url, netaddr, origin};
use crate::concurrency::Stop;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

/// One network read (`_CHUNK`).
const CHUNK: usize = 64 * 1024;
/// One local read (`_LOCAL_CHUNK`).
pub(crate) const LOCAL_CHUNK: usize = 1024 * 1024;
/// A body without a declared length is checked against the free-disk floor this often.
const DISK_CHECK_EVERY: u64 = 8 * 1024 * 1024;
/// A response body may take `policy.timeout` per MiB of its declared length.
const TIMEOUT_UNIT: u128 = 1024 * 1024;
/// Redirect hops per resource, the first request included.
const HOPS: usize = 6;

/// `(offset, length)` of an `EXT-X-BYTERANGE` / `EXT-X-MAP` `BYTERANGE`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ByteRange {
    /// First byte.
    pub offset: u64,
    /// Number of bytes.
    pub length: u64,
}

/// What [`Fetcher::fetch`] streamed into its destination.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// The URL the bytes came from (after redirects; a local file's `file:` URI).
    pub effective: String,
    /// SHA-256 of the bytes, lower-case hex.
    pub sha256: String,
    /// Number of bytes.
    pub size: u64,
}

/// A connection's identity in the pool: `(scheme, hostname, port, address)`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ConnKey {
    scheme: String,
    host: String,
    port: u16,
    address: SocketAddr,
}

#[derive(Default)]
struct Shared {
    used: u64,
    playlists: u64,
    /// Bytes claimed against the disk floor by bodies and probe copies being written.
    inflight: u128,
    tls: Option<TlsContext>,
    addresses: HashMap<(String, u16), Vec<SocketAddr>>,
    /// Kept-alive connections per worker thread.
    kept: HashMap<ThreadId, HashMap<ConnKey, Box<dyn Stream>>>,
}

/// A fault inside a fetch, before the caller decides what an `OSError` means there.
pub(crate) enum Fault {
    Source(SourceError),
    Cancelled,
    /// An `OSError` (a disk-usage call, a socket error): a retry in the remote loop,
    /// `source-read-failed` in the local loop.
    Os,
}

impl From<SourceError> for Fault {
    fn from(error: SourceError) -> Self {
        Fault::Source(error)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn token(name: &str) -> SourceError {
    SourceError::new(name)
}

/// Reads of one analysis, bounded by its policy.
pub struct Fetcher {
    policy: FetchPolicy,
    directory: PathBuf,
    platform: Arc<dyn Platform>,
    stop: Stop,
    shared: Mutex<Shared>,
}

impl fmt::Debug for Fetcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fetcher")
            .field("policy", &self.policy)
            .field("stop", &self.stop)
            .finish_non_exhaustive()
    }
}

impl Fetcher {
    /// A fetcher for `policy` whose free-disk checks look at `directory` (the snapshot's).
    pub fn new(policy: &FetchPolicy, directory: &Path, platform: Arc<dyn Platform>) -> Self {
        Fetcher {
            policy: policy.clone(),
            directory: directory.to_path_buf(),
            platform,
            stop: Stop::new(),
            shared: Mutex::new(Shared::default()),
        }
    }

    /// The analysis-wide stop flag: once set, nothing new starts and a download in flight
    /// stops at its next chunk or before its next attempt.
    pub fn stop(&self) -> &Stop {
        &self.stop
    }

    /// The number of kept-alive connections across every worker thread.
    pub fn kept(&self) -> usize {
        lock(&self.shared).kept.values().map(HashMap::len).sum()
    }

    /// Close every kept-alive connection.
    pub fn close_all(&self) {
        let kept = std::mem::take(&mut lock(&self.shared).kept);
        drop(kept);
    }

    // --- budget --------------------------------------------------------------------------

    pub(crate) fn reserve(&self, count: u64) -> Result<(), SourceError> {
        let mut shared = lock(&self.shared);
        if shared.used.saturating_add(count) > self.policy.max_total_bytes {
            return Err(token("byte-budget-exceeded"));
        }
        shared.used += count;
        Ok(())
    }

    pub(crate) fn release(&self, count: u64) {
        let mut shared = lock(&self.shared);
        shared.used -= count;
    }

    /// The free-disk floor must hold for `pending` more bytes on top of everything in flight.
    pub(crate) fn disk(&self, pending: u128) -> Result<(), Fault> {
        let shared = lock(&self.shared);
        self.check_floor(&shared, pending)
    }

    fn check_floor(&self, shared: &Shared, pending: u128) -> Result<(), Fault> {
        let free = self
            .platform
            .disk_free(&self.directory)
            .map_err(|_| Fault::Os)?;
        let needed = u128::from(self.policy.min_free_disk_bytes) + shared.inflight + pending;
        if u128::from(free) < needed {
            return Err(Fault::Source(token("disk-budget-exceeded")));
        }
        Ok(())
    }

    /// Claim `count` bytes about to be written (a declared body, a probe copy) against the
    /// floor, on top of every other claim in flight, so concurrent writers cannot overshoot it
    /// together.
    pub(crate) fn claim_disk(&self, count: u128) -> Result<(), Fault> {
        let mut shared = lock(&self.shared);
        self.check_floor(&shared, count)?;
        shared.inflight += count;
        Ok(())
    }

    pub(crate) fn release_disk(&self, count: u128) {
        lock(&self.shared).inflight -= count;
    }

    /// [`Fetcher::claim_disk`] where Python lets the `OSError` of `shutil.disk_usage` escape
    /// (the probe's temporary copy).
    pub(crate) fn claim_disk_or_escape(&self, count: u128) -> Result<(), Failure> {
        self.claim_disk(count)
            .map_err(|fault| escape_os(fault, "OSError"))
    }

    // --- public reads ----------------------------------------------------------------------

    /// The resource in memory (playlists) with its effective URL.
    pub fn read(
        &self,
        url: &str,
        playlist: bool,
        byte_range: Option<ByteRange>,
    ) -> Result<(Vec<u8>, String), Failure> {
        let cap = if playlist {
            self.policy.max_playlist_bytes
        } else {
            self.policy.max_resource_bytes
        };
        let mut sink = Sink::new(self, None, cap, playlist);
        let effective = self.fetch_into(url, &mut sink, playlist, byte_range)?;
        let size = u128::from(sink.size);
        self.disk(size)
            .map_err(|fault| escape_os(fault, "OSError"))?;
        Ok((std::mem::take(&mut sink.chunks), effective))
    }

    /// The resource streamed into the new file `destination` (created exclusively, mode
    /// 0600). Nothing is left behind when it fails.
    pub fn fetch(
        &self,
        url: &str,
        destination: &Path,
        byte_range: Option<ByteRange>,
    ) -> Result<Fetched, Failure> {
        let mut sink = Sink::new(
            self,
            Some(destination.to_path_buf()),
            self.policy.max_resource_bytes,
            false,
        );
        let result = self
            .fetch_into(url, &mut sink, false, byte_range)
            .and_then(|effective| {
                sink.close();
                self.disk(0).map_err(|fault| escape_os(fault, "OSError"))?;
                Ok(effective)
            });
        match result {
            Ok(effective) => Ok(Fetched {
                effective,
                sha256: sink.digest(),
                size: sink.size,
            }),
            Err(failure) => {
                sink.discard();
                Err(failure)
            }
        }
    }

    // --- internals ---------------------------------------------------------------------------

    fn fetch_into(
        &self,
        url: &str,
        sink: &mut Sink<'_>,
        playlist: bool,
        byte_range: Option<ByteRange>,
    ) -> Result<String, Failure> {
        if playlist {
            let mut shared = lock(&self.shared);
            shared.playlists += 1;
            if shared.playlists > self.policy.max_playlists {
                return Err(Failure::Source(token("playlist-budget-exceeded")));
            }
        }
        let parts = urlsplit(url, "").map_err(|_| Failure::Escaped(Escaped::new("ValueError")))?;
        let effective = if parts.scheme.is_empty() || parts.scheme == "file" {
            self.fetch_local(&parts, sink, byte_range)?
        } else {
            self.fetch_remote(url, sink, byte_range)?
        };
        if let Some(range) = byte_range
            && sink.size != range.length
        {
            return Err(Failure::Source(token("invalid-byte-range")));
        }
        if sink.size == 0 {
            return Err(Failure::Source(token("empty-source-resource")));
        }
        Ok(effective)
    }

    fn fetch_local(
        &self,
        parts: &SplitResult,
        sink: &mut Sink<'_>,
        byte_range: Option<ByteRange>,
    ) -> Result<String, Failure> {
        if parts.scheme == "file"
            && (!parts.netloc.is_empty() || !parts.query.is_empty() || !parts.fragment.is_empty())
        {
            return Err(Failure::Source(token("source-not-allowed")));
        }
        // Outside the `except OSError` of `_fetch`: what `resolve` raises escapes.
        let path = resolve(Path::new(&unquote(&parts.path))).map_err(Failure::Escaped)?;
        let mut allowed = false;
        for root in &self.policy.allowed_local_roots {
            if is_relative_to(&path, &resolve(root).map_err(Failure::Escaped)?) {
                allowed = true;
                break;
            }
        }
        if !allowed {
            return Err(Failure::Source(token("source-not-allowed")));
        }
        match read_local(&path, sink, byte_range) {
            Ok(()) => Ok(as_uri(&path)),
            Err(Fault::Source(error)) => Err(Failure::Source(error)),
            Err(Fault::Cancelled) => Err(Failure::Cancelled),
            Err(Fault::Os) => Err(Failure::Source(token("source-read-failed"))),
        }
    }

    fn tls(&self) -> io::Result<TlsContext> {
        let mut shared = lock(&self.shared);
        if let Some(context) = &shared.tls {
            return Ok(context.clone());
        }
        let context = self.platform.tls_context()?;
        shared.tls = Some(context.clone());
        Ok(context)
    }

    /// Resolve once per host, validate every address, and connect to that exact address while
    /// keeping the origin's hostname for certificate validation. The validated answer is
    /// reused for the rest of the analysis (never re-resolved mid-way).
    fn resolve_host(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, Failure> {
        let key = (host.to_string(), port);
        if let Some(addresses) = lock(&self.shared).addresses.get(&key) {
            return Ok(addresses.clone());
        }
        let addresses = self
            .platform
            .resolve(host, port)
            .map_err(|_| Failure::Source(token("source-resolution-failed")))?;
        if addresses.is_empty() {
            return Err(Failure::Source(token("source-resolution-failed")));
        }
        if !self.policy.allow_private_network
            && addresses
                .iter()
                .any(|address| !netaddr::is_global(address.ip()))
        {
            return Err(Failure::Source(token("source-not-allowed")));
        }
        lock(&self.shared).addresses.insert(key, addresses.clone());
        Ok(addresses)
    }

    fn take(&self, key: &ConnKey) -> Option<Box<dyn Stream>> {
        let mut shared = lock(&self.shared);
        let pool = shared.kept.get_mut(&std::thread::current().id())?;
        pool.remove(key)
    }

    fn keep(&self, key: ConnKey, connection: Box<dyn Stream>) {
        if self.stop.is_set() {
            return; // dropped: closed
        }
        lock(&self.shared)
            .kept
            .entry(std::thread::current().id())
            .or_default()
            .insert(key, connection);
    }

    fn fetch_remote(
        &self,
        url: &str,
        sink: &mut Sink<'_>,
        byte_range: Option<ByteRange>,
    ) -> Result<String, Failure> {
        let mut url = url.to_string();
        for _ in 0..HOPS {
            let origin = origin(&url).map_err(Failure::Source)?;
            let parts =
                urlsplit(&url, "").map_err(|_| Failure::Source(token("source-not-allowed")))?;
            if !(parts.scheme == "https" || parts.scheme == "http")
                || (parts.scheme == "http" && !self.policy.allow_http)
                || !self.policy.allowed_origins.contains(&origin)
            {
                return Err(Failure::Source(token("source-not-allowed")));
            }
            let default_port = if parts.scheme == "https" { 443 } else { 80 };
            let port = match parts.port() {
                Ok(Some(port)) if port != 0 => port,
                _ => default_port,
            };
            let host = parts.hostname().unwrap_or_default();
            let addresses = self.resolve_host(&host, port)?;
            let mut headers = self.policy.headers_for(&origin);
            set_header(&mut headers, "Accept-Encoding", "identity");
            if let Some(range) = byte_range {
                let last = u128::from(range.offset) + u128::from(range.length) - 1;
                set_header(
                    &mut headers,
                    "Range",
                    &format!("bytes={}-{last}", range.offset),
                );
            }
            let request = Request {
                url: &url,
                parts: &parts,
                host: &host,
                port,
                addresses: &addresses,
                headers: &headers,
                byte_range,
            };
            match self.attempts(&request, sink)? {
                Hop::Done => return Ok(url),
                Hop::Redirect(location) => url = location,
            }
        }
        Err(Failure::Source(token("redirect-budget-exceeded")))
    }

    /// The attempts at one hop: a 5xx response is retried while attempts remain, a network
    /// fault is an attempt, a kept-alive connection the server had dropped before any response
    /// is retried for free.
    fn attempts(&self, request: &Request<'_>, sink: &mut Sink<'_>) -> Result<Hop, Failure> {
        let attempts = self.policy.attempts as usize;
        let mut attempt = 0;
        while attempt < attempts {
            if self.stop.is_set() {
                return Err(Failure::Cancelled);
            }
            let address = unscoped(request.addresses[attempt % request.addresses.len()]);
            let key = ConnKey {
                scheme: request.parts.scheme.clone(),
                host: request.host.to_string(),
                port: request.port,
                address,
            };
            let mut connection = self.take(&key);
            let reused = connection.is_some();
            let mut responded = false;
            let outcome = self.exchange(
                request,
                sink,
                &key,
                attempt,
                &mut connection,
                &mut responded,
            );
            sink.unclaim();
            if let Ok(Step::Done { keep: true }) = outcome
                && let Some(connection) = connection.take()
            {
                self.keep(key, connection);
            }
            drop(connection); // closed unless kept
            match outcome {
                Ok(Step::Done { .. }) => return Ok(Hop::Done),
                Ok(Step::Redirect(location)) => return Ok(Hop::Redirect(location)),
                Ok(Step::ServerError) => attempt += 1,
                Err(Attempt::Fatal(failure)) => return Err(failure),
                Err(Attempt::Retry(dropped)) => {
                    sink.reset();
                    if reused && !responded && dropped {
                        continue; // the server had dropped the idle connection: not an attempt
                    }
                    attempt += 1;
                    if attempt == attempts {
                        return Err(Failure::Source(token("source-fetch-failed")));
                    }
                }
            }
        }
        Err(Failure::Source(token("source-fetch-failed")))
    }

    /// Open a connection to the attempt's address: TCP, then TLS for `https` (the handshake
    /// bounded by the timeout), every operation bounded by `timeout`.
    fn connect(&self, key: &ConnKey, host: &str) -> Result<Box<dyn Stream>, Attempt> {
        let timeout = seconds(self.policy.timeout);
        let stream = self
            .platform
            .connect(key.address, timeout)
            .map_err(retry_io)?;
        stream.set_read_timeout(Some(timeout)).map_err(retry_io)?;
        stream.set_write_timeout(Some(timeout)).map_err(retry_io)?;
        if key.scheme != "https" {
            return Ok(stream);
        }
        let context = self.tls().map_err(retry_io)?;
        self.platform
            .wrap_tls(&context, stream, host, timeout)
            .map_err(retry_io)
    }

    /// One attempt: connect (unless a kept connection is reused), send, read the response.
    fn exchange(
        &self,
        request: &Request<'_>,
        sink: &mut Sink<'_>,
        key: &ConnKey,
        attempt: usize,
        connection: &mut Option<Box<dyn Stream>>,
        responded: &mut bool,
    ) -> Result<Step, Attempt> {
        if connection.is_none() {
            *connection = Some(self.connect(key, request.host)?);
        }
        let stream = connection.as_mut().expect("connected above");
        // Every request gets the whole timeout for its headers: a kept-alive socket still
        // carries whatever remained of the previous resource's deadline.
        let timeout = seconds(self.policy.timeout);
        stream.set_read_timeout(Some(timeout)).map_err(retry_io)?;
        stream.set_write_timeout(Some(timeout)).map_err(retry_io)?;
        let path = if request.parts.path.is_empty() {
            "/"
        } else {
            &request.parts.path
        };
        let target = urlunsplit("", "", path, &request.parts.query, "");
        let host_header = http::host_header(request.host, request.port);
        http::send_request(stream.as_mut(), &host_header, &target, request.headers)
            .map_err(retry_http)?;
        let mut response = Response::begin(stream.as_mut()).map_err(retry_http)?;
        *responded = true;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            let location = response.header("Location", None).unwrap_or_default();
            if location.is_empty() {
                return Err(fatal("invalid-source-redirect"));
            }
            return match child_url(request.url, &location) {
                Ok(next) => Ok(Step::Redirect(next)),
                Err(Failure::Source(error)) => Err(Attempt::Fatal(Failure::Source(error))),
                // A `ValueError` from the URL parser takes the network-fault path, as Python's
                // `except (OSError, HTTPException, ValueError)` around the exchange does.
                Err(_) => Err(Attempt::Retry(false)),
            };
        }
        if response.status >= 500 && attempt + 1 < self.policy.attempts as usize {
            return Ok(Step::ServerError);
        }
        let expected = if request.byte_range.is_some() {
            206
        } else {
            200
        };
        if response.status != expected {
            return Err(fatal("source-http-failed"));
        }
        let encoding = response
            .header("Content-Encoding", Some("identity"))
            .unwrap_or_default();
        if encoding.to_lowercase() != "identity" {
            return Err(fatal("unsupported-content-encoding"));
        }
        let expected_length = response.declared_length();
        if let Some(range) = request.byte_range {
            check_content_range(&response, range)?;
        }
        if sink.path.is_some()
            && let Some(length) = expected_length
            && length > 0
        {
            // The floor holds before the body lands, not only after.
            sink.claim(length).map_err(fault_to_attempt)?;
        }
        self.read_body(&mut response, sink, expected_length)?;
        Ok(Step::Done {
            keep: !response.will_close,
        })
    }

    /// Bounded reads: each chunk is hashed, charged and written as it arrives, within one
    /// `timeout` per MiB of the declared length (a body without one earns its allowance as it
    /// lands), and no single chunk may stall longer than `timeout`.
    fn read_body(
        &self,
        response: &mut Response<'_>,
        sink: &mut Sink<'_>,
        expected_length: Option<u128>,
    ) -> Result<(), Attempt> {
        let started = Instant::now();
        while !response.is_closed() {
            let allowed = allowance(expected_length.unwrap_or(u128::from(sink.size) + 1));
            let deadline = self.policy.timeout * allowed;
            let remaining = deadline - started.elapsed().as_secs_f64();
            if remaining <= 0.0 {
                return Err(Attempt::Fatal(Failure::Source(token("source-timeout"))));
            }
            response
                .set_read_timeout(seconds(remaining.min(self.policy.timeout)))
                .map_err(retry_io)?;
            let chunk = response.read1(CHUNK).map_err(retry_http)?;
            if chunk.is_empty() {
                break;
            }
            sink.write(&chunk).map_err(fault_to_attempt)?;
        }
        if let Some(length) = expected_length
            && u128::from(sink.size) != length
        {
            // read1 permits EOF without IncompleteRead; a partial object takes the bounded
            // retry path instead.
            return Err(Attempt::Retry(false));
        }
        Ok(())
    }
}

/// `sock.settimeout(value)`: a positive number of seconds as a socket timeout (at least a
/// microsecond, as a zero timeout is not one; at most what a `Duration` holds).
fn seconds(value: f64) -> Duration {
    Duration::try_from_secs_f64(value)
        .unwrap_or(Duration::MAX)
        .max(Duration::from_micros(1))
}

fn fatal(name: &str) -> Attempt {
    Attempt::Fatal(Failure::Source(token(name)))
}

/// How one hop of a remote fetch ended.
enum Hop {
    Done,
    Redirect(String),
}

/// How one attempt ended without a fault.
enum Step {
    /// The body was read in full; `keep`: the connection may serve the worker's next resource.
    Done {
        keep: bool,
    },
    Redirect(String),
    /// A 5xx response while attempts remain: the next attempt.
    ServerError,
}

/// How one attempt failed.
enum Attempt {
    /// Raised as is (a `SourceError`, `Cancelled`, an escaping exception).
    Fatal(Failure),
    /// A network fault (`OSError`, `HTTPException`, `ValueError`): the next attempt; `true`
    /// when it is how a dropped kept-alive connection shows.
    Retry(bool),
}

fn retry_io(error: io::Error) -> Attempt {
    retry_http(HttpError::Io(error))
}

fn retry_http(error: HttpError) -> Attempt {
    Attempt::from(error)
}

impl From<HttpError> for Attempt {
    fn from(error: HttpError) -> Self {
        Attempt::Retry(error.is_dropped())
    }
}

fn fault_to_attempt(fault: Fault) -> Attempt {
    match fault {
        Fault::Source(error) => Attempt::Fatal(Failure::Source(error)),
        Fault::Cancelled => Attempt::Fatal(Failure::Cancelled),
        Fault::Os => Attempt::Retry(false),
    }
}

/// An `OSError` from a disk-usage call at the corresponding call site not catch it: it escapes.
fn escape_os(fault: Fault, class: &'static str) -> Failure {
    match fault {
        Fault::Source(error) => Failure::Source(error),
        Fault::Cancelled => Failure::Cancelled,
        Fault::Os => Failure::Escaped(Escaped::new(class)),
    }
}

struct Request<'a> {
    url: &'a str,
    parts: &'a SplitResult,
    host: &'a str,
    port: u16,
    addresses: &'a [SocketAddr],
    headers: &'a [(String, String)],
    byte_range: Option<ByteRange>,
}

/// `headers[name] = value` on a dict: the exact key keeps its place, a new key goes last.
fn set_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    match headers.iter_mut().find(|(key, _)| key == name) {
        Some(entry) => entry.1 = value.to_string(),
        None => headers.push((name.to_string(), value.to_string())),
    }
}

/// `address[4][:2]`: the address without flow information or scope.
fn unscoped(address: SocketAddr) -> SocketAddr {
    match address {
        SocketAddr::V4(v4) => SocketAddr::V4(v4),
        SocketAddr::V6(v6) => {
            SocketAddr::V6(std::net::SocketAddrV6::new(*v6.ip(), v6.port(), 0, 0))
        }
    }
}

/// `bytes (\d+)-(\d+)/(\d+)` naming exactly the requested range of a resource at least that
/// long.
fn check_content_range(response: &Response<'_>, range: ByteRange) -> Result<(), Attempt> {
    let value = response
        .header("Content-Range", Some(""))
        .unwrap_or_default();
    let invalid = || Attempt::Fatal(Failure::Source(token("invalid-byte-range")));
    let rest = value.strip_prefix("bytes ").ok_or_else(invalid)?;
    let (first, rest) = rest.split_once('-').ok_or_else(invalid)?;
    let (last, total) = rest.split_once('/').ok_or_else(invalid)?;
    let first = digits_value(first).ok_or_else(invalid)?;
    let last = digits_value(last).ok_or_else(invalid)?;
    let total = digits_value(total).ok_or_else(invalid)?;
    let offset = u128::from(range.offset);
    let end = offset + u128::from(range.length);
    if first != offset || last != end - 1 || total < end {
        return Err(invalid());
    }
    Ok(())
}

/// How many `policy.timeout`s a body of `length` bytes may take: one per MiB, one at least.
fn allowance(length: u128) -> f64 {
    let units = length.div_ceil(TIMEOUT_UNIT).max(1);
    #[allow(clippy::cast_precision_loss)] // a count of MiB; exact far beyond any real body
    let units = units as f64;
    units
}

fn read_local(
    path: &Path,
    sink: &mut Sink<'_>,
    byte_range: Option<ByteRange>,
) -> Result<(), Fault> {
    let mut handle = File::open(path).map_err(|_| Fault::Os)?;
    // `path.open("rb")`: `io.FileIO` refuses a directory (`IsADirectoryError`, an `OSError`)
    // before any range is compared with its size.
    if handle.metadata().map_err(|_| Fault::Os)?.is_dir() {
        return Err(Fault::Os);
    }
    let mut remaining: Option<u64> = None;
    if let Some(range) = byte_range {
        let size = handle.metadata().map_err(|_| Fault::Os)?.len();
        if u128::from(range.offset) + u128::from(range.length) > u128::from(size) {
            return Err(Fault::Source(token("invalid-byte-range")));
        }
        handle
            .seek(SeekFrom::Start(range.offset))
            .map_err(|_| Fault::Os)?;
        remaining = Some(range.length);
    }
    loop {
        let want = match remaining {
            Some(0) => break,
            Some(left) => usize::try_from(left).map_or(LOCAL_CHUNK, |left| left.min(LOCAL_CHUNK)),
            None => LOCAL_CHUNK,
        };
        let chunk = read_full(&mut handle, want).map_err(|_| Fault::Os)?;
        if chunk.is_empty() {
            break;
        }
        sink.write(&chunk)?;
        if let Some(left) = remaining.as_mut() {
            *left -= chunk.len() as u64;
        }
    }
    Ok(())
}

/// A buffered file's `read(n)`: `n` bytes, fewer only at the end of the file.
pub(crate) fn read_full(reader: &mut impl Read, n: usize) -> io::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(n);
    reader.take(n as u64).read_to_end(&mut out)?;
    Ok(out)
}

/// `os.open(path, O_WRONLY | O_CREAT | O_EXCL, 0o600)`.
pub(crate) fn open_exclusive(path: &Path) -> Result<File, SourceError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| token("snapshot-write-failed"))
}

/// Receives one resource chunk by chunk: hashes it, charges the analysis-wide byte budget per
/// chunk, and keeps it in memory (playlists) or streams it to a new exclusive file.
pub(crate) struct Sink<'f> {
    fetcher: &'f Fetcher,
    path: Option<PathBuf>,
    cap: u64,
    playlist: bool,
    size: u64,
    reserved: u64,
    hash: Sha256,
    chunks: Vec<u8>,
    handle: Option<File>,
    since_disk_check: u64,
    declared: bool,
    claim: u128,
}

impl<'f> Sink<'f> {
    fn new(fetcher: &'f Fetcher, path: Option<PathBuf>, cap: u64, playlist: bool) -> Self {
        Sink {
            fetcher,
            path,
            cap,
            playlist,
            size: 0,
            reserved: 0,
            hash: Sha256::new(),
            chunks: Vec::new(),
            handle: None,
            since_disk_check: 0,
            declared: false,
            claim: 0,
        }
    }

    fn clear(&mut self) {
        self.size = 0;
        self.reserved = 0;
        self.hash = Sha256::new();
        self.chunks = Vec::new();
        self.handle = None;
        self.since_disk_check = 0;
        self.declared = false;
        self.claim = 0;
    }

    /// A declared body: claim its length against the disk floor before its first byte; the
    /// claim shrinks as the bytes land (they then count in the disk's own free space), so
    /// nothing is counted twice, and whatever is left of it is released by `unclaim`.
    fn claim(&mut self, length: u128) -> Result<(), Fault> {
        self.fetcher.claim_disk(length)?;
        self.declared = true;
        self.claim = length;
        Ok(())
    }

    fn unclaim(&mut self) {
        if self.claim > 0 {
            self.fetcher.release_disk(self.claim);
            self.claim = 0;
        }
    }

    fn write(&mut self, chunk: &[u8]) -> Result<(), Fault> {
        if self.fetcher.stop.is_set() {
            return Err(Fault::Cancelled);
        }
        let length = chunk.len() as u64;
        if self.size.saturating_add(length) > self.cap {
            return Err(Fault::Source(token(if self.playlist {
                "playlist-too-large"
            } else {
                "byte-budget-exceeded"
            })));
        }
        self.fetcher.reserve(length)?;
        self.reserved += length;
        self.size += length;
        self.hash.update(chunk);
        let Some(path) = &self.path else {
            self.chunks.extend_from_slice(chunk);
            return Ok(());
        };
        if self.handle.is_none() {
            self.handle = Some(open_exclusive(path)?);
        }
        if self.declared {
            let landed = u128::from(length).min(self.claim);
            if landed > 0 {
                self.fetcher.release_disk(landed);
                self.claim -= landed;
            }
        } else {
            self.since_disk_check += length;
            if self.since_disk_check >= DISK_CHECK_EVERY {
                self.since_disk_check = 0;
                self.fetcher.disk(0)?; // bodies without a declared length: the floor, every few MiB
            }
        }
        self.handle
            .as_mut()
            .expect("opened above")
            .write_all(chunk)
            .map_err(|_| Fault::Source(token("snapshot-write-failed")))
    }

    /// Forget a failed attempt (its bytes and the rest of its disk claim are refunded).
    fn reset(&mut self) {
        self.fetcher.release(self.reserved);
        self.unclaim();
        self.discard();
        self.clear();
    }

    fn discard(&mut self) {
        self.handle = None;
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }

    fn close(&mut self) {
        self.handle = None;
    }

    fn digest(&self) -> String {
        hex::encode(self.hash.clone().finalize())
    }
}
