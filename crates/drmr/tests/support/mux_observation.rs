//! Observe a mux attempt separately from the fixture's actual readiness.
use drmr_core::{
    concurrency::Stop,
    events::{Record, Sink},
};
use std::{
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Debug, Default)]
struct State {
    attempt: Option<Instant>,
    terminal: Option<&'static str>,
    last_activity: Option<(&'static str, &'static str)>,
}

#[derive(Debug, Default)]
pub struct MuxObservation {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ObservationFailure {
    pub phase: &'static str,
    pub conversion: Option<&'static str>,
    pub last_activity: Option<(&'static str, &'static str)>,
}

fn failure(state: &State, phase: &'static str) -> ObservationFailure {
    ObservationFailure {
        phase,
        conversion: state.terminal,
        last_activity: state.last_activity,
    }
}

pub struct DelayedPreflight {
    delayed: AtomicBool,
    duration: Duration,
    observation: Arc<MuxObservation>,
}

impl DelayedPreflight {
    pub fn new(observation: Arc<MuxObservation>, duration: Duration) -> Self {
        Self {
            delayed: AtomicBool::new(false),
            duration,
            observation,
        }
    }
}

impl Sink for DelayedPreflight {
    fn emit(&self, record: &Record) {
        if record.get("tool").and_then(serde_json::Value::as_str) == Some("ffprobe")
            && record.get("event").and_then(serde_json::Value::as_str) == Some("command")
            && !self.delayed.swap(true, Ordering::SeqCst)
        {
            // Deliberately exceed the mux-readiness budget while still in preflight.
            std::thread::sleep(self.duration);
        }
        self.observation.emit(record);
    }
}

impl Sink for MuxObservation {
    fn emit(&self, record: &Record) {
        let tool = match record.get("tool").and_then(serde_json::Value::as_str) {
            Some("ffprobe") => "ffprobe",
            Some("ffmpeg") => "ffmpeg",
            _ => return,
        };
        let event = match record.get("event").and_then(serde_json::Value::as_str) {
            Some("command") => "command",
            Some("tool-output") => "tool-output",
            _ => return,
        };
        let mut state = self.state.lock().expect("mux observation lock");
        state.last_activity = Some((tool, event));
        // A command record precedes admission and spawn. Only the fixture proves readiness.
        if tool == "ffmpeg" && event == "command" && state.attempt.is_none() {
            state.attempt = Some(Instant::now());
        }
        self.changed.notify_all();
    }
}

impl MuxObservation {
    pub fn complete(&self, code: &'static str) {
        self.state.lock().expect("mux observation lock").terminal = Some(code);
        self.changed.notify_all();
    }

    pub fn cancel_when_ready(
        &self,
        ready: &Path,
        stop: &Stop,
        preflight_budget: Duration,
        readiness_budget: Duration,
    ) -> Result<(), ObservationFailure> {
        let preflight_deadline = Instant::now() + preflight_budget;
        let mut state = self.state.lock().expect("mux observation lock");
        loop {
            if state.terminal.is_some() {
                let phase = if state.attempt.is_some() {
                    "mux-terminal-before-ready"
                } else {
                    "preflight-terminal"
                };
                return Err(failure(&state, phase));
            }
            let (deadline, phase) = match state.attempt {
                Some(attempt) => (attempt + readiness_budget, "mux-readiness-deadline"),
                None => (preflight_deadline, "preflight-deadline"),
            };
            let now = Instant::now();
            if now >= deadline {
                return Err(failure(&state, phase));
            }
            if state.attempt.is_some() {
                match std::fs::read(ready) {
                    Ok(bytes) if bytes == b"ready" => {
                        stop.set();
                        return Ok(());
                    }
                    // Publishing the five-byte marker can briefly expose an empty file.
                    Ok(bytes) if bytes.is_empty() => {}
                    Ok(_) => return Err(failure(&state, "invalid-readiness-marker")),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(failure(&state, "readiness-file-failed")),
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let pause = if state.attempt.is_some() {
                remaining.min(Duration::from_millis(10))
            } else {
                remaining
            };
            state = self
                .changed
                .wait_timeout(state, pause)
                .expect("mux observation wait")
                .0;
        }
    }
}

fn mux_command() -> Record {
    let mut record = Record::new();
    record.insert("tool".into(), "ffmpeg".into());
    record.insert("event".into(), "command".into());
    record
}

#[test]
fn a_mux_attempt_without_actual_readiness_cannot_cancel() {
    let root = tempfile::tempdir().unwrap();
    let observation = MuxObservation::default();
    observation.emit(&mux_command());
    let stop = Stop::new();
    let error = observation
        .cancel_when_ready(
            &root.path().join("absent"),
            &stop,
            Duration::from_secs(10),
            Duration::ZERO,
        )
        .unwrap_err();
    assert_eq!(error.phase, "mux-readiness-deadline");
    assert!(!stop.is_set());
}

#[test]
fn a_preflight_failure_preserves_its_error_and_cannot_cancel() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    std::fs::write(&ready, b"ready").unwrap();
    let observation = MuxObservation::default();
    let mut output = Record::new();
    output.insert("tool".into(), "ffprobe".into());
    output.insert("event".into(), "tool-output".into());
    observation.emit(&output);
    observation.complete("media-tool-failed");
    let stop = Stop::new();
    let error = observation
        .cancel_when_ready(
            &ready,
            &stop,
            Duration::from_secs(10),
            Duration::from_secs(10),
        )
        .unwrap_err();
    assert_eq!(error.conversion, Some("media-tool-failed"));
    assert_eq!(error.last_activity, Some(("ffprobe", "tool-output")));
    assert!(!stop.is_set());
}

#[test]
fn a_terminal_mux_does_not_become_active_cancellation_from_a_ready_marker() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    std::fs::write(&ready, b"ready").unwrap();
    let observation = MuxObservation::default();
    observation.emit(&mux_command());
    observation.complete("media-tool-failed");
    let stop = Stop::new();
    let error = observation
        .cancel_when_ready(
            &ready,
            &stop,
            Duration::from_secs(10),
            Duration::from_secs(10),
        )
        .unwrap_err();
    assert_eq!(error.conversion, Some("media-tool-failed"));
    assert_eq!(error.last_activity, Some(("ffmpeg", "command")));
    assert!(!stop.is_set());
}

#[test]
fn an_invalid_readiness_marker_cannot_cancel() {
    let root = tempfile::tempdir().unwrap();
    let ready = root.path().join("ready");
    std::fs::write(&ready, b"not-ready").unwrap();
    let observation = MuxObservation::default();
    observation.emit(&mux_command());
    let stop = Stop::new();
    let error = observation
        .cancel_when_ready(
            &ready,
            &stop,
            Duration::from_secs(10),
            Duration::from_secs(10),
        )
        .unwrap_err();
    assert_eq!(error.phase, "invalid-readiness-marker");
    assert!(!stop.is_set());
}
