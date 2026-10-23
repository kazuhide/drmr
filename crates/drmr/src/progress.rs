//! Bounded command observations built from fixed labels and numeric counters only.
use drmr_core::events::{Record, Sink};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::{IsTerminal as _, Write};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod display;

#[derive(Clone, Copy)]
struct Analysis {
    completed: u64,
    total: u64,
    began: Duration,
    state: &'static str,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum Mode {
    #[default]
    Auto,
    Text,
    Json,
    Off,
}

struct Configuration {
    mode: Mode,
    command: &'static str,
}
static CONFIGURATION: OnceLock<Configuration> = OnceLock::new();

fn resolved(mode: Mode, human_result: bool, terminal: bool) -> Mode {
    match mode {
        Mode::Auto if human_result || terminal => Mode::Text,
        Mode::Auto => Mode::Off,
        value => value,
    }
}

pub(crate) fn configure(mode: Mode, command: &'static str) {
    let _ = CONFIGURATION.set(Configuration { mode, command });
}

#[derive(Default)]
struct State {
    last_activity: Option<&'static str>,
    acquiring: bool,
    started: u64,
    closed: u64,
    workers: Option<u64>,
    last_line: Option<Instant>,
    finished: bool,
    output_failed: bool,
    tools: [u64; 3],
    tools_started: u64,
    tools_closed: u64,
    tasks: [u64; 4],
    downloads: Option<[u64; 3]>,
    resources_total: Option<u64>,
    resources_began: Option<(Duration, u64)>,
    analysis: Option<Analysis>,
    work_completed: u64,
    display: display::Display,
}

struct Inner {
    mode: Mode,
    command: &'static str,
    run_id: String,
    began: Instant,
    state: Mutex<State>,
    wake: Condvar,
    out: Mutex<Box<dyn Write + Send>>,
    terminal: Option<ProgressBar>,
}

pub(crate) struct Progress {
    inner: Arc<Inner>,
    heartbeat: Mutex<Option<JoinHandle<()>>>,
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
        "tool-check" => Some("tool-check"),
        "server-startup" => Some("server-startup"),
        "serving" => Some("serving"),
        _ => None,
    }
}

fn description(value: &str) -> &'static str {
    match value {
        "configuration" => "Checking command settings",
        "acquisition" => "Downloading source metadata and resources",
        "input-preparation" => "Preparing and checking source files",
        "inspection" => "Reading container, track, and encryption metadata",
        "normalization" => "Preparing media for the selected output format",
        "packaging" => "Writing media and encryption signaling",
        "verification" => "Checking media content, packet timing, and manifests",
        "publication" => "Writing the checked result",
        "tool-check" => "Checking external media tools",
        "server-startup" => "Starting the local web console",
        "serving" => "Running the local web console",
        _ => "Waiting for activity",
    }
}

const TOOLS: [&str; 3] = ["ffmpeg", "ffprobe", "packager"];
const TASKS: [(&str, &str); 4] = [
    (
        "packet-timing",
        "Reading complete packet timestamps and durations",
    ),
    ("encoded-content", "Hashing complete encoded media payloads"),
    (
        "decoded-content",
        "Decoding and hashing complete media content",
    ),
    (
        "manifest-lint",
        "Checking final manifest syntax and signaling",
    ),
];

impl Progress {
    pub(crate) fn new(human_result: bool) -> Self {
        let (mode, command) = CONFIGURATION
            .get()
            .map_or((Mode::Auto, "job"), |c| (c.mode, c.command));
        let terminal = std::io::stderr().is_terminal();
        let mode = resolved(mode, human_result, terminal);
        let bar = if terminal && matches!(mode, Mode::Text) {
            let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr_with_hz(10));
            if bar.is_hidden() {
                None
            } else {
                bar.set_style(
                    ProgressStyle::with_template("{spinner:.cyan} {wide_msg}")
                        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                );
                bar.enable_steady_tick(Duration::from_millis(100));
                Some(bar)
            }
        } else {
            None
        };
        Self::with_terminal(
            mode,
            command,
            Box::new(std::io::stderr()),
            Duration::from_secs(2),
            bar,
        )
    }

    #[cfg(test)]
    fn with_writer(
        mode: Mode,
        command: &'static str,
        out: Box<dyn Write + Send>,
        interval: Duration,
    ) -> Self {
        Self::with_terminal(mode, command, out, interval, None)
    }

    fn with_terminal(
        mode: Mode,
        command: &'static str,
        out: Box<dyn Write + Send>,
        interval: Duration,
        terminal: Option<ProgressBar>,
    ) -> Self {
        let inner = Arc::new(Inner {
            mode,
            command,
            run_id: format!(
                "{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            began: Instant::now(),
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            out: Mutex::new(out),
            terminal,
        });
        let heartbeat = if !matches!(mode, Mode::Off) || crate::common_job::active() {
            let observed = Arc::clone(&inner);
            std::thread::Builder::new()
                .name("drmr-progress".into())
                .spawn(move || {
                    let mut state = observed
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    loop {
                        let (next, _) = observed
                            .wake
                            .wait_timeout_while(state, interval, |s| !s.finished)
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state = next;
                        if state.finished {
                            break;
                        }
                        if state.last_activity.is_some() {
                            observed.line(&mut state, "heartbeat", None, false);
                        }
                    }
                })
                .ok()
        } else {
            None
        };
        Self {
            inner,
            heartbeat: Mutex::new(heartbeat),
        }
    }

    pub(crate) fn mark(&self, value: &'static str) {
        let Some(value) = activity(value) else {
            return;
        };
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.finished {
            return;
        }
        let changed = state.last_activity != Some(value);
        state.acquiring = value == "acquisition";
        if value != "input-preparation" {
            state.analysis = None;
        }
        state.last_activity = Some(value);
        self.inner.line(&mut state, "activity", None, changed);
    }

    pub(crate) fn last_activity(&self) -> Option<&'static str> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .last_activity
    }

    pub(crate) fn structured(&self) -> bool {
        matches!(self.inner.mode, Mode::Json)
    }

    pub(crate) fn finish(&self) {
        self.stop("finished", None);
    }

    pub(crate) fn fail(&self, code: &'static str) {
        self.stop(
            if code == "cancelled" {
                "cancelled"
            } else {
                "failed"
            },
            Some(code),
        );
    }

    pub(crate) fn diagnostic(&self, command: &str, code: &'static str) {
        self.fail(code);
        if matches!(self.inner.mode, Mode::Off | Mode::Auto) {
            eprintln!("{command}: {code}");
        }
    }

    fn stop(&self, event: &'static str, code: Option<&'static str>) {
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !state.finished || code.is_some() {
                state.finished = true;
                self.inner.line(&mut state, event, code, true);
                self.inner.wake.notify_all();
            }
        }
        if let Some(worker) = self
            .heartbeat
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = worker.join();
        }
        if let Some(bar) = &self.inner.terminal {
            bar.disable_steady_tick();
            bar.finish_and_clear();
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        self.finish();
    }
}

impl Inner {
    fn line(
        &self,
        state: &mut State,
        event: &'static str,
        error: Option<&'static str>,
        force: bool,
    ) {
        let observe = force
            || state
                .last_line
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(250));
        if !observe && !matches!(self.mode, Mode::Text) {
            return;
        }
        let elapsed_ms = u64::try_from(self.began.elapsed().as_millis()).unwrap_or(u64::MAX);
        if observe {
            crate::common_job::progress(crate::common_job::Observation {
                elapsed_ms,
                last_activity: state.last_activity,
                scopes_started: state.started,
                scopes_closed: state.closed,
                workers: state.workers,
            });
            state.last_line = Some(Instant::now());
        }
        if state.output_failed || matches!(self.mode, Mode::Off | Mode::Auto) {
            return;
        }
        let active: Vec<_> = TOOLS
            .iter()
            .zip(state.tools)
            .filter_map(|(name, count)| (count > 0).then_some(*name))
            .collect();
        let tasks: Vec<_> = TASKS
            .iter()
            .zip(state.tasks)
            .filter_map(|((_, label), count)| (count > 0).then_some(*label))
            .collect();
        let task = if tasks.is_empty() {
            description(state.last_activity.unwrap_or("")).to_string()
        } else {
            tasks.join("; ")
        };
        let mut out = self
            .out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = if matches!(self.mode, Mode::Json) {
            let mut value = serde_json::json!({
                "schema_version": 1, "entry_point": "cli", "command": self.command,
                "run_id": self.run_id, "event": event, "elapsed_ms": elapsed_ms,
                "activity": state.last_activity, "task": task,
                "scopes_started": state.started, "scopes_closed": state.closed,
                "active_scopes": state.started.saturating_sub(state.closed),
                "tools_started": state.tools_started, "tools_closed": state.tools_closed,
                "active_tool_count": state.tools.iter().copied().sum::<u64>(),
                "active_tools": active, "work_units_completed": state.work_completed,
            });
            if let Some(workers) = state.workers {
                value["workers"] = workers.into();
            }
            if let Some([started, completed, bytes]) = state.downloads {
                value["resources_started"] = started.into();
                value["resources_completed"] = completed.into();
                value["bytes_received"] = bytes.into();
            }
            if let Some(total) = state.resources_total {
                value["resources_total"] = total.into();
            }
            if let Some(analysis) = state.analysis {
                value["analysis_task"] = "ts-preparation".into();
                value["analysis_state"] = analysis.state.into();
                value["analysis_completed"] = analysis.completed.into();
                value["analysis_total"] = analysis.total.into();
            }
            if let Some(code) = error {
                value["error_code"] = code.into();
            }
            serde_json::to_writer(&mut *out, &value)
                .map_err(std::io::Error::other)
                .and_then(|()| writeln!(out))
        } else {
            let activity = state.last_activity.unwrap_or("configuration");
            let units = state.units(activity);
            let view = display::View {
                activity,
                task: if tasks.is_empty() { "" } else { &task },
                tools: &active,
                downloads: state.downloads,
                units,
            };
            state.display.render(
                &mut *out,
                self.terminal.as_ref(),
                self.command,
                &view,
                self.began.elapsed(),
                matches!(event, "finished" | "failed" | "cancelled").then_some((event, error)),
            )
        };
        if result.and_then(|()| out.flush()).is_err() {
            state.output_failed = true;
        }
    }
}

impl State {
    fn units(&self, activity: &str) -> Option<display::Units> {
        if let Some(analysis) = self.analysis.filter(|a| a.state != "aborted") {
            Some(display::Units {
                completed: analysis.completed,
                total: analysis.total,
                initial: 0,
                began: analysis.began,
                segments: true,
                checking: analysis.completed == analysis.total && analysis.state != "finished",
            })
        } else if activity == "acquisition" {
            self.downloads
                .zip(self.resources_total)
                .zip(self.resources_began)
                .map(
                    |(([_, completed, _], total), (began, initial))| display::Units {
                        completed,
                        total,
                        initial,
                        began,
                        segments: false,
                        checking: false,
                    },
                )
        } else {
            None
        }
    }

    fn observe_download(&mut self, record: &Record, now: Duration) -> Option<()> {
        let get = |key| record.get(key).and_then(serde_json::Value::as_u64);
        let (Some(started), Some(completed), Some(bytes)) = (
            get("resources_started"),
            get("resources_completed"),
            get("bytes_received"),
        ) else {
            return None;
        };
        if completed > started {
            return None;
        }
        let total = record.get("resources_total");
        let parsed = total.and_then(serde_json::Value::as_u64);
        if total.is_some() && parsed.is_none_or(|n| completed > n || started > n) {
            return None;
        }
        if parsed != self.resources_total {
            self.resources_began = parsed.map(|_| (now, completed));
        }
        self.resources_total = parsed;
        self.downloads = Some([started, completed, bytes]);
        self.last_activity = Some("acquisition");
        Some(())
    }

    fn observe_activity(&mut self, record: &Record) -> Option<()> {
        let value = record
            .get("activity")
            .and_then(serde_json::Value::as_str)
            .and_then(activity)?;
        match record.get("state").and_then(serde_json::Value::as_str) {
            Some("started") => {
                self.started = self.started.saturating_add(1);
                if value != "input-preparation" {
                    self.analysis = None;
                }
            }
            Some("closed") => self.closed = self.closed.saturating_add(1),
            _ => return None,
        }
        self.last_activity = Some(if self.acquiring { "acquisition" } else { value });
        Some(())
    }

    fn observe_analysis(&mut self, record: &Record, now: Duration) -> Option<bool> {
        if record.get("task").and_then(serde_json::Value::as_str) != Some("ts-preparation") {
            return None;
        }
        let (Some(completed), Some(total)) = (
            record.get("completed").and_then(serde_json::Value::as_u64),
            record.get("total").and_then(serde_json::Value::as_u64),
        ) else {
            return None;
        };
        let status = match record.get("state").and_then(serde_json::Value::as_str) {
            Some("started") if completed == 0 => "started",
            Some("running") => "running",
            Some("finished") if completed == total => "finished",
            Some("aborted") => "aborted",
            _ => return None,
        };
        if completed > total {
            return None;
        }
        let began = if status == "started" {
            now
        } else if let Some(prior) = self.analysis {
            if prior.total != total || prior.completed > completed {
                return None;
            }
            prior.began
        } else {
            return None;
        };
        self.analysis = Some(Analysis {
            completed,
            total,
            began,
            state: status,
        });
        self.last_activity = Some("input-preparation");
        Some(status != "running")
    }

    fn tool(&mut self, record: &Record, started: bool) -> Option<bool> {
        let index = record
            .get("tool")
            .and_then(serde_json::Value::as_str)
            .and_then(|v| TOOLS.iter().position(|t| *t == v))?;
        let changed = started && self.tools[index] == 0;
        if started {
            self.tools[index] = self.tools[index].saturating_add(1);
            self.tools_started = self.tools_started.saturating_add(1);
        } else if self.tools[index] > 0 {
            self.tools[index] -= 1;
            self.tools_closed = self.tools_closed.saturating_add(1);
        }
        Some(changed)
    }
}

impl Sink for Progress {
    fn emit(&self, record: &Record) {
        let record_stage = record.get("stage").and_then(serde_json::Value::as_str);
        let event = record.get("event").and_then(serde_json::Value::as_str);
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.finished {
            return;
        }
        let previous = state.last_activity;
        let mut force = false;
        match (record_stage, event) {
            (Some("resources"), _) => {
                let Some(workers) = record.get("workers").and_then(serde_json::Value::as_u64)
                else {
                    return;
                };
                force = state.workers != Some(workers);
                state.workers = Some(workers);
            }
            (Some("acquisition"), Some("progress")) => {
                if state
                    .observe_download(record, self.inner.began.elapsed())
                    .is_none()
                {
                    return;
                }
            }
            (Some("analysis"), Some("progress")) => {
                let Some(changed) = state.observe_analysis(record, self.inner.began.elapsed())
                else {
                    return;
                };
                force = changed;
            }
            (Some("tool"), Some("command" | "tool-output")) => {
                let Some(changed) = state.tool(record, event == Some("command")) else {
                    return;
                };
                force = changed;
            }
            (Some("conversion"), Some("activity")) => {
                if state.observe_activity(record).is_none() {
                    return;
                }
            }
            (Some("conversion"), Some("task")) => {
                let Some(index) = record
                    .get("task")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|v| TASKS.iter().position(|(name, _)| *name == v))
                else {
                    return;
                };
                match record.get("state").and_then(serde_json::Value::as_str) {
                    Some("started") => {
                        force = state.tasks[index] == 0;
                        state.tasks[index] = state.tasks[index].saturating_add(1);
                    }
                    Some("closed") => {
                        state.tasks[index] = state.tasks[index].saturating_sub(1);
                    }
                    _ => return,
                }
            }
            (Some("conversion"), Some("work-unit-completed")) => {
                let (Some(ordinal), Some(total)) = (
                    record.get("ordinal").and_then(serde_json::Value::as_u64),
                    record.get("total").and_then(serde_json::Value::as_u64),
                ) else {
                    return;
                };
                if ordinal >= total {
                    return;
                }
                state.work_completed = state.work_completed.saturating_add(1);
            }
            _ => return,
        }
        force |= previous != state.last_activity;
        self.inner.line(&mut state, "progress", None, force);
        if state.analysis.is_some_and(|a| a.state == "finished") {
            // Preserve the final count in history, then reveal the remaining
            // whole-track preparation rather than leaving a stale 100% visible.
            state.analysis = None;
            self.inner.line(&mut state, "progress", None, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    struct Captured {
        bytes: Arc<Mutex<Vec<u8>>>,
        flushed: mpsc::Sender<()>,
    }
    impl Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            let _ = self.flushed.send(());
            Ok(())
        }
    }
    fn records(bytes: &Arc<Mutex<Vec<u8>>>) -> Vec<serde_json::Value> {
        String::from_utf8(bytes.lock().unwrap().clone())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn observed(interval: Duration) -> (Progress, Arc<Mutex<Vec<u8>>>, mpsc::Receiver<()>) {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (sent, received) = mpsc::channel();
        let progress = Progress::with_writer(
            Mode::Json,
            "analyze",
            Box::new(Captured {
                bytes: bytes.clone(),
                flushed: sent,
            }),
            interval,
        );
        (progress, bytes, received)
    }
    #[test]
    fn heartbeat_observes_idle_work_and_drop_joins_its_worker() {
        // Use the production throttle interval; receiving a flush bounds this test.
        let (progress, bytes, updates) = observed(Duration::from_millis(300));
        progress.mark("verification");
        updates.recv_timeout(Duration::from_secs(5)).unwrap();
        updates.recv_timeout(Duration::from_secs(5)).unwrap();
        let values = records(&bytes);
        assert!(
            values
                .iter()
                .any(|v| v["event"] == "heartbeat" && v["activity"] == "verification")
        );
        drop(progress);
        assert_eq!(records(&bytes).last().unwrap()["event"], "finished");
        assert_eq!(
            Arc::strong_count(&bytes),
            1,
            "worker and writer have been dropped"
        );
    }
    #[test]
    fn only_fixed_tool_labels_and_numeric_observations_reach_output() {
        let (progress, bytes, _) = observed(Duration::from_mins(1));
        progress.mark("verification");
        for value in [
            serde_json::json!({"stage":"tool","event":"command","tool":"ffmpeg","argv":["secret-sentinel"],"rendition_id":"private-sentinel"}),
            serde_json::json!({"stage":"tool","event":"tool-output","tool":"ffmpeg","stdout":"secret-sentinel","stderr":"secret-sentinel"}),
            serde_json::json!({"stage":"conversion","event":"activity","activity":"secret-sentinel","state":"started"}),
            serde_json::json!({"stage":"tool","event":"command","tool":"secret-sentinel"}),
            serde_json::json!({"stage":"acquisition","event":"progress","resources_started":3,"resources_completed":2,"bytes_received":1024,"url":"secret-sentinel"}),
        ] {
            progress.emit(value.as_object().unwrap());
        }
        progress.finish();
        let before = bytes.lock().unwrap().clone();
        progress.mark("publication");
        assert_eq!(*bytes.lock().unwrap(), before);
        let text = String::from_utf8(before).unwrap();
        assert!(!text.contains("sentinel") && !text.contains("argv") && !text.contains("stdout"));
        let values = records(&bytes);
        let last = values.last().unwrap();
        assert_eq!(last["resources_started"], 3);
        assert_eq!(last["resources_completed"], 2);
        assert_eq!(last["bytes_received"], 1024);
        assert_eq!(last["tools_started"], 1);
        assert_eq!(last["tools_closed"], 1);
        assert_eq!(last["active_tools"], serde_json::json!([]));
    }

    #[test]
    fn a_result_write_failure_after_work_finishes_remains_structured() {
        let (progress, bytes, _) = observed(Duration::from_mins(1));
        progress.mark("publication");
        progress.finish();
        progress.diagnostic("analyze", "result-output-unavailable");
        drop(progress);
        let values = records(&bytes);
        assert_eq!(values.len(), 3);
        assert_eq!(values.last().unwrap()["event"], "failed");
        assert_eq!(
            values.last().unwrap()["error_code"],
            "result-output-unavailable"
        );
        assert_eq!(Arc::strong_count(&bytes), 1);
    }

    #[test]
    fn automatic_progress_covers_interactive_json_and_preserves_redirected_json() {
        assert_eq!(resolved(Mode::Auto, false, true), Mode::Text);
        assert_eq!(resolved(Mode::Auto, false, false), Mode::Off);
        for terminal in [false, true] {
            assert_eq!(resolved(Mode::Auto, true, terminal), Mode::Text);
            for mode in [Mode::Text, Mode::Json, Mode::Off] {
                assert_eq!(resolved(mode, false, terminal), mode);
                assert_eq!(resolved(mode, true, terminal), mode);
            }
        }
    }

    #[test]
    fn repeated_short_tools_do_not_flood_plain_progress() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (sent, _received) = mpsc::channel();
        let progress = Progress::with_writer(
            Mode::Text,
            "analyze",
            Box::new(Captured {
                bytes: bytes.clone(),
                flushed: sent,
            }),
            Duration::from_mins(1),
        );
        progress.mark("inspection");
        for _ in 0..100 {
            for event in ["command", "tool-output"] {
                progress.emit(
                    serde_json::json!({
                        "stage":"tool", "event":event, "tool":"ffprobe",
                        "argv":["secret-sentinel"], "stderr":"secret-sentinel"
                    })
                    .as_object()
                    .unwrap(),
                );
            }
        }
        progress.finish();
        let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(output.lines().count() <= 4, "{output}");
        assert!(!output.contains("scopes") && !output.contains("sentinel"));
        assert!(!output.contains('\u{1b}') && !output.contains('\r'));
    }

    #[test]
    fn analysis_progress_accepts_only_valid_monotonic_numeric_evidence() {
        let (progress, bytes, _) = observed(Duration::from_mins(1));
        progress.mark("input-preparation");
        for value in [
            serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"started","completed":0,"total":5}),
            serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"running","completed":3,"total":5,"url":"secret-sentinel"}),
            serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"running","completed":2,"total":5}),
            serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"finished","completed":4,"total":5}),
            serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"running","completed":4,"total":6}),
            serde_json::json!({"stage":"analysis","event":"progress","task":"secret-sentinel","state":"running","completed":4,"total":5}),
            serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"secret-sentinel","completed":4,"total":5}),
        ] {
            progress.emit(value.as_object().unwrap());
        }
        progress.finish();
        let values = records(&bytes);
        let last = values.last().unwrap();
        assert_eq!(last["analysis_task"], "ts-preparation");
        assert_eq!(last["analysis_state"], "running");
        assert_eq!(last["analysis_completed"], 3);
        assert_eq!(last["analysis_total"], 5);
        assert!(
            !String::from_utf8(bytes.lock().unwrap().clone())
                .unwrap()
                .contains("sentinel")
        );
    }

    #[test]
    fn finished_ts_collection_yields_to_the_remaining_source_checks() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (sent, _) = mpsc::channel();
        let progress = Progress::with_writer(
            Mode::Text,
            "analyze",
            Box::new(Captured {
                bytes: bytes.clone(),
                flushed: sent,
            }),
            Duration::from_mins(1),
        );
        for (state, completed) in [("started", 0), ("running", 5), ("finished", 5)] {
            progress.emit(
                serde_json::json!({
                    "stage":"analysis", "event":"progress", "task":"ts-preparation",
                    "state":state, "completed":completed, "total":5
                })
                .as_object()
                .unwrap(),
            );
        }
        progress.emit(
            serde_json::json!({
                "stage":"tool", "event":"command", "tool":"ffprobe"
            })
            .as_object()
            .unwrap(),
        );
        progress.finish();
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        let source = text.find("Preparing source").expect(&text);
        assert!(text[..source].contains("5/5 segments"), "{text}");
        assert!(!text[source..].contains("Preparing TS segments"), "{text}");
    }

    #[test]
    fn phase_history_retains_completion_received_inside_the_observer_interval() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (sent, _) = mpsc::channel();
        let progress = Progress::with_writer(
            Mode::Text,
            "analyze",
            Box::new(Captured {
                bytes: bytes.clone(),
                flushed: sent,
            }),
            Duration::from_mins(1),
        );
        for completed in [1, 6] {
            progress.emit(serde_json::json!({
                "stage":"acquisition", "event":"progress", "resources_started":6,
                "resources_completed":completed, "resources_total":6, "bytes_received":completed*100
            }).as_object().unwrap());
        }
        progress.mark("input-preparation");
        progress.finish();
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        assert!(text.contains("6/6 files"), "{text}");
    }

    #[test]
    fn acquisition_keeps_one_label_while_its_internal_preparation_scope_is_open() {
        let (progress, bytes, _) = observed(Duration::from_mins(1));
        progress.mark("acquisition");
        for state in ["started", "closed"] {
            progress.emit(serde_json::json!({
                "stage":"conversion", "event":"activity", "activity":"input-preparation", "state":state
            }).as_object().unwrap());
        }
        progress.finish();
        let values = records(&bytes);
        let last = values.last().unwrap();
        assert_eq!(last["activity"], "acquisition");
        assert_eq!(last["scopes_started"], 1);
        assert_eq!(last["scopes_closed"], 1);
    }
}
