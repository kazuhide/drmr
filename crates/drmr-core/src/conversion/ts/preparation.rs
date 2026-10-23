//! Bounded segment evidence collection; boundary validation remains in playlist order.
use super::super::{ConversionError, backend::Backend};
use crate::{
    concurrency::{Pool, Stop, Unit, run_ordered},
    events::Sink,
    resources::{Budget, Hardware},
};
use std::sync::{Arc, Mutex, PoisonError};

/// Compact evidence is retained for at most one coordinator window, never all media.
pub(super) const BATCH: usize = crate::concurrency::MAX_PENDING_TASKS;

pub(super) struct Batch<'a> {
    stop: &'a Stop,
    workers: usize,
    progress: Progress,
}
impl<'a> Batch<'a> {
    pub(super) fn new(
        backend: &'a Backend,
        total: usize,
        maximum_workers: Option<usize>,
    ) -> Result<Self, ConversionError> {
        backend.check_cancelled()?;
        let mut budget = Budget::choose(
            Hardware::detect(),
            backend.runner.registered_workload().unwrap_or_default(),
            maximum_workers,
        )
        .map_err(|_| ConversionError("resource-budget"))?;
        budget.workers = budget
            .workers
            .min(total.max(1))
            .min(backend.runner.native_capacity().0);
        backend.runner.note_resource_budget(&budget);
        Ok(Self {
            stop: &backend.stop,
            workers: budget.workers,
            progress: Progress::new(backend.activity.clone(), total),
        })
    }

    pub(super) fn collect<T: Send>(
        &self,
        count: usize,
        inspect: impl Fn(usize) -> Result<T, ConversionError> + Sync,
    ) -> Result<Vec<T>, ConversionError> {
        let inspect = |ordinal| {
            if self.stop.is_set() {
                return Err(ConversionError("cancelled"));
            }
            let result = inspect(ordinal)?;
            if self.stop.is_set() {
                return Err(ConversionError("cancelled"));
            }
            self.progress.completed();
            Ok(result)
        };
        if self.workers == 1 {
            return (0..count).map(inspect).collect();
        }
        std::thread::scope(|scope| {
            let pool = Pool::new(scope, self.workers.min(count).max(1));
            let units = (0..count).map(|ordinal| {
                let inspect = &inspect;
                Box::new(move || inspect(ordinal)) as Unit<'_, T, ConversionError>
            });
            // Use a private failure gate. Already-running tools keep the external
            // cancellation flag and drain before their scoped workspace can disappear.
            run_ordered(|unit| pool.submit(unit), units, None)
        })
    }

    /// Collection reaching its total is not success until ordered boundary checks pass.
    pub(super) fn finish(&self) -> Result<(), ConversionError> {
        if self.stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        self.progress.finish()
    }
}

struct State {
    completed: usize,
    finished: bool,
}
struct Progress {
    sink: Option<Arc<dyn Sink>>,
    total: usize,
    state: Mutex<State>,
}
impl Progress {
    fn new(sink: Option<Arc<dyn Sink>>, total: usize) -> Self {
        let progress = Self {
            sink,
            total,
            state: Mutex::new(State {
                completed: 0,
                finished: false,
            }),
        };
        progress.emit("started", 0);
        progress
    }
    fn emit(&self, state: &'static str, completed: usize) {
        if let Some(sink) = &self.sink {
            sink.emit(
                serde_json::json!({
                    "stage": "analysis",
                    "event": "progress",
                    "task": "ts-preparation",
                    "state": state,
                    "completed": completed,
                    "total": self.total,
                })
                .as_object()
                .expect("object"),
            );
        }
    }
    fn completed(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.completed += 1;
        // Keep emission under the counter lock so concurrent workers cannot
        // deliver an older cumulative snapshot after a newer one.
        self.emit("running", state.completed);
    }
    fn finish(&self) -> Result<(), ConversionError> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.completed != self.total {
            return Err(ConversionError("ts-segment-evidence-incomplete"));
        }
        state.finished = true;
        self.emit("finished", state.completed);
        Ok(())
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.finished {
            self.emit("aborted", state.completed);
        }
    }
}

#[cfg(test)]
mod tests;
