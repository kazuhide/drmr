//! Optional numeric acquisition snapshots; no source-derived strings enter events.
use crate::events::Sink;
use std::{
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};
const INTERVAL: Duration = Duration::from_millis(250);
const BATCH: u64 = 64 * 1024;
struct State {
    started: u64,
    completed: u64,
    bytes: u64,
    last: Instant,
}
pub(crate) struct Progress {
    sink: Arc<dyn Sink>,
    state: Mutex<State>,
}
impl Progress {
    pub(crate) fn new(sink: Arc<dyn Sink>) -> Arc<Self> {
        let progress = Arc::new(Self {
            sink,
            state: Mutex::new(State {
                started: 0,
                completed: 0,
                bytes: 0,
                last: Instant::now(),
            }),
        });
        progress.update(0, 0, 0, true);
        progress
    }
    fn update(&self, started: u64, completed: u64, bytes: u64, force: bool) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.started = state.started.saturating_add(started);
        state.completed = state.completed.saturating_add(completed);
        state.bytes = state.bytes.saturating_add(bytes);
        if force || state.last.elapsed() >= INTERVAL {
            state.last = Instant::now();
            // Serialize snapshot delivery with updates, so parallel emitters cannot reorder
            // cumulative counters. Only fixed labels and measured integers are supplied.
            self.sink.emit(
                serde_json::json!({
                    "stage": "acquisition",
                    "event": "progress",
                    "resources_started": state.started,
                    "resources_completed": state.completed,
                    "bytes_received": state.bytes,
                })
                .as_object()
                .expect("object"),
            );
        }
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        self.update(0, 0, 0, true);
    }
}
/// Each resource batches body bytes locally, including discarded retry/failed bytes.
/// Completion is explicit; dropping a partial transfer never increments it.
pub(crate) struct Transfer {
    progress: Option<Arc<Progress>>,
    pending: u64,
    last: Instant,
}
impl Transfer {
    pub(crate) fn new(progress: Option<&Arc<Progress>>) -> Self {
        if let Some(progress) = progress {
            progress.update(1, 0, 0, false);
        }
        Self {
            progress: progress.cloned(),
            pending: 0,
            last: Instant::now(),
        }
    }
    pub(crate) fn received(&mut self, count: usize) {
        if self.progress.is_none() {
            return;
        }
        self.pending = self.pending.saturating_add(count as u64);
        if self.pending >= BATCH || self.last.elapsed() >= INTERVAL {
            self.flush();
        }
    }
    fn flush(&mut self) {
        if let Some(progress) = &self.progress
            && self.pending > 0
        {
            progress.update(0, 0, std::mem::take(&mut self.pending), false);
            self.last = Instant::now();
        }
    }
    pub(crate) fn complete(&mut self) {
        self.flush();
        if let Some(progress) = &self.progress {
            progress.update(0, 1, 0, false);
        }
    }
}
impl Drop for Transfer {
    fn drop(&mut self) {
        self.flush();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Collector;
    #[test]
    fn parallel_success_and_partial_failure_keep_exact_monotonic_private_counters() {
        let sink = Arc::new(Collector::default());
        let progress = Progress::new(sink.clone());
        std::thread::scope(|scope| {
            for n in 0..8 {
                let p = progress.clone();
                scope.spawn(move || {
                    let mut transfer = Transfer::new(Some(&p));
                    for _ in 0..1000 {
                        transfer.received(1);
                    }
                    if n != 0 {
                        transfer.complete();
                    }
                });
            }
        });
        drop(progress);
        let events = sink.records();
        let last = events.last().unwrap();
        assert_eq!(last["resources_started"], 8);
        assert_eq!(last["resources_completed"], 7);
        assert_eq!(last["bytes_received"], 8000);
        for event in &events {
            assert_eq!(event.len(), 5);
            assert_eq!(event["stage"], "acquisition");
            assert_eq!(event["event"], "progress");
        }
        for pair in events.windows(2) {
            for field in ["resources_started", "resources_completed", "bytes_received"] {
                assert!(pair[0][field].as_u64().unwrap() <= pair[1][field].as_u64().unwrap());
            }
        }
        assert!(events.len() <= 10, "tiny payload writes must be batched");
    }
}
