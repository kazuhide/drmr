//! Shared subprocess reservations. A permit owns its tokens until every pipe reader joins.

use super::{Hardware, Workload, capacity};
use crate::concurrency::Stop;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

/// Estimated resources held while a media subprocess is running and its output is consumed.
#[derive(Clone, Copy, Debug)]
pub struct Claim {
    /// Participating tools use an explicit single-threaded codec configuration.
    pub cpu_threads: usize,
    /// Input, codec, and configured capture working-set estimate in bytes.
    pub memory_bytes: u64,
}

impl Claim {
    /// Include the actual capture caps, which can differ between metadata and packet probes.
    pub fn for_workload(workload: Workload, stdout_bytes: usize, stderr_bytes: usize) -> Self {
        Self {
            cpu_threads: 1,
            memory_bytes: workload.estimated_child_bytes(stdout_bytes, stderr_bytes),
        }
    }
}

/// A failed queued reservation. No process has been spawned when this is returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReservationError {
    /// The claim exceeds the entire capacity, so waiting cannot help.
    #[error("resource-budget")]
    Insufficient,
    /// Cooperative cancellation while waiting for an existing child to finish.
    #[error("cancelled")]
    Cancelled,
    /// The call's time allowance elapsed before resources became available.
    #[error("timeout")]
    Timeout,
}

#[derive(Debug, Default)]
struct Used {
    cpu_threads: usize,
    memory_bytes: u64,
}

/// One admission pool shared by tool runners and independently scheduled media pools.
#[derive(Debug)]
pub struct Limiter {
    cpu_threads: usize,
    memory_bytes: u64,
    used: Mutex<Used>,
    available: Condvar,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Limiter {
    /// Configured admission ceiling, without observing other commands' transient usage.
    pub(crate) fn capacity(&self) -> (usize, u64) {
        (self.cpu_threads, self.memory_bytes)
    }

    /// A fixed capacity, useful for an enclosing application's own resource allocation.
    pub fn new(cpu_threads: usize, memory_bytes: u64) -> Self {
        Self {
            cpu_threads,
            memory_bytes,
            used: Mutex::new(Used::default()),
            available: Condvar::new(),
        }
    }

    /// Wait cooperatively for both token types. The caller acquires once per child, outside
    /// helpers that themselves run tools, so a child never holds a permit while asking for another.
    pub fn acquire(
        self: &Arc<Self>,
        claim: Claim,
        stop: Option<&Stop>,
        timeout: Option<Duration>,
    ) -> Result<Permit, ReservationError> {
        if claim.cpu_threads == 0
            || claim.cpu_threads > self.cpu_threads
            || claim.memory_bytes > self.memory_bytes
        {
            return Err(ReservationError::Insufficient);
        }
        let started = Instant::now();
        let mut used = lock(&self.used);
        loop {
            if stop.is_some_and(Stop::is_set) {
                return Err(ReservationError::Cancelled);
            }
            if timeout.is_some_and(|limit| started.elapsed() >= limit) {
                return Err(ReservationError::Timeout);
            }
            if claim.cpu_threads <= self.cpu_threads - used.cpu_threads
                && claim.memory_bytes <= self.memory_bytes - used.memory_bytes
            {
                used.cpu_threads += claim.cpu_threads;
                used.memory_bytes += claim.memory_bytes;
                return Ok(Permit {
                    limiter: Arc::clone(self),
                    claim,
                });
            }
            let interval = timeout.map_or(Duration::from_millis(20), |limit| {
                limit
                    .saturating_sub(started.elapsed())
                    .min(Duration::from_millis(20))
            });
            used = self
                .available
                .wait_timeout(used, interval)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// A live child reservation. Dropping it returns both token types, including during unwinding.
#[derive(Debug)]
pub struct Permit {
    limiter: Arc<Limiter>,
    claim: Claim,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut used = lock(&self.limiter.used);
        used.cpu_threads -= self.claim.cpu_threads;
        used.memory_bytes -= self.claim.memory_bytes;
        self.limiter.available.notify_all();
    }
}

/// All default runners in this process share one point-in-time capacity snapshot.
/// It reserves host/control-group headroom; this is admission control, not an RSS sandbox.
pub fn process_limiter() -> Arc<Limiter> {
    static LIMITER: OnceLock<Arc<Limiter>> = OnceLock::new();
    Arc::clone(LIMITER.get_or_init(|| {
        let (cpu, memory) = capacity(Hardware::detect(), 1);
        Arc::new(Limiter::new(cpu, memory))
    }))
}
