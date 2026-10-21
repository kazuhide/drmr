//! Fixed command-local observations, never raw tool output or user-supplied labels.
use drmr_core::events::{Record, Sink};
use std::io::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    last_activity: Option<&'static str>,
    started: u64,
    closed: u64,
    workers: Option<u64>,
    last_line: Option<Instant>,
}
pub(crate) struct Progress {
    enabled: bool,
    began: Instant,
    state: Mutex<State>,
}
fn activity(value: &str) -> Option<&'static str> {
    match value {
        "configuration" => Some("configuration"),
        "acquisition" => Some("acquisition"),
        "input-preparation" => Some("input-preparation"),
        "inspection" => Some("inspection"),
        "normalization" => Some("normalization"),
        "packaging" => Some("packaging"),
        "verification" => Some("verification"),
        "publication" => Some("publication"),
        _ => None,
    }
}
impl Progress {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            began: Instant::now(),
            state: Mutex::new(State::default()),
        }
    }
    pub fn mark(&self, value: &'static str) {
        let Some(value) = activity(value) else {
            return;
        };
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let changed = state.last_activity != Some(value);
        state.last_activity = Some(value);
        self.line(&mut state, changed);
    }
    pub fn last_activity(&self) -> Option<&'static str> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_activity
    }
    fn line(&self, state: &mut State, force: bool) {
        if !force
            && state
                .last_line
                .is_some_and(|last| last.elapsed() < Duration::from_millis(250))
        {
            return;
        }
        if let Some(activity) = state.last_activity {
            crate::common_job::progress(crate::common_job::Observation {
                elapsed_ms: u64::try_from(self.began.elapsed().as_millis()).unwrap_or(u64::MAX),
                last_activity: state.last_activity,
                scopes_started: state.started,
                scopes_closed: state.closed,
                workers: state.workers,
            });
            if self.enabled {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "Activity: {activity}; scopes started: {}; scopes closed: {}; elapsed: {:.1}s.",
                    state.started,
                    state.closed,
                    self.began.elapsed().as_secs_f64()
                );
            }
            state.last_line = Some(Instant::now());
        }
    }
    pub fn finish(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.line(&mut state, true);
    }
}
impl Sink for Progress {
    fn emit(&self, record: &Record) {
        if record.get("stage").and_then(serde_json::Value::as_str) == Some("resources") {
            let Some(workers) = record.get("workers").and_then(serde_json::Value::as_u64) else {
                return;
            };
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if self.enabled && state.workers != Some(workers) {
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "Using up to {workers} workers (CPU and memory limits)."
                );
            }
            state.workers = Some(workers);
            self.line(&mut state, true);
            return;
        }
        if record.get("stage").and_then(serde_json::Value::as_str) != Some("conversion") {
            return;
        }
        match record.get("event").and_then(serde_json::Value::as_str) {
            Some("activity") => {
                let Some(value) = record
                    .get("activity")
                    .and_then(serde_json::Value::as_str)
                    .and_then(activity)
                else {
                    return;
                };
                let transition = record.get("state").and_then(serde_json::Value::as_str);
                if !matches!(transition, Some("started" | "closed")) {
                    return;
                }
                let mut state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if transition == Some("started") {
                    state.started = state.started.saturating_add(1);
                } else {
                    state.closed = state.closed.saturating_add(1);
                }
                let changed = state.last_activity != Some(value);
                state.last_activity = Some(value);
                self.line(&mut state, changed);
            }
            Some("work-unit-completed") if self.enabled => {
                let Some(ordinal) = record.get("ordinal").and_then(serde_json::Value::as_u64)
                else {
                    return;
                };
                let Some(total) = record.get("total").and_then(serde_json::Value::as_u64) else {
                    return;
                };
                if ordinal < total {
                    let _ = writeln!(
                        std::io::stderr().lock(),
                        "Track work unit #{}/{} completed; elapsed: {:.1}s.",
                        ordinal + 1,
                        total,
                        self.began.elapsed().as_secs_f64()
                    );
                }
            }
            _ => {}
        }
    }
}
