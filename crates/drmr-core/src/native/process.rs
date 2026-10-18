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
    _job: std::sync::Arc<win32job::Job>,
    cleanup: CleanupState,
    /// The child's piped standard input, if requested.
    pub stdin: Option<ChildStdin>,
    /// The child's piped standard output, if requested.
    pub stdout: Option<ChildStdout>,
    /// The child's piped standard error, if requested.
    pub stderr: Option<ChildStderr>,
}

#[derive(Debug, PartialEq, Eq)]
enum CleanupState {
    Pending,
    Confirmed,
    Unconfirmed,
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
                cleanup: CleanupState::Pending,
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
                cleanup: CleanupState::Pending,
                stdin: child.stdin().take(),
                stdout: child.stdout().take(),
                stderr: child.stderr().take(),
                child,
                _job: job,
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
            self.finish()?;
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
            self.finish()
        }
    }

    /// Terminate owned descendants and reap the direct child before releasing
    /// resources used by them. Successful confirmation is idempotent. Errors
    /// mean cleanup is unconfirmed; callers must not report successful work.
    /// After reaping its direct child, Unix waits up to 30 seconds for group absence.
    /// Windows waits up to 30 seconds for the terminal job notification.
    /// This deadline does not bound other blocking I/O or pipe-reader joins.
    pub fn finish(&mut self) -> io::Result<()> {
        if self.cleanup == CleanupState::Confirmed {
            return Ok(());
        }
        let result = self.finish_inner();
        self.cleanup = if result.is_ok() {
            CleanupState::Confirmed
        } else {
            CleanupState::Unconfirmed
        };
        result
    }

    fn finish_inner(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            // Inherited worker groups remain owned by the worker supervisor.
            // A reaped direct child needs no further direct-child kill.
            if !self.own_tree && self.child.try_wait()?.is_some() {
                return Ok(());
            }
            let killed = self.kill();
            let reaped = self.child.wait();
            reaped?;
            if self.own_tree {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
                // SIGKILL delivery and the direct child's reap do not finish descendants.
                // The completion loop only observes: after this reap, the identifier could
                // be reused once the owned group disappears, so it must not signal again.
                confirm_group_completion(
                    killed,
                    || {
                        rustix::process::test_kill_process_group(rustix::process::Pid::from_child(
                            &self.child,
                        ))
                    },
                    || std::time::Instant::now() >= deadline,
                    || std::thread::sleep(std::time::Duration::from_millis(1)),
                )
            } else {
                match killed {
                    Err(error)
                        if error.raw_os_error() == Some(rustix::io::Errno::SRCH.raw_os_error()) =>
                    {
                        Ok(())
                    }
                    result => result,
                }
            }
        }
        #[cfg(windows)]
        {
            self.child.start_kill()?;
            let wrapper = (&mut *self.child as &mut dyn std::any::Any)
                .downcast_mut::<process_wrap::std::JobObjectChild>()
                .ok_or_else(|| io::Error::other("process tree supervisor unavailable"))?;
            if !wrapper.wait_for_tree_timeout(std::time::Duration::from_secs(30))? {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "process tree cleanup timed out",
                ));
            }
            self.child.wait().map(|_| ())
        }
    }
}

#[cfg(unix)]
fn confirm_group_completion(
    killed: io::Result<()>,
    mut group_exists: impl FnMut() -> rustix::io::Result<()>,
    mut expired: impl FnMut() -> bool,
    mut wait: impl FnMut(),
) -> io::Result<()> {
    use rustix::io::Errno;
    if let Err(error) = killed
        && !matches!(error.raw_os_error(), Some(code) if code == Errno::SRCH.raw_os_error() || code == Errno::PERM.raw_os_error())
    {
        return Err(error);
    }
    loop {
        match group_exists() {
            Err(Errno::SRCH) => return Ok(()),
            // Darwin can retain a zombie group while excluding its members from signals.
            // Neither an existing group nor EPERM proves that its resources are released.
            Ok(()) | Err(Errno::PERM) => {}
            Err(error) => return Err(error.into()),
        }
        if expired() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "process group cleanup timed out",
            ));
        }
        wait();
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        // An explicit failed attempt has already spent its bounded cleanup
        // deadline. Do not hide another attempt in resource destruction.
        if self.cleanup == CleanupState::Pending {
            let _ = self.finish();
        }
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

#[cfg(all(test, unix))]
mod cleanup_tests {
    use super::*;
    use rustix::io::Errno;

    #[test]
    fn a_live_or_zombie_group_requires_observed_absence_even_after_a_successful_kill() {
        for killed in [Ok(()), Err(Errno::PERM.into()), Err(Errno::SRCH.into())] {
            let mut states = [Ok(()), Err(Errno::PERM), Err(Errno::SRCH)].into_iter();
            let waits = std::cell::Cell::new(0);
            confirm_group_completion(
                killed,
                || states.next().expect("must stop at observed absence"),
                || false,
                || waits.set(waits.get() + 1),
            )
            .unwrap();
            assert_eq!(waits.get(), 2);
        }
    }

    #[test]
    fn absence_needs_no_wait_and_live_or_permission_states_reach_the_deadline() {
        confirm_group_completion(
            Ok(()),
            || Err(Errno::SRCH),
            || true,
            || panic!("absent group needs no wait"),
        )
        .unwrap();
        for state in [Ok(()), Err(Errno::PERM)] {
            let waits = std::cell::Cell::new(0);
            let error = confirm_group_completion(
                Ok(()),
                || state,
                || waits.get() == 2,
                || waits.set(waits.get() + 1),
            )
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            assert_eq!(waits.get(), 2);
        }
    }

    #[test]
    fn unrelated_kill_and_observation_errors_remain_unconfirmed() {
        let error = confirm_group_completion(
            Err(Errno::IO.into()),
            || panic!("must preserve kill error"),
            || false,
            || panic!("must not wait after error"),
        )
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(Errno::IO.raw_os_error()));
        let error = confirm_group_completion(
            Ok(()),
            || Err(Errno::IO),
            || false,
            || panic!("must not wait after error"),
        )
        .unwrap_err();
        assert_eq!(error.raw_os_error(), Some(Errno::IO.raw_os_error()));
    }
}
