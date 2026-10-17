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
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// A job's public state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobHandle {
    /// 32 hex digits (`uuid.uuid4().hex`).
    pub job_id: String,
    /// `running`, `succeeded` or `failed`.
    pub state: &'static str,
    /// The CLI's exit code (`-N` when a signal ended it), `None` while running.
    pub exit_code: Option<i32>,
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

struct State {
    current_job_id: Option<String>,
    process: Option<ManagedChild>,
    log_file: Option<File>,
    exit_code: Option<i32>,
    /// Exit codes of jobs that finished and were replaced as the current job.
    finished: HashMap<String, i32>,
}

impl State {
    /// `_refresh()`: observe the current child's exit (and close the parent's log handle).
    fn refresh(&mut self) {
        if self.exit_code.is_none()
            && let Some(child) = self.process.as_mut()
        {
            let code = observed_exit(&child.try_wait());
            if code.is_some() {
                // Finish ownership of descendants before publishing a terminal state.
                self.process = None;
                self.exit_code = code;
                self.log_file = None;
            }
        }
    }

    fn busy(&self) -> bool {
        self.process.is_some() && self.exit_code.is_none()
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
        native::create_private_dir_all(jobs_dir)?;
        Ok(ConversionJobManager {
            jobs_dir: jobs_dir.to_path_buf(),
            cli_argv,
            base_env,
            state: Mutex::new(State {
                current_job_id: None,
                process: None,
                log_file: None,
                exit_code: None,
                finished: HashMap::new(),
            }),
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
            .filter(|(name, _)| !is_drm_var(name))
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
        state.refresh();
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
        self.launch(&mut state, &argv, &env)
    }

    /// `submit_command(arguments, env=…)`: another CLI command under the same lease, with the
    /// caller's fresh environment (private-config paths and ids only on argv).
    pub fn submit_command(
        &self,
        arguments: &[OsString],
        env: &[(OsString, OsString)],
    ) -> Result<JobHandle, JobError> {
        let mut state = self.lock();
        state.refresh();
        if state.busy() {
            return Err(JobError::AlreadyRunning);
        }
        let mut argv = self.cli_argv.clone();
        argv.extend(arguments.iter().cloned());
        self.launch(&mut state, &argv, env)
    }

    fn launch(
        &self,
        state: &mut State,
        argv: &[OsString],
        env: &[(OsString, OsString)],
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
        let child = ManagedChild::spawn(command, true).map_err(JobError::Launch)?;
        if let (Some(current), Some(code)) = (state.current_job_id.take(), state.exit_code) {
            state.finished.insert(current, code);
        }
        state.current_job_id = Some(job_id.clone());
        state.process = Some(child);
        state.log_file = Some(log_file);
        state.exit_code = None;
        Ok(JobHandle {
            job_id,
            state: "running",
            exit_code: None,
            log_path,
        })
    }

    /// `is_busy()`: the lease is taken.
    pub fn is_busy(&self) -> bool {
        let mut state = self.lock();
        state.refresh();
        state.busy()
    }

    /// `status(job_id)`: the current job (refreshed) or a finished one replaced by a newer
    /// job; `NotFound` only for an id never submitted in this server run.
    pub fn status(&self, job_id: &str) -> Result<JobHandle, JobError> {
        let exit_code = {
            let mut state = self.lock();
            if state.current_job_id.as_deref() == Some(job_id) {
                state.refresh();
                state.exit_code
            } else if let Some(code) = state.finished.get(job_id) {
                Some(*code)
            } else {
                return Err(JobError::NotFound(job_id.to_string()));
            }
        };
        Ok(JobHandle {
            job_id: job_id.to_string(),
            state: match exit_code {
                None => "running",
                Some(0) => "succeeded",
                Some(_) => "failed",
            },
            exit_code,
            log_path: self.jobs_dir.join(format!("{job_id}.jsonl")),
        })
    }

    /// `_job_has_exited(job_id)`: a job replaced as the current one has necessarily exited.
    fn job_has_exited(&self, job_id: &str) -> bool {
        let mut state = self.lock();
        if state.current_job_id.as_deref() != Some(job_id) {
            return true;
        }
        state.refresh();
        state.exit_code.is_some()
    }

    /// Stop the current child (used by the tests to clean up a job that never ends).
    pub fn kill_current(&self) {
        let mut state = self.lock();
        if let Some(child) = state.process.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        state.refresh();
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
        Ok(TailLog::new(
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
}

/// The iterator `tail_log` returns: blocking, polling the file every `poll_interval`.
pub struct TailLog<'a> {
    /// `_job_has_exited(job_id)`: asked only after a read found nothing new.
    has_exited: Box<dyn FnMut() -> bool + Send + 'a>,
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
            if (self.has_exited)() {
                // The file is final, but the child may have written between the read above and
                // this check: read once more before stopping.
                self.exited = true;
                continue;
            }
            std::thread::sleep(self.poll_interval);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

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
