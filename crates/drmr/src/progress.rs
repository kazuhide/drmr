//! Bounded command observations built from fixed labels and numeric counters only.
use drmr_core::events::{Record, Sink};
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::{IsTerminal as _, Write};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod display;
mod pipeline;

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

#[derive(Default)]
enum Rendering {
    #[default]
    Live,
    Result,
}

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
    display_activity: Option<&'static str>,
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
    activities: [u64; 6],
    downloads: Option<[u64; 3]>,
    resources_total: Option<u64>,
    resources_began: Option<(Duration, u64)>,
    analysis: Option<Analysis>,
    work_completed: u64,
    pipeline: pipeline::State,
    source_retained: bool,
    rendering: Rendering,
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
const ACTIVITIES: [(&str, &str); 6] = [
    ("input-preparation", "Preparing media"),
    ("inspection", "Reading metadata"),
    ("normalization", "Preparing media for conversion"),
    ("packaging", "Writing media and encryption signaling"),
    ("verification", "Checking media"),
    ("publication", "Finalizing media"),
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
                    ProgressStyle::with_template("{msg}")
                        .unwrap_or_else(|_| ProgressStyle::default_spinner()),
                );
                Some(bar)
            }
        } else {
            None
        };
        let interval = Duration::from_secs(if bar.is_some() { 1 } else { 2 });
        Self::with_terminal(mode, command, Box::new(std::io::stderr()), interval, bar)
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
        if value != "input-preparation" {
            state.analysis = None;
        }
        state.last_activity = Some(value);
        state.display_activity = Some(value);
        self.inner.line(&mut state, "activity", None, changed);
    }

    pub(crate) fn source_retained(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.finished || state.source_retained || matches!(state.rendering, Rendering::Result) {
            return;
        }
        state.source_retained = true;
        if matches!(self.inner.mode, Mode::Text)
            && self.inner.terminal.is_none()
            && !state.output_failed
        {
            let mut out = self
                .inner
                .out
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if display::notice(&mut *out, "Downloaded source retained for offline use.")
                .and_then(|()| out.flush())
                .is_err()
            {
                state.output_failed = true;
            }
        }
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

    pub(crate) fn server_handoff(&self) {
        // Startup diagnostics and ongoing request logs own the terminal afterward.
        if self.inner.terminal.is_some() {
            self.begin_result();
        }
    }

    pub(crate) fn server_address(&self, address: std::net::SocketAddr) {
        if self.structured() {
            return;
        }
        let message = format!(" * Running on http://{address}");
        let mut out = self
            .inner
            .out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(out, "{message}").and_then(|()| out.flush());
    }

    /// Drain live drawing before a result changes the terminal cursor.
    /// Command success or failure remains pending until the result is written.
    pub(crate) fn begin_result(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.rendering = Rendering::Result;
        if let Some(bar) = &self.inner.terminal {
            bar.disable_steady_tick();
            bar.finish_and_clear();
            bar.set_draw_target(ProgressDrawTarget::hidden());
        }
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
        self.close_worker();
    }

    fn close_worker(&self) {
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
        {
            let mut state = self
                .inner
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.finished = true;
            self.inner.wake.notify_all();
        }
        self.close_worker();
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
        if matches!(event, "failed" | "cancelled") {
            state.pipeline.stop_estimates();
        }
        if matches!(self.mode, Mode::Text)
            && matches!(state.rendering, Rendering::Result)
            && !matches!(event, "finished" | "failed" | "cancelled")
        {
            return;
        }
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
        let mut out = self
            .out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = if matches!(self.mode, Mode::Json) {
            let value = self.progress_record(state, event, error, elapsed_ms);
            serde_json::to_writer(&mut *out, &value)
                .map_err(std::io::Error::other)
                .and_then(|()| writeln!(out))
        } else {
            let activity = state.display_activity.unwrap_or("configuration");
            let units = state.units(activity);
            let live_task = state.live_task();
            let view = display::View {
                activity,
                task: &live_task,
                downloads: state.downloads,
                units,
                pipeline: state.pipeline.phase.is_some().then_some(&state.pipeline),
            };
            state.display.render(
                &mut *out,
                self.terminal
                    .as_ref()
                    .filter(|_| matches!(state.rendering, Rendering::Live)),
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

    fn progress_record(
        &self,
        state: &State,
        event: &'static str,
        error: Option<&'static str>,
        elapsed_ms: u64,
    ) -> serde_json::Value {
        let active: Vec<_> = TOOLS
            .iter()
            .zip(state.tools)
            .filter_map(|(name, count)| (count > 0).then_some(*name))
            .collect();
        let mut value = serde_json::json!({
            "schema_version": 1, "entry_point": "cli", "command": self.command,
            "run_id": self.run_id, "event": event, "elapsed_ms": elapsed_ms,
            "activity": state.last_activity, "task": state.machine_task(),
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
        if state.pipeline.phase.is_some() {
            value["pipeline"] = state.pipeline.json(self.began.elapsed());
        }
        value
    }
}

impl State {
    fn clear_finished_analysis(&mut self) -> bool {
        let mut cleared = self.pipeline.clear_finished_analysis();
        if self
            .analysis
            .is_some_and(|analysis| analysis.state == "finished")
        {
            // Collection completion does not complete whole-track checks.
            self.analysis = None;
            cleared = true;
        }
        cleared
    }

    fn machine_task(&self) -> String {
        let tasks: Vec<_> = TASKS
            .iter()
            .zip(self.tasks)
            .filter_map(|((_, label), count)| (count > 0).then_some(*label))
            .collect();
        if tasks.is_empty() {
            description(self.last_activity.unwrap_or("")).to_string()
        } else {
            tasks.join("; ")
        }
    }

    fn live_task(&self) -> String {
        let labels: Vec<_> = TASKS
            .iter()
            .zip(self.tasks)
            .filter_map(|((_, label), count)| (count > 0).then_some(*label))
            .collect();
        if labels.is_empty() {
            ACTIVITIES
                .iter()
                .zip(self.activities)
                .filter_map(|((_, label), count)| (count > 0).then_some(*label))
                .collect::<Vec<_>>()
                .join("; ")
        } else {
            labels.join("; ")
        }
    }

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
        self.display_activity.get_or_insert("acquisition");
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
                if let Some(index) = ACTIVITIES.iter().position(|(name, _)| *name == value) {
                    self.activities[index] = self.activities[index].saturating_add(1);
                }
                self.display_activity.get_or_insert(value);
                if value != "input-preparation" {
                    self.analysis = None;
                }
            }
            Some("closed") => {
                self.closed = self.closed.saturating_add(1);
                if let Some(index) = ACTIVITIES.iter().position(|(name, _)| *name == value) {
                    self.activities[index] = self.activities[index].saturating_sub(1);
                }
            }
            _ => return None,
        }
        self.last_activity = Some(if self.display_activity == Some("acquisition") {
            "acquisition"
        } else {
            value
        });
        Some(())
    }

    fn observe_task(&mut self, record: &Record) -> Option<bool> {
        let index = record
            .get("task")
            .and_then(serde_json::Value::as_str)
            .and_then(|value| TASKS.iter().position(|(name, _)| *name == value))?;
        match record.get("state").and_then(serde_json::Value::as_str) {
            Some("started") => {
                let changed = self.tasks[index] == 0;
                self.tasks[index] = self.tasks[index].saturating_add(1);
                Some(changed)
            }
            Some("closed") => {
                self.tasks[index] = self.tasks[index].saturating_sub(1);
                Some(false)
            }
            _ => None,
        }
    }

    fn observe_analysis(&mut self, record: &Record, now: Duration) -> Option<bool> {
        let (analysis, changed) = parse_analysis(self.analysis, record, now)?;
        self.analysis = Some(analysis);
        self.last_activity = Some("input-preparation");
        self.display_activity.get_or_insert("input-preparation");
        Some(changed)
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

fn parse_analysis(
    previous: Option<Analysis>,
    record: &Record,
    now: Duration,
) -> Option<(Analysis, bool)> {
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
    } else {
        let prior = previous?;
        if prior.total != total || prior.completed > completed {
            return None;
        }
        prior.began
    };
    Some((
        Analysis {
            completed,
            total,
            began,
            state: status,
        },
        status != "running",
    ))
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
        let now = self.inner.began.elapsed();
        if record_stage == Some("pipeline") && event == Some("progress") {
            if state.pipeline.observe(record, now) {
                self.inner.line(&mut state, "progress", None, true);
            }
            return;
        }
        let scoped = pipeline::has_context(record);
        if scoped && !state.pipeline.valid_context(record) {
            return;
        }
        let nested = state.pipeline.observe_nested(record, now);
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
                if scoped {
                    if !nested {
                        return;
                    }
                    force = true;
                } else {
                    let Some(changed) = state.observe_analysis(record, now) else {
                        return;
                    };
                    force = changed;
                }
            }
            (Some("media-progress" | "rendition-progress"), Some("progress")) => {
                if !nested {
                    return;
                }
                force = record.get("state").and_then(serde_json::Value::as_str) != Some("running");
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
                let Some(changed) = state.observe_task(record) else {
                    return;
                };
                force = changed;
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
        if state.clear_finished_analysis() {
            self.inner.line(&mut state, "progress", None, true);
        }
    }
}

#[cfg(test)]
mod result_tests;
#[cfg(test)]
mod tests;
