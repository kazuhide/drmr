//! Deterministic HTTP servers, controlled platform hooks, and local fixture bytes.
#![allow(dead_code)]
pub mod server;
use drmr_core::source::platform::Platform;
use drmr_core::source::{Failure, FetchPolicy};
use std::{
    io,
    net::SocketAddr,
    path::Path,
    sync::{Mutex, MutexGuard},
};
static TIMING: Mutex<()> = Mutex::new(());

pub fn timing() -> MutexGuard<'static, ()> {
    TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
pub fn token<T: std::fmt::Debug>(result: Result<T, Failure>) -> String {
    match result {
        Err(Failure::Source(error)) => error.token().to_string(),
        other => panic!("expected a SourceError, got {other:?}"),
    }
}
pub fn remote_policy(origin: &str) -> FetchPolicy {
    FetchPolicy {
        allowed_origins: vec![origin.to_string()],
        allow_http: true,
        allow_private_network: true,
        ..FetchPolicy::default()
    }
}
pub fn ts_packet(text: &str, fill: u8) -> Vec<u8> {
    let mut data = vec![0x47_u8];
    data.extend_from_slice(text.as_bytes());
    data.resize(188, fill);
    data
}
/// The system with controlled available space for bounded-fetch tests.
pub struct FreeSpace<F: Fn(&Path) -> u64 + Send + Sync>(pub F);
impl<F: Fn(&Path) -> u64 + Send + Sync> Platform for FreeSpace<F> {
    fn disk_free(&self, path: &Path) -> io::Result<u64> {
        Ok((self.0)(path))
    }
}
/// A resolver restricted to fixed test addresses.
pub struct Resolves(pub Vec<SocketAddr>);
impl Platform for Resolves {
    fn resolve(&self, _: &str, _: u16) -> io::Result<Vec<SocketAddr>> {
        Ok(self.0.clone())
    }
}
