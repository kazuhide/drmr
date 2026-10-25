//! A command's nested pools share one CPU ceiling independently of other commands.
use super::ReservationError;
use crate::concurrency::Stop;
use std::{
    sync::{Arc, Condvar, Mutex, PoisonError},
    time::{Duration, Instant},
};

#[derive(Debug)]
pub(crate) struct CpuScope {
    limit: usize,
    used: Mutex<usize>,
    available: Condvar,
    parent: Option<Arc<Self>>,
}

impl CpuScope {
    pub fn new(limit: usize, parent: Option<Arc<Self>>) -> Arc<Self> {
        let limit = parent
            .as_ref()
            .map_or(limit, |parent| limit.min(parent.limit));
        Arc::new(Self {
            limit,
            used: Mutex::new(0),
            available: Condvar::new(),
            parent,
        })
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn acquire(
        self: &Arc<Self>,
        threads: usize,
        stop: Option<&Stop>,
        timeout: Option<Duration>,
    ) -> Result<CpuPermit, ReservationError> {
        if threads == 0 || threads > self.limit {
            return Err(ReservationError::Insufficient);
        }
        let started = Instant::now();
        let mut used = self.used.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if stop.is_some_and(Stop::is_set) {
                return Err(ReservationError::Cancelled);
            }
            if timeout.is_some_and(|limit| started.elapsed() >= limit) {
                return Err(ReservationError::Timeout);
            }
            if threads <= self.limit - *used {
                break;
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
        *used += threads;
        drop(used);
        let mut permit = CpuPermit {
            scope: Arc::clone(self),
            threads,
            parent: None,
        };
        if let Some(parent) = &self.parent {
            permit.parent = Some(Box::new(parent.acquire(
                threads,
                stop,
                timeout.map(|limit| limit.saturating_sub(started.elapsed())),
            )?));
        }
        Ok(permit)
    }
}

#[derive(Debug)]
pub(crate) struct CpuPermit {
    scope: Arc<CpuScope>,
    threads: usize,
    parent: Option<Box<Self>>,
}
impl Drop for CpuPermit {
    fn drop(&mut self) {
        let mut used = self
            .scope
            .used
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *used -= self.threads;
        self.scope.available.notify_all();
    }
}
