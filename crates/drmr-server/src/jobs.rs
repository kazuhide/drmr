//! Supervised common-workflow execution with one retained ownership lease.

use crate::pyutil;
use drmr_core::native::{self, ManagedChild};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::Write;
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

/// Native ownership observations, without interpreting a common CLI result.
#[derive(Clone, Debug)]
pub struct JobObservation {
    pub handle: JobHandle,
    pub supervision_confirmed: bool,
    pub cleanup_confirmed: bool,
}
/// The single execution lease used by all common workflow operations.
#[derive(Clone, Debug)]
pub struct ExecutionObservation {
    pub job_id: Option<String>,
    pub busy: bool,
    pub closing: bool,
    pub cleanup_unconfirmed: bool,
    pub owner: Option<&'static str>,
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
    /// A launched CLI did not establish supervision; its cleanup owner is retained.
    #[error("job-supervision-failed")]
    SupervisionFailed(JobHandle),
    /// Process-tree cleanup failed; the execution lease remains held.
    #[error("cleanup-unconfirmed")]
    CleanupUnconfirmed,
    /// The server is closing and no longer accepts jobs.
    #[error("server-shutting-down")]
    ShuttingDown,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SupervisionState {
    Pending,
    Established,
    Failed,
}

#[derive(Clone, Copy)]
struct Finished {
    code: i32,
    stop_requested: bool,
    supervision: SupervisionState,
}

enum CommonOwner {
    Preparation {
        token: String,
        cleanup_unconfirmed: bool,
    },
    Job(String),
}
struct State {
    current_job_id: Option<String>,
    process: Option<Arc<Mutex<ManagedChild>>>,
    cleanup_worker: Option<std::thread::JoinHandle<std::io::Result<i32>>>,
    log_file: Option<File>,
    exit_code: Option<i32>,
    stop_requested: bool,
    supervision: SupervisionState,
    closing: bool,
    cleanup_unconfirmed: bool,
    common_owner: Option<CommonOwner>,
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
        if self.exit_code.is_none() && self.process.is_some() {
            self.stop_requested = true;
            if self.cleanup_worker.is_none() {
                self.start_cleanup(None, finish)?;
            }
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
        self.common_owner.is_some() || (self.process.is_some() && self.exit_code.is_none())
    }

    fn handle(&self, job_id: &str, jobs_dir: &Path) -> Result<JobHandle, JobError> {
        let (code, stopped, supervision) = if self.current_job_id.as_deref() == Some(job_id) {
            self.check_cleanup()?;
            (self.exit_code, self.stop_requested, self.supervision)
        } else if let Some(finished) = self.finished.get(job_id) {
            (
                Some(finished.code),
                finished.stop_requested,
                finished.supervision,
            )
        } else {
            return Err(JobError::NotFound(job_id.to_string()));
        };
        Ok(JobHandle {
            job_id: job_id.to_string(),
            state: match (code, stopped) {
                (None, _) => "running",
                (Some(_), true) => "interrupted",
                (Some(0), false) if supervision == SupervisionState::Established => "succeeded",
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
            cli_argv,
            base_env,
            state: Mutex::new(State {
                current_job_id: None,
                process: None,
                cleanup_worker: None,
                stop_requested: false,
                supervision: SupervisionState::Pending,
                closing: false,
                log_file: None,
                exit_code: None,
                cleanup_unconfirmed: false,
                common_owner: None,
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
            .filter(|(name, _)| !is_drm_var(name))
            .collect()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Exercise native supervision without exposing a generic production launch API.
    #[cfg(test)]
    fn submit_command(
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
        self.launch(&mut state, &argv, env)
    }

    /// Reserve the shared lease before a common attempt creates private copies.
    pub(crate) fn reserve_common(&self, token: &str) -> Result<(), JobError> {
        let mut state = self.lock();
        state.refresh()?;
        if state.closing {
            return Err(JobError::ShuttingDown);
        }
        if state.busy() {
            return Err(JobError::AlreadyRunning);
        }
        if !crate::common_protocol::correlation_id(token) {
            return Err(JobError::CleanupUnconfirmed);
        }
        state.common_owner = Some(CommonOwner::Preparation {
            token: token.into(),
            cleanup_unconfirmed: false,
        });
        Ok(())
    }
    /// A reserved common attempt uses only private typed result/progress channels.
    pub(crate) fn submit_common(
        &self,
        token: &str,
        request_file: &Path,
    ) -> Result<JobHandle, JobError> {
        let mut state = self.lock();
        state.refresh()?;
        if !matches!(&state.common_owner, Some(CommonOwner::Preparation { token: owner, .. }) if owner == token)
        {
            return Err(JobError::CleanupUnconfirmed);
        }
        if state.closing {
            return Err(JobError::ShuttingDown);
        }
        let mut argv = self.cli_argv.clone();
        argv.push(crate::common_protocol::JOB_ARGUMENT.into());
        argv.push(request_file.as_os_str().to_owned());
        let result = self.launch(&mut state, &argv, &self.fresh_env());
        match &result {
            Ok(handle) | Err(JobError::SupervisionFailed(handle)) => {
                state.common_owner = Some(CommonOwner::Job(handle.job_id.clone()));
            }
            _ => {}
        }
        result
    }
    pub(crate) fn retain_common_preparation(&self, token: &str) -> Result<(), JobError> {
        let mut state = self.lock();
        match &mut state.common_owner {
            Some(CommonOwner::Preparation {
                token: owner,
                cleanup_unconfirmed,
            }) if owner == token => {
                *cleanup_unconfirmed = true;
                Ok(())
            }
            _ => Err(JobError::CleanupUnconfirmed),
        }
    }
    /// Only the exact unlaunched reservation may be released after its copies are gone.
    pub(crate) fn release_common_preparation(&self, token: &str) -> Result<(), JobError> {
        let mut state = self.lock();
        if !matches!(&state.common_owner, Some(CommonOwner::Preparation { token: owner, .. }) if owner == token)
        {
            return Err(JobError::CleanupUnconfirmed);
        }
        state.common_owner = None;
        Ok(())
    }
    /// Release common admission only after its native tree and private-copy cleanup.
    pub fn release_common(&self, job_id: &str) -> Result<(), JobError> {
        let mut state = self.lock();
        state.refresh()?;
        if !matches!(&state.common_owner, Some(CommonOwner::Job(owner)) if owner == job_id)
            || state.exit_code.is_none()
        {
            return Err(JobError::CleanupUnconfirmed);
        }
        state.common_owner = None;
        Ok(())
    }

    /// Forget only a common service's terminal receipt when its session is removed.
    pub(crate) fn forget_common(&self, job_id: &str) -> Result<(), JobError> {
        let mut state = self.lock();
        state.refresh()?;
        if matches!(&state.common_owner, Some(CommonOwner::Job(owner)) if owner == job_id) {
            return Err(JobError::CleanupUnconfirmed);
        }
        if state.current_job_id.as_deref() == Some(job_id) {
            if state.process.is_some() || state.exit_code.is_none() {
                return Err(JobError::CleanupUnconfirmed);
            }
            state.current_job_id = None;
            state.exit_code = None;
        } else {
            state.finished.remove(job_id);
        }
        Ok(())
    }

    pub fn observation(&self, job_id: &str) -> Result<JobObservation, JobError> {
        let mut state = self.lock();
        if state.current_job_id.as_deref() == Some(job_id) {
            state.refresh()?;
        }
        let handle = state.handle(job_id, &self.jobs_dir)?;
        let supervision = if state.current_job_id.as_deref() == Some(job_id) {
            state.supervision
        } else {
            state
                .finished
                .get(job_id)
                .ok_or_else(|| JobError::NotFound(job_id.into()))?
                .supervision
        };
        Ok(JobObservation {
            cleanup_confirmed: handle.exit_code.is_some(),
            handle,
            supervision_confirmed: supervision == SupervisionState::Established,
        })
    }

    pub fn execution(&self) -> ExecutionObservation {
        let mut state = self.lock();
        let _ = state.refresh();
        ExecutionObservation {
            job_id: if matches!(state.common_owner, Some(CommonOwner::Preparation { .. })) {
                None
            } else {
                state.busy().then(|| state.current_job_id.clone()).flatten()
            },
            busy: state.busy(),
            closing: state.closing,
            cleanup_unconfirmed: state.cleanup_unconfirmed
                || matches!(
                    state.common_owner,
                    Some(CommonOwner::Preparation {
                        cleanup_unconfirmed: true,
                        ..
                    })
                ),
            owner: (state.common_owner.is_some() || state.busy()).then_some("common"),
        }
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
        let program = argv.first().cloned().unwrap_or_default();
        let mut command = Command::new(&program);
        #[cfg(unix)]
        command.arg("--internal-tool-broker-v1");
        command
            .args(&argv[1..])
            .env_clear()
            .envs(env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = ManagedChild::spawn_supervised_cli(command, env).map_err(JobError::Launch)?;
        if let (Some(current), Some(code)) = (state.current_job_id.take(), state.exit_code) {
            state.finished.insert(
                current,
                Finished {
                    code,
                    stop_requested: state.stop_requested,
                    supervision: state.supervision,
                },
            );
        }
        state.current_job_id = Some(job_id.clone());
        state.process = Some(Arc::new(Mutex::new(child)));
        state.log_file = Some(log_file);
        state.exit_code = None;
        state.stop_requested = false;
        state.supervision = SupervisionState::Pending;
        let handle = JobHandle {
            job_id,
            state: "running",
            exit_code: None,
            stop_requested: false,
            log_path,
        };
        let confirmed = state
            .process
            .as_ref()
            .expect("the launched child has an owner")
            .lock()
            .map_err(|_| std::io::Error::other("job-supervision-failed"))
            .and_then(|mut child| child.confirm_supervision());
        if confirmed.is_err() {
            state.supervision = SupervisionState::Failed;
            if let Some(log) = &mut state.log_file {
                let _ = writeln!(log, "{{\"event\":\"job-supervision-failed\"}}");
            }
            // Callers must retain the launched job's private request files even
            // when the cleanup worker cannot start or later cannot confirm exit.
            let _ = state.start_cleanup(None, ManagedChild::finish);
            return Err(JobError::SupervisionFailed(handle));
        }
        state.supervision = SupervisionState::Established;
        Ok(handle)
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
    }

    /// Stop and confirm the owned tree, retaining ownership on any error.
    /// The caller may block, but status and repeated stop requests remain responsive.
    pub fn shutdown(&self) -> Result<(), JobError> {
        let job_id = {
            let mut state = self.lock();
            state.closing = true;
            if matches!(state.common_owner, Some(CommonOwner::Preparation { .. })) {
                None
            } else {
                state.current_job_id.clone()
            }
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

    /// Stop the current child (used by the tests to clean up a job that never ends).
    pub fn kill_current(&self) {
        let job_id = self.lock().current_job_id.clone();
        if let Some(job_id) = job_id {
            let _ = self
                .request_stop(&job_id)
                .and_then(|_| self.wait_for_cleanup(&job_id));
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn common_reservation_rejects_wrong_and_old_tokens_and_never_exposes_a_previous_job() {
        let root = native::private_tempdir().unwrap();
        let jobs = root.path().join("jobs");
        native::create_private_dir_all(&jobs).unwrap();
        let manager = ConversionJobManager::new(
            &jobs,
            vec![root.path().join("absent-cli").into_os_string()],
            Arc::new(Vec::new),
        )
        .unwrap();
        let old = "a".repeat(32);
        {
            let mut state = manager.lock();
            state.current_job_id = Some(old.clone());
            state.exit_code = Some(0);
            state.supervision = SupervisionState::Established;
        }
        let token = "b".repeat(32);
        manager.reserve_common(&token).unwrap();
        let observed = manager.execution();
        assert!(observed.busy);
        assert_eq!(observed.owner, Some("common"));
        assert!(observed.job_id.is_none());
        assert!(matches!(
            manager.submit_command(&[], &[]),
            Err(JobError::AlreadyRunning)
        ));
        assert!(manager.release_common(&old).is_err());
        assert!(manager.release_common_preparation(&old).is_err());
        assert!(
            manager
                .submit_common(&old, &jobs.join("unused.json"))
                .is_err()
        );
        assert_eq!(std::fs::read_dir(&jobs).unwrap().count(), 0);
        let request = jobs.join("request.json");
        drop(native::create_private_new(&request).unwrap());
        assert!(matches!(
            manager.submit_common(&token, &request),
            Err(JobError::Launch(_))
        ));
        assert!(manager.execution().busy);
        manager.request_stop(&old).unwrap();
        assert!(manager.execution().busy);
        manager.retain_common_preparation(&token).unwrap();
        assert!(manager.execution().cleanup_unconfirmed);
        manager.shutdown().unwrap();
        assert!(manager.execution().job_id.is_none());
        assert!(matches!(
            manager.submit_common(&token, &request),
            Err(JobError::ShuttingDown)
        ));
        assert!(manager.execution().busy);
        manager.release_common_preparation(&token).unwrap();
        assert!(!manager.execution().busy);
        assert!(manager.release_common_preparation(&token).is_err());
        assert!(matches!(
            manager.reserve_common(&"c".repeat(32)),
            Err(JobError::ShuttingDown)
        ));
    }
    #[cfg(unix)]
    #[test]
    fn a_cli_without_the_handshake_fails_even_if_its_actual_exit_code_is_zero() {
        let root = tempfile::tempdir().unwrap();
        let manager = ConversionJobManager::new(
            &root.path().join("jobs"),
            vec!["/usr/bin/true".into()],
            Arc::new(Vec::new),
        )
        .unwrap();
        let Err(JobError::SupervisionFailed(job)) = manager.submit_command(&[], &[]) else {
            panic!("an executable without the protocol must not become an accepted job");
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let terminal = loop {
            let status = manager.status(&job.job_id).unwrap();
            if status.state != "running" {
                break status;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(terminal.state, "failed");
        assert_eq!(terminal.exit_code, Some(0));
        assert!(!terminal.stop_requested);
        assert!(!manager.is_busy());
        assert!(
            std::fs::read_to_string(&job.log_path)
                .unwrap()
                .contains("job-supervision-failed")
        );
        let Err(JobError::SupervisionFailed(next)) = manager.submit_command(&[], &[]) else {
            panic!("a replacement without supervision must also be refused");
        };
        assert_ne!(next.job_id, job.job_id);
        let previous = manager.status(&job.job_id).unwrap();
        assert_eq!(previous.state, "failed");
        assert_eq!(previous.exit_code, Some(0));
        manager.shutdown().unwrap();
    }

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
            supervision: SupervisionState::Established,
            closing: false,
            log_file: Some(File::create(root.join("job.jsonl")).unwrap()),
            exit_code: None,
            cleanup_unconfirmed: false,
            common_owner: None,
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
    fn stop_during_natural_cleanup_records_intent_without_starting_another_worker() {
        let root = tempfile::tempdir().unwrap();
        let manager =
            ConversionJobManager::new(&root.path().join("jobs"), Vec::new(), Arc::new(Vec::new))
                .unwrap();
        *manager.lock() = state_with_child(root.path());
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        manager
            .lock()
            .refresh_with(
                |_| Some(0),
                move |child| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    child.finish()
                },
            )
            .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let pending = manager.request_stop("job").unwrap();
        release_tx.send(()).unwrap();
        manager.wait_for_cleanup("job").unwrap();
        let completed = manager.status("job").unwrap();
        assert!(
            pending.stop_requested,
            "a stop accepted before confirmed completion retains intent"
        );
        assert_eq!(pending.state, "running");
        assert_eq!(completed.state, "interrupted");
        assert_eq!(completed.exit_code, Some(0));
        assert!(completed.stop_requested);
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
            crate::ServerConfig::new(root.path().join("input"), root.path().join("output"));
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
        let mut response = agent
            .get(&format!("{url}/api/common/capabilities"))
            .call()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let payload: Value =
            serde_json::from_str(&response.body_mut().read_to_string().unwrap()).unwrap();
        assert_eq!(payload["execution"]["availability"], "closing");
        assert!(app.manager.status("job").unwrap().stop_requested);
        // A valid envelope reaches closed admission instead of failing JSON parsing.
        let request = serde_json::json!({"schema_version":1,"server_id":payload["server_id"],
            "session_id":"a".repeat(32),"request_id":"b".repeat(32),
            "operation":{"kind":"analyze"},"runtime":{"keys_ref":null,"max_workers":null}});
        let mut rejected = agent
            .post(&format!("{url}/api/common/jobs"))
            .header("Content-Type", "application/json")
            .send(request.to_string())
            .unwrap();
        assert_eq!(rejected.status().as_u16(), 503);
        let refusal: Value =
            serde_json::from_str(&rejected.body_mut().read_to_string().unwrap()).unwrap();
        assert_eq!(refusal["error"]["code"], "server-shutting-down");
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
                manager.observation("job"),
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
}
