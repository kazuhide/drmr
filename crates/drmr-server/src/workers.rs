//! Owned background work and wakeable shutdown for the local server.

use std::io;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Default, Debug)]
struct State {
    closing: bool,
    failed: bool,
    handles: Vec<JoinHandle<()>>,
}

/// Threads remain owned until an explicit drain confirms their completion.
#[derive(Default, Debug)]
pub(crate) struct Workers {
    state: Mutex<State>,
    wake: Condvar,
    drain: Mutex<()>,
}

impl Workers {
    pub(crate) fn spawn(&self, name: &str, task: impl FnOnce() + Send + 'static) -> io::Result<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closing {
            return Err(io::Error::other("server-shutting-down"));
        }
        let mut pending = Vec::new();
        for handle in std::mem::take(&mut state.handles) {
            if handle.is_finished() {
                state.failed |= handle.join().is_err();
            } else {
                pending.push(handle);
            }
        }
        state.handles = pending;
        state
            .handles
            .push(std::thread::Builder::new().name(name.into()).spawn(task)?);
        Ok(())
    }

    pub(crate) fn close(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closing = true;
        self.wake.notify_all();
    }

    /// Wait unless closing; shutdown wakes timers without waiting for their interval.
    pub(crate) fn wait(&self, delay: Duration) -> bool {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (state, _) = self
            .wake
            .wait_timeout_while(state, delay, |state| !state.closing)
            .unwrap_or_else(PoisonError::into_inner);
        !state.closing
    }

    pub(crate) fn is_open(&self) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closing
    }

    pub(crate) fn shutdown(&self) -> io::Result<()> {
        let _drain = self.drain.lock().unwrap_or_else(PoisonError::into_inner);
        self.close();
        let handles = std::mem::take(
            &mut self
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .handles,
        );
        let mut failed = false;
        for handle in handles {
            failed |= handle.join().is_err();
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.failed |= failed;
        if state.failed {
            Err(io::Error::other("worker-cleanup-unconfirmed"))
        } else {
            Ok(())
        }
    }
}

/// A blocking dispatch remains counted even if the HTTP future is cancelled.
#[derive(Default, Debug)]
pub(crate) struct Requests {
    state: Mutex<RequestState>,
    wake: Condvar,
}

#[derive(Default, Debug)]
struct RequestState {
    closing: bool,
    active: usize,
}

pub(crate) struct RequestPermit(Arc<Requests>);

impl Requests {
    pub(crate) fn enter(self: &Arc<Self>) -> Option<RequestPermit> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closing {
            return None;
        }
        state.active += 1;
        Some(RequestPermit(self.clone()))
    }

    pub(crate) fn close(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closing = true;
    }

    pub(crate) fn drain(&self) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        drop(
            self.wake
                .wait_while(state, |state| state.active != 0)
                .unwrap_or_else(PoisonError::into_inner),
        );
    }
}

impl Drop for RequestPermit {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.active -= 1;
        self.0.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closing_wakes_a_long_timer_joins_it_and_rejects_new_work() {
        let workers = Arc::new(Workers::default());
        let weak = Arc::downgrade(&workers);
        let (ready, entered) = std::sync::mpsc::channel();
        let (done, exited) = std::sync::mpsc::channel();
        workers
            .spawn("timer-test", move || {
                let workers = weak.upgrade().unwrap();
                ready.send(()).unwrap();
                assert!(!workers.wait(Duration::from_secs(30)));
                done.send(()).unwrap();
            })
            .unwrap();
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        workers.shutdown().unwrap();
        exited.try_recv().unwrap();
        assert!(
            workers
                .spawn("late", || panic!("closed worker must not run"))
                .is_err()
        );
        workers.shutdown().unwrap();
    }

    #[test]
    fn a_failed_owned_worker_cannot_be_reported_as_successful_cleanup() {
        let workers = Workers::default();
        workers
            .spawn("failed-test", || panic!("injected worker failure"))
            .unwrap();
        assert!(workers.shutdown().is_err());
        assert!(workers.shutdown().is_err());
    }

    #[test]
    fn cancelled_dispatch_keeps_its_owned_work_counted_until_release() {
        let requests = Arc::new(Requests::default());
        let permit = requests.enter().unwrap();
        requests.close();
        assert!(requests.enter().is_none());
        let shared = requests.clone();
        let (done, drained) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            shared.drain();
            done.send(()).unwrap();
        });
        assert!(matches!(
            drained.recv_timeout(Duration::from_millis(25)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        drop(permit);
        drained.recv_timeout(Duration::from_secs(5)).unwrap();
        thread.join().unwrap();
    }
}
