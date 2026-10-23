//! Acquisition-only short socket waits retain each caller's full operation deadline.
use crate::{
    concurrency::Stop,
    source::platform::{Platform, Stream, SystemPlatform},
};
use std::{
    cell::Cell,
    io::{self, Read, Write},
    net::SocketAddr,
    time::{Duration, Instant},
};
const SLICE: Duration = Duration::from_millis(50);
pub(in crate::conversion) struct CancellablePlatform {
    pub stop: Stop,
    pub abort: Stop,
}
impl Platform for CancellablePlatform {
    fn connect(&self, address: SocketAddr, timeout: Duration) -> io::Result<Box<dyn Stream>> {
        check(&self.stop, &self.abort)?;
        let stream = SystemPlatform.connect(address, timeout)?;
        check(&self.stop, &self.abort)?;
        Ok(Box::new(CancellableStream {
            inner: stream,
            stop: self.stop.clone(),
            abort: self.abort.clone(),
            read: Cell::new(Some(timeout)),
            write: Cell::new(Some(timeout)),
        }))
    }
}
struct CancellableStream {
    inner: Box<dyn Stream>,
    stop: Stop,
    abort: Stop,
    read: Cell<Option<Duration>>,
    write: Cell<Option<Duration>>,
}
fn check(stop: &Stop, abort: &Stop) -> io::Result<()> {
    if stop.is_set() || abort.is_set() {
        Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "acquisition cancelled",
        ))
    } else {
        Ok(())
    }
}
fn remaining(start: Instant, timeout: Option<Duration>) -> io::Result<Duration> {
    match timeout {
        Some(limit) => limit
            .checked_sub(start.elapsed())
            .filter(|time| !time.is_zero())
            .map(|time| time.min(SLICE))
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut)),
        None => Ok(SLICE),
    }
}
fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}
impl Read for CancellableStream {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let start = Instant::now();
        loop {
            check(&self.stop, &self.abort)?;
            self.inner
                .set_read_timeout(Some(remaining(start, self.read.get())?))?;
            match self.inner.read(bytes) {
                Err(error) if retry(&error) => {}
                result => return result,
            }
        }
    }
}
impl Write for CancellableStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let start = Instant::now();
        loop {
            check(&self.stop, &self.abort)?;
            self.inner
                .set_write_timeout(Some(remaining(start, self.write.get())?))?;
            match self.inner.write(bytes) {
                Err(error) if retry(&error) => {}
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        check(&self.stop, &self.abort)?;
        self.inner.flush()
    }
}
impl Stream for CancellableStream {
    fn set_read_timeout(&self, value: Option<Duration>) -> io::Result<()> {
        self.read.set(value);
        Ok(())
    }
    fn set_write_timeout(&self, value: Option<Duration>) -> io::Result<()> {
        self.write.set(value);
        Ok(())
    }
}
