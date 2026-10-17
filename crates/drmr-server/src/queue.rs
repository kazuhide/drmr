//! Sequential conversion queue shared by browser requests.

use crate::catalog;
use crate::config::{Clock, QueueRunner, ServerConfig};
use crate::hls::{HlsApiError, HlsService, ServiceError, is_catalog_id};
use crate::jobs::{ConversionJobManager, JobError};
use crate::pyutil;
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

/// `ConversionQueue.RECENT`: outcomes kept.
const RECENT: usize = 50;

#[derive(Debug)]
struct State {
    pending: Vec<(String, String)>,
    recent: Vec<Map<String, Value>>,
    /// The runner slot (`_thread is not None`).
    runner_active: bool,
    /// How many runners were started (the tests' thread factory count).
    runners_started: usize,
    mode: QueueRunner,
}

/// The conversion queue.
pub struct ConversionQueue {
    config: Arc<ServerConfig>,
    manager: Arc<ConversionJobManager>,
    service: Arc<HlsService>,
    clock: Clock,
    state: Mutex<State>,
    this: Weak<ConversionQueue>,
}

impl std::fmt::Debug for ConversionQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversionQueue").finish_non_exhaustive()
    }
}

impl ConversionQueue {
    /// A queue over this service and job runner.
    pub fn new(
        config: Arc<ServerConfig>,
        manager: Arc<ConversionJobManager>,
        service: Arc<HlsService>,
    ) -> Arc<ConversionQueue> {
        let clock = config
            .hooks
            .queue_clock
            .clone()
            .unwrap_or_else(|| Arc::new(pyutil::time_now));
        let mode = config.hooks.queue_runner;
        Arc::new_cyclic(|this| ConversionQueue {
            config,
            manager,
            service,
            clock,
            state: Mutex::new(State {
                pending: Vec::new(),
                recent: Vec::new(),
                runner_active: false,
                runners_started: 0,
                mode,
            }),
            this: this.clone(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn log(&self, record: &Value) {
        let _ = (self.config.hooks.log)(&pyutil::dumps(record));
    }

    /// `_now()`: `datetime.fromtimestamp(clock(), timezone.utc).isoformat()`.
    fn now(&self) -> String {
        pyutil::iso_from_timestamp((self.clock)())
    }

    /// Change how the runner starts (the tests swap the thread factory).
    pub fn set_runner_mode(&self, mode: QueueRunner) {
        self.lock().mode = mode;
    }

    /// Whether a runner holds the slot (`_thread is not None`).
    pub fn runner_active(&self) -> bool {
        self.lock().runner_active
    }

    /// How many runners have been started.
    pub fn runners_started(&self) -> usize {
        self.lock().runners_started
    }

    /// `snapshot()`: `{running, pending, recent}`.
    pub fn snapshot(&self) -> Value {
        let (pending, recent) = {
            let state = self.lock();
            let pending: Vec<Value> = state
                .pending
                .iter()
                .map(|(asset_id, queued_at)| pending_item(asset_id, queued_at))
                .collect();
            let recent: Vec<Value> = state.recent.iter().cloned().map(Value::Object).collect();
            (pending, recent)
        };
        let mut answer = Map::new();
        answer.insert(
            "running".into(),
            self.service
                .running_job()
                .map_or(Value::Null, Value::Object),
        );
        answer.insert("pending".into(), Value::Array(pending));
        answer.insert("recent".into(), Value::Array(recent));
        Value::Object(answer)
    }

    /// `enqueue(asset_ids)`: 1–500 distinct catalog ids, all-or-nothing; each must be a known
    /// catalog entry, not queued or running, not reserved by clear HLS.
    pub fn enqueue(&self, asset_ids: Option<&Value>) -> Result<Value, ServiceError> {
        let invalid = || ServiceError::from(HlsApiError::new("queue-parameters-invalid"));
        let Some(Value::Array(items)) = asset_ids else {
            return Err(invalid());
        };
        if items.is_empty() || items.len() > 500 {
            return Err(invalid());
        }
        let mut ids = Vec::with_capacity(items.len());
        for item in items {
            match item {
                Value::String(id) if is_catalog_id(id) => ids.push(id.clone()),
                _ => return Err(invalid()),
            }
        }
        if ids.iter().collect::<HashSet<_>>().len() != ids.len() {
            return Err(invalid());
        }
        // One catalog scan per request.
        let known: HashSet<String> = catalog::discover_catalog(&self.config.catalog_root, None)
            .map_err(|_| ServiceError::Internal)?
            .into_iter()
            .map(|entry| entry.asset_id)
            .collect();
        {
            let mut state = self.lock();
            // The running job is read under the queue lock: a launch happens inside tick()'s
            // locked section (record included), so it is never missed here.
            let running = self.service.running_job();
            let queued: HashSet<&String> = state.pending.iter().map(|(id, _)| id).collect();
            for asset_id in &ids {
                let detail = || {
                    let mut detail = Map::new();
                    detail.insert("asset_id".into(), Value::from(asset_id.clone()));
                    detail
                };
                if !known.contains(asset_id) {
                    return Err(HlsApiError::with_detail("asset-not-found", 404, detail()).into());
                }
                let running_this = running.as_ref().is_some_and(|row| {
                    row.get("asset_id").and_then(Value::as_str) == Some(asset_id)
                });
                if queued.contains(asset_id) || running_this {
                    return Err(
                        HlsApiError::with_detail("asset-already-queued", 409, detail()).into(),
                    );
                }
                if self.service.asset_reserved(asset_id) {
                    return Err(
                        HlsApiError::with_detail("asset-already-exists", 409, detail()).into(),
                    );
                }
            }
            for asset_id in ids {
                let queued_at = self.now();
                state.pending.push((asset_id, queued_at));
            }
        }
        self.ensure_runner();
        Ok(self.snapshot())
    }

    /// `remove(asset_id)`: drop a pending item; `queue-item-not-found` (404) when absent.
    pub fn remove(&self, asset_id: &str) -> Result<(), HlsApiError> {
        let mut state = self.lock();
        let before = state.pending.len();
        state.pending.retain(|(id, _)| id != asset_id);
        if state.pending.len() == before {
            return Err(HlsApiError::status("queue-item-not-found", 404));
        }
        Ok(())
    }

    fn note(
        &self,
        state: &mut State,
        asset_id: &str,
        job_id: Option<&str>,
        outcome: &str,
        reason: Option<&str>,
    ) {
        let mut item = Map::new();
        item.insert("asset_id".into(), Value::from(asset_id));
        item.insert("job_id".into(), job_id.map_or(Value::Null, Value::from));
        item.insert("outcome".into(), Value::from(outcome));
        item.insert("reason".into(), reason.map_or(Value::Null, Value::from));
        item.insert("at".into(), Value::from(self.now()));
        state.recent.insert(0, item);
        state.recent.truncate(RECENT);
    }

    fn skip_event(&self, asset_id: &str, error: &str) {
        let mut event = Map::new();
        event.insert("event".into(), Value::from("conversion-queue-skip"));
        event.insert("asset_id".into(), Value::from(asset_id));
        event.insert("error".into(), Value::from(error));
        self.log(&Value::Object(event));
    }

    /// `tick()`: launch the head if the lease is free; the launched job id. A head that cannot
    /// be examined or launched is removed and recorded as skipped with a fixed reason.
    pub fn tick(&self) -> Option<String> {
        let mut state = self.lock();
        let (head, _) = state.pending.first()?.clone();
        if self.manager.is_busy() {
            return None; // the lease is taken: no catalog scan until it is free
        }
        let entry = match catalog::get_catalog_entry(&self.config.catalog_root, &head) {
            Ok(entry) => entry,
            Err(error) => {
                // A catalog scan error (permissions, I/O): the class is diagnostic, never a path.
                state.pending.remove(0);
                self.note(
                    &mut state,
                    &head,
                    None,
                    "skipped",
                    Some("catalog-scan-failed"),
                );
                self.skip_event(&head, error.class_name());
                return None;
            }
        };
        let reserved = self.service.asset_reserved(&head);
        let entry = match entry {
            Some(entry) if !reserved => entry,
            other => {
                let reason = if other.is_none() {
                    "asset-not-found"
                } else {
                    "asset-already-exists"
                };
                state.pending.remove(0);
                self.note(&mut state, &head, None, "skipped", Some(reason));
                return None;
            }
        };
        let handle = match self.manager.submit(&entry, &self.config.output_root) {
            Ok(handle) => handle,
            Err(JobError::AlreadyRunning) => return None,
            Err(error) => {
                // Any launch failure: the head is skipped, never retried forever.
                state.pending.remove(0);
                self.note(
                    &mut state,
                    &head,
                    None,
                    "skipped",
                    Some("job-launch-failed"),
                );
                let class = match &error {
                    JobError::Launch(io) => pyutil::exception_name(io),
                    _ => "RuntimeError",
                };
                self.skip_event(&head, class);
                return None;
            }
        };
        state.pending.remove(0);
        self.note(&mut state, &head, Some(&handle.job_id), "launched", None);
        // Inside the lock: enqueue() sees the launch.
        self.service.record_conversion(&handle, &head);
        Some(handle.job_id)
    }

    /// `_ensure_runner()`: start a runner unless one holds the slot.
    fn ensure_runner(&self) {
        let mut state = self.lock();
        if state.mode == QueueRunner::Disabled || state.runner_active {
            return;
        }
        match state.mode {
            QueueRunner::Thread => {
                let this = self.this.clone();
                let started = std::thread::Builder::new()
                    .name("conversion-queue".into())
                    .spawn(move || run(&this));
                if started.is_err() {
                    drop(state);
                    self.runner_failed();
                    return;
                }
            }
            QueueRunner::FailToStart => {
                drop(state);
                self.runner_failed();
                return;
            }
            QueueRunner::Inert | QueueRunner::Disabled => {}
        }
        state.runner_active = true;
        state.runners_started += 1;
    }

    fn runner_failed(&self) {
        // The slot stays free so the next enqueue tries again.
        let mut event = Map::new();
        event.insert(
            "event".into(),
            Value::from("conversion-queue-runner-failed"),
        );
        event.insert("error".into(), Value::from("RuntimeError"));
        self.log(&Value::Object(event));
    }

    /// `_run_once()`: `false` when the queue was empty and the slot was released (in the same
    /// critical section that saw it empty).
    pub fn run_once(&self) -> bool {
        {
            let mut state = self.lock();
            if state.pending.is_empty() {
                state.runner_active = false;
                return false;
            }
        }
        // A tick never panics on bad input; `tick()` failures are its recorded skips.
        let _ = self.tick();
        true
    }
}

fn pending_item(asset_id: &str, queued_at: &str) -> Value {
    let mut item = Map::new();
    item.insert("asset_id".into(), Value::from(asset_id));
    item.insert("queued_at".into(), Value::from(queued_at));
    Value::Object(item)
}

/// `_run()`: tick every half second until the queue is empty.
fn run(queue: &Weak<ConversionQueue>) {
    loop {
        let Some(queue) = queue.upgrade() else {
            return;
        };
        if !queue.run_once() {
            return;
        }
        drop(queue);
        std::thread::sleep(Duration::from_millis(500));
    }
}
