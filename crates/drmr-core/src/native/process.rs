//! A child whose supervisor retains responsibility for its process tree.

use std::io;
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};

/// A supervised child with the same pipe and polling interfaces as `std::process::Child`.
/// On Windows assignment to both jobs completes while the child is suspended. A
/// retained, noninheritable kill-on-close job also covers supervisor crashes.
#[derive(Debug)]
pub struct ManagedChild {
    #[cfg(unix)]
    child: std::process::Child,
    #[cfg(unix)]
    own_tree: bool,
    #[cfg(windows)]
    child: Box<dyn process_wrap::std::ChildWrapper>,
    #[cfg(windows)]
    job: std::sync::Arc<win32job::Job>,
    /// The child's piped standard input, if requested.
    pub stdin: Option<ChildStdin>,
    /// The child's piped standard output, if requested.
    pub stdout: Option<ChildStdout>,
    /// The child's piped standard error, if requested.
    pub stderr: Option<ChildStderr>,
}

impl ManagedChild {
    /// Spawn without a shell. `own_tree` selects a Unix process group; Windows
    /// always owns a nested job, including children of an already supervised worker.
    pub fn spawn(command: Command, own_tree: bool) -> io::Result<Self> {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let mut command = command;
            if own_tree {
                command.process_group(0);
            }
            let mut child = command.spawn()?;
            Ok(Self {
                stdin: child.stdin.take(),
                stdout: child.stdout.take(),
                stderr: child.stderr.take(),
                child,
                own_tree,
            })
        }
        #[cfg(windows)]
        {
            use process_wrap::std::{CommandWrap, JobObject};
            let _ = own_tree;
            windows::ensure_process_job()?;
            let job = windows::child_job()?;
            let mut command = CommandWrap::from(command);
            // CrashJob is applied first: its assignment must precede JobObject's
            // assignment and resume. Neither job permits child breakaway.
            command
                .wrap(windows::CrashJob(std::sync::Arc::clone(&job)))
                .wrap(JobObject);
            let mut child = command.spawn()?;
            Ok(Self {
                stdin: child.stdin().take(),
                stdout: child.stdout().take(),
                stderr: child.stderr().take(),
                child,
                job,
            })
        }
    }

    /// The top-level child's process identifier.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Poll the direct child, retaining the jobs until this supervisor drops it.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        #[cfg(unix)]
        {
            self.child.try_wait()
        }
        #[cfg(windows)]
        {
            self.child.inner_mut().try_wait()
        }
    }

    /// Wait for the direct child. Windows also terminates and drains its nested
    /// job before returning; Unix descendants remain owned until cleanup.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        #[cfg(unix)]
        {
            self.child.wait()
        }
        #[cfg(windows)]
        {
            let status = self.child.inner_mut().wait()?;
            self.kill()?;
            Ok(status)
        }
    }

    /// Terminate the owned process tree (or the direct child in an inherited Unix group).
    /// Windows waits for every process in this child's nested job to leave before
    /// returning, including when `own_tree` was false inside a worker.
    pub fn kill(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            if self.own_tree {
                rustix::process::kill_process_group(
                    rustix::process::Pid::from_child(&self.child),
                    rustix::process::Signal::KILL,
                )
                .map_err(Into::into)
            } else {
                self.child.kill()
            }
        }
        #[cfg(windows)]
        {
            self.child.start_kill()?;
            windows::drain(&self.job)?;
            self.child.inner_mut().wait().map(|_| ())
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        let _ = self.kill();
        #[cfg(unix)]
        let _ = self.wait();
    }
}

#[cfg(windows)]
mod windows {
    use process_wrap::std::{ChildWrapper, CommandWrap, CommandWrapper};
    use std::io;
    use std::os::windows::io::{AsRawHandle, BorrowedHandle};
    use std::sync::{Arc, OnceLock};
    use win32job::{ExtendedLimitInfo, Job};

    // Static values are retained until process exit. Windows then closes this
    // noninheritable handle and kills the inherited tree, even if this process
    // died between creating a suspended child and assigning its per-child job.
    static PROCESS_JOB: OnceLock<io::Result<Job>> = OnceLock::new();

    pub(super) fn ensure_process_job() -> io::Result<()> {
        PROCESS_JOB
            .get_or_init(|| {
                let mut limits = ExtendedLimitInfo::new();
                limits.limit_kill_on_job_close();
                let job = Job::create_with_limit_info(&limits).map_err(io::Error::other)?;
                job.assign_current_process().map_err(io::Error::other)?;
                Ok(job)
            })
            .as_ref()
            .map(|_| ())
            .map_err(|error| io::Error::new(error.kind(), "process isolation unavailable"))
    }

    pub(super) fn child_job() -> io::Result<Arc<Job>> {
        let mut limits = ExtendedLimitInfo::new();
        limits.limit_kill_on_job_close();
        Job::create_with_limit_info(&limits)
            .map(Arc::new)
            .map_err(io::Error::other)
    }

    pub(super) fn drain(job: &Job) -> io::Result<()> {
        super::wait_for_job_empty(|| {
            job.query_process_id_list()
                .map(|processes| processes.is_empty())
                .map_err(io::Error::from)
        })
    }

    #[derive(Debug)]
    pub(super) struct CrashJob(pub(super) Arc<Job>);

    impl CommandWrapper for CrashJob {
        fn wrap_child(
            &mut self,
            mut child: Box<dyn ChildWrapper>,
            _core: &CommandWrap,
        ) -> io::Result<Box<dyn ChildWrapper>> {
            let attach = || -> io::Result<()> {
                let handle = child
                    .process_handle()
                    .ok_or_else(|| io::Error::other("missing process handle"))?;
                self.0
                    .assign_process(handle.as_raw_handle() as isize)
                    .map_err(io::Error::other)
            };
            match attach() {
                Ok(()) => Ok(Box::new(CrashChild {
                    child,
                    _job: Arc::clone(&self.0),
                })),
                Err(error) => {
                    // Assignment failure must never resume or strand a suspended child.
                    let _ = child.start_kill();
                    let _ = child.wait();
                    Err(error)
                }
            }
        }
    }

    #[derive(Debug)]
    struct CrashChild {
        child: Box<dyn ChildWrapper>,
        _job: Arc<Job>,
    }

    impl ChildWrapper for CrashChild {
        fn inner(&self) -> &dyn ChildWrapper {
            self.child.as_ref()
        }
        fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
            self.child.as_mut()
        }
        fn into_inner(self: Box<Self>) -> Box<dyn ChildWrapper> {
            self.child
        }
        fn process_handle(&self) -> Option<BorrowedHandle<'_>> {
            self.child.process_handle()
        }
    }
}

// Asset workers are already supervised as one process group on Unix. Starting
// separate groups inside them would escape their supervisor's crash cleanup.
static IN_ASSET_WORKER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether this process is an asset worker supervised as one process tree.
pub fn in_asset_worker() -> bool {
    IN_ASSET_WORKER.load(std::sync::atomic::Ordering::SeqCst)
}

pub(crate) fn mark_asset_worker() {
    IN_ASSET_WORKER.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(any(windows, test))]
fn wait_for_job_empty(mut query: impl FnMut() -> io::Result<bool>) -> io::Result<()> {
    loop {
        match query() {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            // The pinned job wrapper has a fixed process-list buffer. A larger
            // terminating tree is still active, never proof of completion.
            Err(error) if error.raw_os_error() == Some(234) => {}
            Err(error) => return Err(error),
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[cfg(test)]
mod drain_tests {
    use super::*;
    #[test]
    fn an_oversized_or_nonempty_job_is_never_admitted_as_drained() {
        let mut calls = 0;
        wait_for_job_empty(|| {
            calls += 1;
            match calls {
                1 => Err(io::Error::from_raw_os_error(234)),
                2 => Ok(false),
                3 => Ok(true),
                _ => panic!("queried after completion"),
            }
        })
        .unwrap();
        assert_eq!(calls, 3);
    }
    #[test]
    fn a_job_query_failure_propagates_without_claiming_completion() {
        let error = wait_for_job_empty(|| Err(io::Error::from_raw_os_error(5))).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(5));
    }
}
