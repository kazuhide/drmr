//! Sequential job execution, redacted logs and server-sent log events.

use crate::catalog::CatalogEntry;
use crate::pyutil;
use drmr_core::native::{self, ManagedChild};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::Duration;

/// A job's public state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobHandle {
    /// 32 hex digits (`uuid.uuid4().hex`).
    pub job_id: String,
    /// `running`, `succeeded`, `failed` or `interrupted`.
    pub state: &'static str,
    /// The CLI's exit code (`-N` when a signal ended it), `None` while running.
    pub exit_code: Option<i32>,
    /// Whether stopping this job was requested before confirmed completion.
    pub stop_requested: bool,
    /// `<jobs_dir>/<job_id>.jsonl`.
    pub log_path: PathBuf,
}

/// The job runner's errors.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// `JobAlreadyRunningError`: the lease is taken (the caller answers 409).
    #[error("a conversion job is already running")]
    AlreadyRunning,
    /// `JobNotFoundError`: no job with this id ran in this server run.
    #[error("{0}")]
    NotFound(String),
    /// The log file or the child could not be created (`OSError`).
    #[error("job launch failed")]
    Launch(#[source] std::io::Error),
    /// Process-tree cleanup failed; the execution lease remains held.
    #[error("cleanup-unconfirmed")]
    CleanupUnconfirmed,
    /// The server is closing and no longer accepts jobs.
    #[error("server-shutting-down")]
    ShuttingDown,
}

/// `_ensure_jsonl(line)`: a line that is valid JSON as it is, otherwise a synthetic
/// `{"stage": "raw", "level": "error", "message": <line>}` record, so that every line a client
/// receives parses.
pub fn ensure_jsonl(line: &str) -> String {
    if pyutil::loads_nonfinite_as_null(line).is_some() {
        return line.to_string();
    }
    let mut record = Map::new();
    record.insert("stage".into(), Value::from("raw"));
    record.insert("level".into(), Value::from("error"));
    record.insert("message".into(), Value::from(line));
    pyutil::dumps(&Value::Object(record))
}

/// A job id that may form a path (`[A-Za-z0-9][A-Za-z0-9_.-]{0,127}`).
pub fn safe_job_id(job_id: &str) -> bool {
    let bytes = job_id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
}

#[derive(Clone, Copy)]
struct Finished {
    code: i32,
    stop_requested: bool,
}

struct State {
    current_job_id: Option<String>,
    process: Option<Arc<Mutex<ManagedChild>>>,
    cleanup_worker: Option<std::thread::JoinHandle<std::io::Result<i32>>>,
    log_file: Option<File>,
    exit_code: Option<i32>,
    stop_requested: bool,
    closing: bool,
    cleanup_unconfirmed: bool,
    /// Outcomes of jobs that finished and were replaced as the current job.
    finished: HashMap<String, Finished>,
}

impl State {
    /// Observe without waiting for another thread's process-tree cleanup.
    fn refresh(&mut self) -> Result<(), JobError> {
        self.refresh_with(
            |child| observed_exit(&child.try_wait()),
            ManagedChild::finish,
        )
    }

    fn refresh_with(
        &mut self,
        observe: impl FnOnce(&mut ManagedChild) -> Option<i32>,
        finish: impl FnOnce(&mut ManagedChild) -> std::io::Result<()> + Send + 'static,
    ) -> Result<(), JobError> {
        self.poll_cleanup()?;
        if self.exit_code.is_some() || self.cleanup_worker.is_some() {
            return Ok(());
        }
        let Some(process) = self.process.clone() else {
            return Ok(());
        };
        let observed = match process.try_lock() {
            Ok(mut child) => observe(&mut child),
            Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Poisoned(_)) => {
                self.cleanup_unconfirmed = true;
                return Err(JobError::CleanupUnconfirmed);
            }
        };
        if let Some(code) = observed {
            self.start_cleanup(Some(code), finish)?;
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<(), JobError> {
        self.stop_with(ManagedChild::finish)
    }

    fn stop_with(
        &mut self,
        finish: impl FnOnce(&mut ManagedChild) -> std::io::Result<()> + Send + 'static,
    ) -> Result<(), JobError> {
        self.poll_cleanup()?;
        if self.exit_code.is_none() && self.process.is_some() && self.cleanup_worker.is_none() {
            self.stop_requested = true;
            self.start_cleanup(None, finish)?;
        }
        Ok(())
    }

    fn start_cleanup(
        &mut self,
        observed: Option<i32>,
        finish: impl FnOnce(&mut ManagedChild) -> std::io::Result<()> + Send + 'static,
    ) -> Result<(), JobError> {
        let process = self
            .process
            .clone()
            .expect("cleanup retains its child owner");
        let worker = std::thread::Builder::new()
            .name("job-cleanup".into())
            .spawn(move || {
                let mut child = process
                    .lock()
                    .map_err(|_| std::io::Error::other("cleanup-unconfirmed"))?;
                finish(&mut child)?;
                Ok(observed.unwrap_or_else(|| observed_exit(&child.try_wait()).unwrap_or(1)))
            });
        let Ok(worker) = worker else {
            self.cleanup_unconfirmed = true;
            return Err(JobError::CleanupUnconfirmed);
        };
        self.cleanup_worker = Some(worker);
        Ok(())
    }

    fn poll_cleanup(&mut self) -> Result<(), JobError> {
        self.check_cleanup()?;
        if self
            .cleanup_worker
            .as_ref()
            .is_some_and(std::thread::JoinHandle::is_finished)
        {
            let worker = self.cleanup_worker.take().expect("finished cleanup worker");
            let Ok(Ok(code)) = worker.join() else {
                self.cleanup_unconfirmed = true;
                return Err(JobError::CleanupUnconfirmed);
            };
            self.process = None;
            self.exit_code = Some(code);
            self.log_file = None;
        }
        Ok(())
    }

    fn check_cleanup(&self) -> Result<(), JobError> {
        if self.cleanup_unconfirmed {
            Err(JobError::CleanupUnconfirmed)
        } else {
            Ok(())
        }
    }

    fn busy(&self) -> bool {
        self.process.is_some() && self.exit_code.is_none()
    }

    fn handle(&self, job_id: &str, jobs_dir: &Path) -> Result<JobHandle, JobError> {
        let (code, stopped) = if self.current_job_id.as_deref() == Some(job_id) {
            self.check_cleanup()?;
            (self.exit_code, self.stop_requested)
        } else if let Some(finished) = self.finished.get(job_id) {
            (Some(finished.code), finished.stop_requested)
        } else {
            return Err(JobError::NotFound(job_id.to_string()));
        };
        Ok(JobHandle {
            job_id: job_id.to_string(),
            state: match (code, stopped) {
                (None, _) => "running",
                (Some(_), true) => "interrupted",
                (Some(0), false) => "succeeded",
                (Some(_), false) => "failed",
            },
            exit_code: code,
            stop_requested: stopped,
            log_path: jobs_dir.join(format!("{job_id}.jsonl")),
        })
    }
}

fn observed_exit(result: &std::io::Result<Option<std::process::ExitStatus>>) -> Option<i32> {
    match result {
        Ok(Some(status)) => Some(exit_code(*status)),
        Ok(None) => None,
        Err(_) => Some(1),
    }
}

#[cfg(unix)]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
        .unwrap_or(0)
}

#[cfg(not(unix))]
fn exit_code(status: std::process::ExitStatus) -> i32 {
    status.code().unwrap_or(0)
}

/// `ConversionJobManager`: single concurrency across every asset and mode; the check, the spawn
/// and the assignment are one critical section, so two overlapping submissions spawn once.
pub struct ConversionJobManager {
    jobs_dir: PathBuf,
    cli_argv: Vec<OsString>,
    base_env: EnvSource,
    state: Mutex<State>,
    readiness: crate::readiness::CatalogReadiness,
    // Process cleanup and open log handles drop before the directory pin.
    #[cfg(windows)]
    _jobs_pin: native::PrivateDirectoryGuard,
}

/// The environment a child inherits (`os.environ`, read at each launch).
pub type EnvSource = std::sync::Arc<dyn Fn() -> Vec<(OsString, OsString)> + Send + Sync>;

/// `os.environ` of this process.
pub fn process_env() -> EnvSource {
    std::sync::Arc::new(|| std::env::vars_os().collect())
}

impl std::fmt::Debug for ConversionJobManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversionJobManager")
            .field("jobs_dir", &self.jobs_dir)
            .field("cli_argv", &self.cli_argv)
            .finish_non_exhaustive()
    }
}

impl ConversionJobManager {
    /// A manager writing logs to `jobs_dir` (created) and running `cli_argv` + the command,
    /// its children inheriting `base_env` (this process's environment in production).
    pub fn new(
        jobs_dir: &Path,
        cli_argv: Vec<OsString>,
        base_env: EnvSource,
    ) -> std::io::Result<Self> {
        #[cfg(not(windows))]
        native::create_private_dir_all(jobs_dir)?;
        #[cfg(windows)]
        let jobs_pin = native::pin_private_directory(jobs_dir)?;
        Ok(ConversionJobManager {
            jobs_dir: jobs_dir.to_path_buf(),
            readiness: crate::readiness::CatalogReadiness::default(),
            cli_argv,
            base_env,
            state: Mutex::new(State {
                current_job_id: None,
                process: None,
                cleanup_worker: None,
                stop_requested: false,
                closing: false,
                log_file: None,
                exit_code: None,
                cleanup_unconfirmed: false,
                finished: HashMap::new(),
            }),
            #[cfg(windows)]
            _jobs_pin: jobs_pin,
        })
    }

    /// The directory holding the job logs.
    pub fn jobs_dir(&self) -> &Path {
        &self.jobs_dir
    }

    /// The inherited environment without `DRM_KID` / `DRM_KEY` / `DRM_IV` (`_fresh_env()`).
    pub fn fresh_env(&self) -> Vec<(OsString, OsString)> {
        (self.base_env)()
            .into_iter()
            .filter(|(name, _)| {
                !is_drm_var(name) && name != drmr_core::catalog_receipt::REQUEST_ENV
            })
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `submit(entry=…, output_root=…)`: a catalog conversion with the full-guarantee argv
    /// (never `--no-round-trip`, `--limit-renditions` or `--no-manifests`) and the entry's own
    /// key material in a fresh environment.
    pub fn submit(&self, entry: &CatalogEntry, output_root: &Path) -> Result<JobHandle, JobError> {
        let mut state = self.lock();
        state.refresh()?;
        if state.closing {
            return Err(JobError::ShuttingDown);
        }
        if state.busy() {
            return Err(JobError::AlreadyRunning);
        }
        let mut argv: Vec<OsString> = self.cli_argv.clone();
        let text = |value: &str| OsString::from(value);
        argv.extend([
            text("run"),
            text("--mpd"),
            entry.mpd_path.clone().into_os_string(),
            text("--hls-master"),
            entry.hls_master_path.clone().into_os_string(),
            text("--output"),
            output_root.as_os_str().to_os_string(),
            text("--asset-id"),
            text(&entry.asset_id),
            text("--log-format"),
            text("jsonl"),
            // The trailing slash is load-bearing: it is the DASH <BaseURL> every relative
            // SegmentTemplate path resolves against (RFC 3986 §5.2.3 drops a last segment
            // without it, and every segment 404s).
            text("--manifest-local-url-base"),
            text(&format!("/output/{}/cbcs/", entry.asset_id)),
        ]);
        // Keys and IVs only through this per-job environment, from this entry, never cached
        // and never written to the server's own environment (an ambient DRM_* is replaced).
        let mut env = self.fresh_env();
        env.push(("DRM_KID".into(), entry.key_material.key_id_hex().into()));
        env.push(("DRM_KEY".into(), entry.key_material.key_hex().into()));
        env.push(("DRM_IV".into(), entry.key_material.iv_hex().into()));
        self.launch(
            &mut state,
            &argv,
            &env,
            Some((&entry.asset_id, &output_root.join(&entry.asset_id))),
        )
    }

    /// `submit_command(arguments, env=…)`: another CLI command under the same lease, with the
    /// caller's fresh environment (private-config paths and ids only on argv).
    pub fn submit_command(
        &self,
        arguments: &[OsString],
        env: &[(OsString, OsString)],
    ) -> Result<JobHandle, JobError> {
        let mut state = self.lock();
        state.refresh()?;
        if state.closing {
            return Err(JobError::ShuttingDown);
        }
        if state.busy() {
            return Err(JobError::AlreadyRunning);
        }
        let mut argv = self.cli_argv.clone();
        argv.extend(arguments.iter().cloned());
        self.launch(&mut state, &argv, env, None)
    }

    fn launch(
        &self,
        state: &mut State,
        argv: &[OsString],
        env: &[(OsString, OsString)],
        catalog: Option<(&str, &Path)>,
    ) -> Result<JobHandle, JobError> {
        let job_id = pyutil::uuid4_hex();
        let log_path = self.jobs_dir.join(format!("{job_id}.jsonl"));
        let log_file = native::create_private_new(&log_path).map_err(JobError::Launch)?;
        let stdout = log_file.try_clone().map_err(JobError::Launch)?;
        let stderr = log_file.try_clone().map_err(JobError::Launch)?;
        let program = argv.first().cloned().unwrap_or_default();
        let mut command = Command::new(&program);
        command
            .args(&argv[1..])
            .env_clear()
            .envs(env.iter().map(|(name, value)| (name, value)))
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        command.env_remove(drmr_core::catalog_receipt::REQUEST_ENV);
        if let Some((asset, root)) = catalog {
            let request = self
                .readiness
                .begin(&self.jobs_dir, &job_id, asset, root)
                .map_err(JobError::Launch)?;
            command.env(drmr_core::catalog_receipt::REQUEST_ENV, request);
        }
        let child = ManagedChild::spawn(command, true).map_err(JobError::Launch)?;
        if let (Some(current), Some(code)) = (state.current_job_id.take(), state.exit_code) {
            state.finished.insert(
                current,
                Finished {
                    code,
                    stop_requested: state.stop_requested,
                },
            );
        }
        state.current_job_id = Some(job_id.clone());
        state.process = Some(Arc::new(Mutex::new(child)));
        state.log_file = Some(log_file);
        state.exit_code = None;
        state.stop_requested = false;
        Ok(JobHandle {
            job_id,
            state: "running",
            exit_code: None,
            stop_requested: false,
            log_path,
        })
    }

    /// `is_busy()`: the lease is taken.
    pub fn is_busy(&self) -> bool {
        let mut state = self.lock();
        let _ = state.refresh();
        state.busy()
    }

    /// `status(job_id)`: the current job (refreshed) or a finished one replaced by a newer
    /// job; `NotFound` only for an id never submitted in this server run.
    pub fn status(&self, job_id: &str) -> Result<JobHandle, JobError> {
        let mut state = self.lock();
        if state.current_job_id.as_deref() == Some(job_id) {
            state.refresh()?;
        }
        state.handle(job_id, &self.jobs_dir)
    }

    /// Request one identified job's stop. Cleanup runs outside the manager lock;
    /// repeated calls retain the same intent, and a replaced job cannot stop its successor.
    pub fn request_stop(&self, job_id: &str) -> Result<JobHandle, JobError> {
        let mut state = self.lock();
        if state.current_job_id.as_deref() == Some(job_id) {
            state.refresh()?;
            state.stop()?;
        }
        state.handle(job_id, &self.jobs_dir)
    }

    /// Close the submission gate before an embedding host begins shutdown.
    pub fn begin_shutdown(&self) {
        self.lock().closing = true;
        self.cancel_readiness();
    }

    /// Stop and confirm the owned tree, retaining ownership on any error.
    /// The caller may block, but status and repeated stop requests remain responsive.
    pub fn shutdown(&self) -> Result<(), JobError> {
        let job_id = {
            let mut state = self.lock();
            state.closing = true;
            state.current_job_id.clone()
        };
        if let Some(job_id) = job_id {
            self.request_stop(&job_id)?;
            self.wait_for_cleanup(&job_id)?;
        }
        Ok(())
    }

    fn wait_for_cleanup(&self, job_id: &str) -> Result<(), JobError> {
        loop {
            if self.status(job_id)?.state != "running" {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Cancel bounded readiness IO before owned server shutdown drains requests.
    pub fn cancel_readiness(&self) {
        self.readiness.cancel();
    }

    /// Validate a completed catalog generation outside manager/service locks.
    pub fn qualify_catalog(&self, job_id: &str) {
        let code = self.status(job_id).ok().and_then(|h| {
            h.exit_code
                .map(|code| if h.state == "succeeded" { code } else { 1 })
        });
        if let Some(code) = code {
            self.readiness.finish(&self.jobs_dir, job_id, code);
        }
    }

    /// Whether a catalog attempt has fenced this asset, including an unreadable fence.
    pub fn catalog_fenced(&self, asset_id: &str) -> bool {
        crate::readiness::CatalogReadiness::fenced(&self.jobs_dir, asset_id)
    }

    /// Additive catalog output authority, independent of the legacy existence flag.
    pub fn output_readiness(&self, asset_id: &str, output_root: &Path) -> Value {
        self.readiness
            .public(&self.jobs_dir, asset_id, &output_root.join(asset_id))
    }

    /// A completed generation whose current bytes and filesystem bindings still match.
    pub fn catalog_output(
        &self,
        asset_id: &str,
        output_root: &Path,
    ) -> Option<std::sync::Arc<drmr_core::catalog_receipt::Verified>> {
        self.readiness
            .output(&self.jobs_dir, asset_id, &output_root.join(asset_id))
    }

    /// `_job_has_exited(job_id)`: a job replaced as the current one has necessarily exited.
    fn job_has_exited(&self, job_id: &str) -> Result<bool, TailError> {
        let mut state = self.lock();
        if state.current_job_id.as_deref() != Some(job_id) {
            return Ok(true);
        }
        state.refresh().map_err(|_| TailError::CleanupUnconfirmed)?;
        Ok(state.exit_code.is_some())
    }

    /// Stop the current child (used by the tests to clean up a job that never ends).
    pub fn kill_current(&self) {
        let job_id = self.lock().current_job_id.clone();
        if let Some(job_id) = job_id {
            let _ = self
                .request_stop(&job_id)
                .and_then(|_| self.wait_for_cleanup(&job_id));
        }
    }

    /// `read_log(job_id)`: every record of a persisted log, without liveness, for a job this
    /// process may never have run, streamed line by line (a feature-length title's log runs to
    /// hundreds of megabytes). An unknown or unsafe id, or a symlink (`O_NOFOLLOW`), yields
    /// nothing. Lines are decoded with replacement and universal newlines, blank lines skipped,
    /// each through [`ensure_jsonl`].
    pub fn read_log(&self, job_id: &str) -> LogLines {
        let file = safe_job_id(job_id)
            .then(|| open_nofollow(&self.jobs_dir.join(format!("{job_id}.jsonl"))).ok())
            .flatten();
        LogLines {
            reader: file.map(std::io::BufReader::new),
        }
    }

    /// `tail_log(job_id)`: the records as they are written until the job has exited and every
    /// line written before that has been read (the SSE `done` event relies on it).
    pub fn tail_log(&self, job_id: &str, poll_interval: Duration) -> Result<TailLog<'_>, JobError> {
        let handle = self.status(job_id)?;
        let job_id = job_id.to_string();
        Ok(TailLog::checked(
            handle.log_path,
            poll_interval,
            Box::new(move || self.job_has_exited(&job_id)),
        ))
    }
}

fn is_drm_var(name: &OsStr) -> bool {
    name.to_str().is_some_and(|name| {
        ["DRM_KID", "DRM_KEY", "DRM_IV"].iter().any(|secret| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case(secret)
            } else {
                name == *secret
            }
        })
    })
}

fn open_nofollow(path: &Path) -> std::io::Result<File> {
    native::open_private_shared_read(path)
}

/// The lines of a persisted log ([`ConversionJobManager::read_log`]): `\r\n`, `\r` and `\n` end a
/// line (universal newlines; a blank line, which a split `\r\n` would make, is skipped anyway),
/// invalid UTF-8 replaced, each line through [`ensure_jsonl`].
pub struct LogLines {
    reader: Option<std::io::BufReader<File>>,
}

impl Iterator for LogLines {
    type Item = String;

    fn next(&mut self) -> Option<String> {
        use std::io::BufRead;
        let reader = self.reader.as_mut()?;
        loop {
            let mut line = Vec::new();
            loop {
                let Ok(available) = reader.fill_buf() else {
                    self.reader = None;
                    return None;
                };
                if available.is_empty() {
                    break;
                }
                if let Some(index) = available.iter().position(|b| matches!(b, b'\n' | b'\r')) {
                    line.extend_from_slice(&available[..index]);
                    reader.consume(index + 1);
                    break;
                }
                let length = available.len();
                line.extend_from_slice(available);
                reader.consume(length);
            }
            if line.is_empty() {
                // End of file, or a blank line (skipped).
                if reader.fill_buf().map_or(true, <[u8]>::is_empty) {
                    self.reader = None;
                    return None;
                }
                continue;
            }
            return Some(ensure_jsonl(&String::from_utf8_lossy(&line)));
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TailError {
    /// The log file could not be opened or read.
    #[error("job log unreadable")]
    Io(#[from] std::io::Error),
    /// Strict UTF-8 decoding failed (`UnicodeDecodeError`).
    #[error("job log is not UTF-8")]
    Decode,
    /// The writer's process-tree cleanup could not be confirmed.
    #[error("cleanup-unconfirmed")]
    CleanupUnconfirmed,
}

/// The iterator `tail_log` returns: blocking, polling the file every `poll_interval`.
pub struct TailLog<'a> {
    /// `_job_has_exited(job_id)`: asked only after a read found nothing new.
    has_exited: Box<dyn FnMut() -> Result<bool, TailError> + Send + 'a>,
    log_path: PathBuf,
    poll_interval: Duration,
    file: Option<File>,
    /// Bytes of an incomplete UTF-8 sequence at the end of the last read.
    pending: Vec<u8>,
    /// Text after the last line break.
    buffer: String,
    ready: std::collections::VecDeque<String>,
    exited: bool,
    done: bool,
}

impl<'a> TailLog<'a> {
    /// A tail of `log_path` that stops once `has_exited` says the writer is gone and a last
    /// read has drained what it wrote before that.
    pub fn new(
        log_path: PathBuf,
        poll_interval: Duration,
        has_exited: Box<dyn FnMut() -> bool + Send + 'a>,
    ) -> Self {
        let mut has_exited = has_exited;
        Self::checked(log_path, poll_interval, Box::new(move || Ok(has_exited())))
    }

    fn checked(
        log_path: PathBuf,
        poll_interval: Duration,
        has_exited: Box<dyn FnMut() -> Result<bool, TailError> + Send + 'a>,
    ) -> Self {
        TailLog {
            has_exited,
            log_path,
            poll_interval,
            file: None,
            pending: Vec::new(),
            buffer: String::new(),
            ready: std::collections::VecDeque::new(),
            exited: false,
            done: false,
        }
    }
}

impl TailLog<'_> {
    /// Read whatever the file has now; `true` when something arrived.
    fn read_chunk(&mut self) -> Result<bool, TailError> {
        let file = match self.file.as_mut() {
            Some(file) => file,
            None => self.file.insert(open_nofollow(&self.log_path)?),
        };
        let mut bytes = std::mem::take(&mut self.pending);
        let before = bytes.len();
        file.read_to_end(&mut bytes)?;
        if bytes.len() == before {
            self.pending = bytes;
            return Ok(false);
        }
        let text = match std::str::from_utf8(&bytes) {
            Ok(text) => text.to_string(),
            Err(error) if error.error_len().is_none() => {
                // An incomplete sequence at the end: keep it for the next read.
                let valid = error.valid_up_to();
                self.pending = bytes[valid..].to_vec();
                String::from_utf8(bytes[..valid].to_vec()).map_err(|_| TailError::Decode)?
            }
            Err(_) => return Err(TailError::Decode),
        };
        self.buffer
            .push_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
        let mut parts: Vec<String> = self.buffer.split('\n').map(str::to_string).collect();
        self.buffer = parts.pop().unwrap_or_default();
        for line in parts {
            if !line.is_empty() {
                self.ready.push_back(ensure_jsonl(&line));
            }
        }
        Ok(true)
    }
}

impl Iterator for TailLog<'_> {
    type Item = Result<String, TailError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(line) = self.ready.pop_front() {
                return Some(Ok(line));
            }
            if self.done {
                return None;
            }
            match self.read_chunk() {
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
                Ok(true) => continue,
                Ok(false) => {}
            }
            if self.exited {
                self.done = true;
                if !self.pending.is_empty() {
                    return Some(Err(TailError::Decode));
                }
                let rest = std::mem::take(&mut self.buffer);
                if !rest.is_empty() {
                    // A trailing unterminated fragment still goes through `ensure_jsonl`.
                    return Some(Ok(ensure_jsonl(&rest)));
                }
                return None;
            }
            match (self.has_exited)() {
                Ok(true) => {
                    // Drain the final writes only after ownership cleanup succeeded.
                    self.exited = true;
                    continue;
                }
                Ok(false) => {}
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
            std::thread::sleep(self.poll_interval);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn state_with_child(root: &Path) -> State {
        let executable = drmr_test_support::install(
            &root.join("child"),
            &drmr_test_support::ToolSpec {
                sleep_ms: 60_000,
                ..Default::default()
            },
        )
        .unwrap();
        State {
            current_job_id: Some("job".into()),
            process: Some(Arc::new(Mutex::new(
                ManagedChild::spawn(Command::new(executable), true).unwrap(),
            ))),
            cleanup_worker: None,
            stop_requested: false,
            closing: false,
            log_file: Some(File::create(root.join("job.jsonl")).unwrap()),
            exit_code: None,
            cleanup_unconfirmed: false,
            finished: HashMap::new(),
        }
    }

    fn settle(state: &mut State) -> Result<(), JobError> {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while state
            .cleanup_worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            assert!(
                std::time::Instant::now() < deadline,
                "cleanup worker failed to settle"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        state.poll_cleanup()
    }

    #[test]
    fn stop_keeps_the_lease_and_status_responsive_until_cleanup_is_confirmed() {
        let root = tempfile::tempdir().unwrap();
        let state = state_with_child(root.path());
        let manager =
            ConversionJobManager::new(&root.path().join("jobs"), Vec::new(), Arc::new(Vec::new))
                .unwrap();
        *manager.lock() = state;
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        manager
            .lock()
            .stop_with(move |child| {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                child.finish()
            })
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // The worker holds its child lock while the manager continues answering calls.
        assert!(manager.is_busy());
        let running = manager.request_stop("job").unwrap();
        assert_eq!(running.state, "running");
        assert!(running.stop_requested);
        assert_eq!(running.exit_code, None);
        assert!(matches!(
            manager.submit_command(&[], &[]),
            Err(JobError::AlreadyRunning)
        ));
        assert!(matches!(
            manager.request_stop("unknown"),
            Err(JobError::NotFound(_))
        ));
        release_tx.send(()).unwrap();
        manager.wait_for_cleanup("job").unwrap();
        let ended = manager.status("job").unwrap();
        assert_eq!(ended.state, "interrupted");
        assert!(ended.stop_requested);
        assert!(ended.exit_code.is_some());
        assert!(!manager.is_busy());
    }

    #[test]
    fn shutdown_keeps_http_observation_available_while_native_cleanup_is_pending() {
        let root = tempfile::tempdir().unwrap();
        let config =
            crate::ServerConfig::new(root.path().join("catalog"), root.path().join("output"));
        let mut server = crate::spawn(crate::ServerConfig {
            bind_port: 0,
            ..config
        })
        .unwrap();
        let app = server.app.clone();
        let url = server.url();
        let mut state = state_with_child(root.path());
        let log = app.manager.jobs_dir().join("job.jsonl");
        let mut file = native::create_private_new(&log).unwrap();
        file.write_all(b"{\"stage\":\"test\",\"message\":\"before cleanup\"}\n")
            .unwrap();
        state.log_file = Some(file);
        *app.manager.lock() = state;
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        app.manager
            .lock()
            .stop_with(move |child| {
                entered_tx.send(()).unwrap();
                let _ = release_rx.recv();
                child.finish()
            })
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let shutdown = std::thread::spawn(move || {
            let result = server.shutdown();
            (server, result)
        });
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !app.manager.lock().closing {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(5)))
            .build()
            .into();
        let mut status = agent.get(&format!("{url}/api/jobs/job")).call().unwrap();
        assert_eq!(status.status().as_u16(), 200);
        let payload: Value =
            serde_json::from_str(&status.body_mut().read_to_string().unwrap()).unwrap();
        assert_eq!(payload["state"], "running");
        assert_eq!(payload["stop_requested"], true);
        assert_eq!(
            agent
                .post(&format!("{url}/api/jobs/job/stop"))
                .header("Content-Type", "application/json")
                .send("{}")
                .unwrap()
                .status()
                .as_u16(),
            202
        );
        assert_eq!(
            agent
                .get(&format!("{url}/api/jobs/job/log"))
                .call()
                .unwrap()
                .status()
                .as_u16(),
            200
        );
        assert_eq!(
            agent
                .post(&format!("{url}/api/convert"))
                .header("Content-Type", "application/json")
                .send("{}")
                .unwrap()
                .status()
                .as_u16(),
            503
        );
        release_tx.send(()).unwrap();
        let (_owner, result) = shutdown.join().unwrap();
        result.unwrap();
    }

    #[test]
    fn a_stop_intent_never_promotes_a_zero_exit_to_success() {
        let root = tempfile::tempdir().unwrap();
        let mut state = state_with_child(root.path());
        state.stop_requested = true;
        state
            .refresh_with(|_| Some(0), ManagedChild::finish)
            .unwrap();
        settle(&mut state).unwrap();
        let handle = state.handle("job", root.path()).unwrap();
        assert_eq!(handle.state, "interrupted");
        assert_eq!(handle.exit_code, Some(0));
        assert!(handle.stop_requested);
    }

    #[test]
    fn an_exit_requires_confirmed_cleanup_before_releasing_the_lease() {
        for code in [0, 7] {
            let root = tempfile::tempdir().unwrap();
            let mut state = state_with_child(root.path());
            state
                .refresh_with(
                    |_| Some(code),
                    |_| Err(std::io::Error::other("sensitive diagnostic")),
                )
                .unwrap();
            let result = settle(&mut state);
            assert!(matches!(result, Err(JobError::CleanupUnconfirmed)));
            assert_eq!(result.unwrap_err().to_string(), "cleanup-unconfirmed");
            assert!(state.busy());
            assert!(state.process.is_some());
            assert!(state.log_file.is_some());
            assert_eq!(state.exit_code, None);
            assert!(state.finished.is_empty());
            assert!(matches!(
                state.refresh_with(
                    |_| panic!("must not poll again"),
                    |_| panic!("must not retry")
                ),
                Err(JobError::CleanupUnconfirmed)
            ));
            assert!(matches!(
                state.stop_with(|_| panic!("must not retry cleanup")),
                Err(JobError::CleanupUnconfirmed)
            ));
            let manager = ConversionJobManager::new(
                &root.path().join("jobs"),
                Vec::new(),
                std::sync::Arc::new(Vec::new),
            )
            .unwrap();
            *manager.lock() = state;
            assert!(manager.is_busy());
            assert!(matches!(
                manager.status("job"),
                Err(JobError::CleanupUnconfirmed)
            ));
            assert!(matches!(
                manager.submit_command(&[], &[]),
                Err(JobError::CleanupUnconfirmed)
            ));
            assert!(matches!(
                manager.tail_log("job", Duration::ZERO),
                Err(JobError::CleanupUnconfirmed)
            ));
        }
    }

    #[test]
    fn a_requested_stop_retains_ownership_when_cleanup_fails() {
        let root = tempfile::tempdir().unwrap();
        let mut state = state_with_child(root.path());
        state
            .stop_with(|_| Err(std::io::Error::other("private failure")))
            .unwrap();
        assert!(matches!(
            settle(&mut state),
            Err(JobError::CleanupUnconfirmed)
        ));
        assert!(state.busy());
        assert!(state.process.is_some());
        assert!(state.log_file.is_some());
        assert_eq!(state.exit_code, None);
    }

    #[test]
    fn confirmed_cleanup_preserves_the_observed_exit_code() {
        for code in [0, 7] {
            let root = tempfile::tempdir().unwrap();
            let mut state = state_with_child(root.path());
            state
                .refresh_with(|_| Some(code), ManagedChild::finish)
                .unwrap();
            settle(&mut state).unwrap();
            assert!(!state.busy());
            assert!(state.process.is_none());
            assert!(state.log_file.is_none());
            assert_eq!(state.exit_code, Some(code));
        }
    }

    #[test]
    fn a_tail_reports_unconfirmed_cleanup_without_a_done_transition() {
        let root = tempfile::tempdir().unwrap();
        let private = root.path().join("private");
        native::create_private_dir_all(&private).unwrap();
        let log = private.join("job.jsonl");
        native::create_private_new(&log).unwrap();
        let mut tail = TailLog::checked(
            log,
            Duration::ZERO,
            Box::new(|| Err(TailError::CleanupUnconfirmed)),
        );
        let error = tail.next().unwrap().unwrap_err();
        assert!(matches!(error, TailError::CleanupUnconfirmed));
        assert_eq!(error.to_string(), "cleanup-unconfirmed");
        assert!(!tail.exited);
        assert!(tail.next().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn a_standalone_manager_pins_its_log_directory_and_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        std::fs::create_dir(&parent).unwrap();
        let jobs = parent.join("jobs");
        let manager =
            ConversionJobManager::new(&jobs, Vec::new(), std::sync::Arc::new(Vec::new)).unwrap();
        assert!(std::fs::rename(&jobs, parent.join("redirected-jobs")).is_err());
        assert!(std::fs::rename(&parent, root.path().join("redirected-parent")).is_err());
        drop(manager);
        std::fs::rename(&parent, root.path().join("released-parent")).unwrap();
    }

    #[test]
    fn an_unobservable_child_exit_is_failed_instead_of_successful() {
        assert_eq!(
            observed_exit(&Err(std::io::Error::other("lost child status"))),
            Some(1)
        );
        assert_eq!(observed_exit(&Ok(None)), None);
    }

    #[test]
    fn non_json_lines_are_wrapped_as_raw_records() {
        assert_eq!(ensure_jsonl("{\"a\": 1}"), "{\"a\": 1}");
        assert_eq!(
            ensure_jsonl("error: bad \"arg\""),
            "{\"stage\": \"raw\", \"level\": \"error\", \"message\": \"error: bad \\\"arg\\\"\"}"
        );
    }

    #[test]
    fn persisted_lines_use_universal_newlines_and_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        let manager =
            ConversionJobManager::new(&private, vec![OsString::from("true")], process_env())
                .unwrap();
        native::create_private_new(&private.join("j1.jsonl"))
            .unwrap()
            .write_all(b"{\"a\": 1}\r\n\"x\"\r\n\nnot \xff json")
            .unwrap();
        let lines: Vec<String> = manager.read_log("j1").collect();
        assert_eq!(
            lines,
            [
                "{\"a\": 1}".to_string(),
                "\"x\"".to_string(),
                ensure_jsonl("not \u{fffd} json"),
            ]
        );
    }

    /// `test_tail_log_drains_what_was_written_just_before_the_exit_was_observed`: the writer
    /// appends its last line and exits between the tail's empty read and its exit check; the
    /// last read after the exit still yields it.
    #[test]
    fn the_tail_drains_what_was_written_just_before_the_exit_was_observed() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        native::create_private_dir_all(&private).unwrap();
        let path = private.join("job.jsonl");
        native::create_private_new(&path)
            .unwrap()
            .write_all(b"{\"stage\": \"fake\", \"message\": \"first\"}\n")
            .unwrap();
        let writer = path.clone();
        let tail = TailLog::new(
            path,
            Duration::from_millis(1),
            Box::new(move || {
                let mut file = std::fs::OpenOptions::new()
                    .append(true)
                    .open(&writer)
                    .unwrap();
                writeln!(
                    file,
                    "{{\"stage\": \"report\", \"message\": \"final-summary\"}}"
                )
                .unwrap();
                true
            }),
        );
        let messages: Vec<String> = tail
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(&line.unwrap()).unwrap()["message"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(messages, ["first", "final-summary"]);
    }

    #[test]
    fn job_ids_are_path_components() {
        assert!(safe_job_id(&"a".repeat(128)) && !safe_job_id(&"a".repeat(129)));
        assert!(!safe_job_id("../x") && !safe_job_id("") && !safe_job_id(".x"));
    }
}
