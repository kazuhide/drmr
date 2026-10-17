//! Operating-system interface for DNS, sockets, TLS, and free-space checks.
//!
//! Tests inject implementations to exercise network and filesystem failures.

use rustls::pki_types::ServerName;
use std::cell::Cell;
use std::fmt;
use std::io::{self, BufRead, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A connected byte stream with per-operation timeouts (a socket with `settimeout`).
pub trait Stream: Read + Write + Send {
    /// Bound every following read (`None`: no bound).
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
    /// Bound every following write (`None`: no bound).
    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()>;
}

impl Stream for TcpStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        unless_reset(TcpStream::set_read_timeout(self, timeout))
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        unless_reset(TcpStream::set_write_timeout(self, timeout))
    }
}

/// Python's `settimeout` cannot fail on an open socket (`CPython` polls with the timeout and
/// never sets it on the socket), but BSD kernels refuse `setsockopt` with `EINVAL` once the
/// peer has reset the connection. That refusal is passed over: the read or write that follows
/// reports the reset itself (a dropped connection, when it is a kept-alive one).
fn unless_reset(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error)
            if error.kind() == io::ErrorKind::InvalidInput
                && error.raw_os_error() == Some(rustix::io::Errno::INVAL.raw_os_error()) =>
        {
            Ok(())
        }
        other => other,
    }
}

/// A verifying TLS client configuration, created once per analysis (`ssl.create_default_context`:
/// certificate chain and hostname verified against the trust roots).
#[derive(Clone)]
pub struct TlsContext(pub Arc<rustls::ClientConfig>);

impl fmt::Debug for TlsContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TlsContext")
    }
}

impl TlsContext {
    /// Mozilla's root store, the ring provider, TLS 1.2 and 1.3.
    pub fn with_webpki_roots() -> io::Result<Self> {
        let roots = rustls::RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(TlsContext(Arc::new(config)))
    }
}

/// The operating system as the fetcher uses it.
pub trait Platform: Send + Sync {
    /// `socket.getaddrinfo(host, port, type=SOCK_STREAM)`: every address, in the resolver's order.
    fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        Ok((host, port).to_socket_addrs()?.collect())
    }

    /// `socket.create_connection(address, timeout)`.
    fn connect(&self, address: SocketAddr, timeout: Duration) -> io::Result<Box<dyn Stream>> {
        let stream = TcpStream::connect_timeout(&address, timeout)?;
        Ok(Box::new(stream))
    }

    /// `ssl.create_default_context`; the fetcher asks once per analysis, and only when it
    /// meets an `https` resource.
    fn tls_context(&self) -> io::Result<TlsContext> {
        TlsContext::with_webpki_roots()
    }

    /// `context.wrap_socket(stream, server_hostname=host)` on a socket whose timeout is
    /// `timeout`: the handshake completes here, within one deadline of `timeout` however many
    /// reads and writes it takes (as `_ssl`'s `do_handshake`).
    fn wrap_tls(
        &self,
        context: &TlsContext,
        stream: Box<dyn Stream>,
        host: &str,
        timeout: Duration,
    ) -> io::Result<Box<dyn Stream>> {
        let name = ServerName::try_from(host.to_string())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let connection = rustls::ClientConnection::new(Arc::clone(&context.0), name)
            .map_err(io::Error::other)?;
        let mut tls = TlsStream {
            tls: rustls::StreamOwned::new(
                connection,
                Deadlined {
                    sock: stream,
                    deadline: None,
                },
            ),
            read_timeout: Cell::new(Some(timeout)),
            write_timeout: Cell::new(Some(timeout)),
        };
        tls.tls.sock.deadline = deadline(Some(timeout));
        // `complete_io` drives the handshake to its end (an early EOF is an error).
        tls.tls.conn.complete_io(&mut tls.tls.sock)?;
        if tls.tls.conn.is_handshaking() {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        Ok(Box::new(tls))
    }

    /// `shutil.disk_usage(path).free`: the bytes an unprivileged process may still write
    /// (`f_bavail * f_frsize`).
    fn disk_free(&self, path: &Path) -> io::Result<u64> {
        disk_free(path)
    }
}

/// The operating system itself.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemPlatform;

impl Platform for SystemPlatform {}

/// When a TLS operation started now with `timeout` must end (`None`: never).
fn deadline(timeout: Option<Duration>) -> Option<Instant> {
    timeout.and_then(|timeout| Instant::now().checked_add(timeout))
}

/// The socket under a TLS connection. `CPython`'s `_ssl` turns the socket timeout into a
/// deadline for a whole `SSL_read`, `SSL_write` or handshake, however many socket reads and
/// writes the record layer needs on the way; each one here gets what is left of the deadline
/// of the TLS operation in progress, and none starts once it has passed.
struct Deadlined {
    sock: Box<dyn Stream>,
    deadline: Option<Instant>,
}

impl Deadlined {
    fn left(&self) -> io::Result<Option<Duration>> {
        let Some(deadline) = self.deadline else {
            return Ok(None);
        };
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the TLS operation timed out",
            ));
        }
        Ok(Some(left))
    }
}

impl Read for Deadlined {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.left()?;
        self.sock.set_read_timeout(left)?;
        self.sock.read(buf)
    }
}

impl Write for Deadlined {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let left = self.left()?;
        self.sock.set_write_timeout(left)?;
        self.sock.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.sock.flush()
    }
}

/// A TLS connection read and written as `ssl.SSLSocket` is: the timeouts set on it bound
/// each TLS operation as a whole (see [`Deadlined`]).
struct TlsStream {
    tls: rustls::StreamOwned<rustls::ClientConnection, Deadlined>,
    read_timeout: Cell<Option<Duration>>,
    write_timeout: Cell<Option<Duration>>,
}

impl Read for TlsStream {
    /// `SSL_read`: the plaintext of one TLS record at most (what is left of it), within one
    /// deadline of the read timeout. A later record stays for the next read even when it has
    /// arrived, as OpenSSL leaves it.
    ///
    /// A peer that closes without `close_notify` ends the stream (Python's `ssl` default,
    /// `suppress_ragged_eofs=True`): a close-delimited body ends there, and a kept-alive
    /// connection the server dropped shows as `RemoteDisconnected`, the free retry.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        self.tls.sock.deadline = deadline(self.read_timeout.get());
        let record = match self.tls.fill_buf() {
            Ok(record) => record,
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(0),
            Err(error) => return Err(error),
        };
        let count = record.len().min(buf.len());
        buf[..count].copy_from_slice(&record[..count]);
        self.tls.consume(count);
        Ok(count)
    }
}

impl Write for TlsStream {
    /// `SSL_write`, within one deadline of the write timeout.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.tls.sock.deadline = deadline(self.write_timeout.get());
        self.tls.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.tls.sock.deadline = deadline(self.write_timeout.get());
        self.tls.flush()
    }
}

impl Stream for TlsStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.read_timeout.set(timeout);
        Ok(())
    }

    fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.write_timeout.set(timeout);
        Ok(())
    }
}

/// `shutil.disk_usage(path).free` on a POSIX system: `statvfs`'s `f_bavail * f_frsize`.
#[cfg(unix)]
pub fn disk_free(path: &Path) -> io::Result<u64> {
    let stats = rustix::fs::statvfs(path)?;
    Ok(stats.f_bavail.saturating_mul(stats.f_frsize))
}

/// Where the platform has no `statvfs`, nothing is known about the free space: the floor
/// check cannot pass a non-zero floor.
#[cfg(not(unix))]
pub fn disk_free(_path: &Path) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "disk usage is not available on this platform",
    ))
}
