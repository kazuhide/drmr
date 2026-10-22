//! A child whose supervisor retains responsibility for its process tree.

use std::io;
use std::process::{ChildStderr, ChildStdin, ChildStdout, Command, ExitStatus};

/// A supervised child with the same pipe and polling interfaces as `std::process::Child`.
/// On Windows assignment to both jobs completes while the child is suspended. A
/// retained, noninheritable kill-on-close job also covers supervisor crashes.
#[derive(Debug)]
pub struct ManagedChild {
    #[cfg(unix)]
    child: UnixChild,
    #[cfg(unix)]
    broker: Option<super::broker::Broker>,
    #[cfg(unix)]
    own_tree: bool,
    #[cfg(unix)]
    termination_attempted: bool,
    #[cfg(unix)]
    reaped_status: Option<ExitStatus>,
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

#[cfg(unix)]
#[derive(Debug)]
enum UnixChild {
    Local(std::process::Child),
    Remote(super::broker::Remote),
}

/// Explicit child I/O; unlike `Command`, this description can cross the private broker.
#[derive(Debug)]
pub struct ManagedIo {
    pub stdin: ManagedStdio,
    pub stdout: ManagedStdio,
    pub stderr: ManagedStdio,
}

/// A pipe, a null stream, or an already owned file. Remote UI jobs admit pipes/null only.
#[derive(Debug)]
pub enum ManagedStdio {
    Null,
    Piped,
    File(std::fs::File),
}
impl ManagedStdio {
    fn into_stdio(self) -> std::process::Stdio {
        match self {
            Self::Null => std::process::Stdio::null(),
            Self::Piped => std::process::Stdio::piped(),
            Self::File(file) => file.into(),
        }
    }
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
            let _gate = super::broker::SPAWN_GATE
                .lock()
                .map_err(|_| io::Error::other("process admission unavailable"))?;
            let mut child = command.spawn()?;
            Ok(Self {
                cleanup: CleanupState::Pending,
                stdin: child.stdin.take(),
                stdout: child.stdout.take(),
                stderr: child.stderr.take(),
                child: UnixChild::Local(child),
                broker: None,
                own_tree,
                termination_attempted: false,
                reaped_status: None,
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

    /// Start a known server CLI with a private tool-ownership channel. The caller
    /// must retain this owner before checking `confirm_supervision`.
    pub fn spawn_supervised_cli(
        mut command: Command,
        environment: &[(std::ffi::OsString, std::ffi::OsString)],
    ) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let (mut broker, endpoint) = super::broker::Broker::new(environment)?;
            command.stdin(std::process::Stdio::from(endpoint));
            command.env(super::broker::ENV, std::process::id().to_string());
            match Self::spawn(command, true) {
                Ok(mut child) => {
                    child.broker = Some(broker);
                    Ok(child)
                }
                Err(error) => {
                    broker.finish()?;
                    Err(error)
                }
            }
        }
        #[cfg(windows)]
        {
            let _ = environment;
            command.env_remove("DRMR_TOOL_BROKER_V1");
            Self::spawn(command, true)
        }
    }

    /// Confirm the CLI's versioned bootstrap without surrendering its owner on error.
    pub fn confirm_supervision(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.broker
                .as_ref()
                .map_or(Ok(()), super::broker::Broker::confirm)
        }
        #[cfg(windows)]
        {
            Ok(())
        }
    }

    /// Spawn with explicit I/O, preserving local behavior outside a supervised CLI.
    pub fn spawn_with_io(command: Command, own_tree: bool, io: ManagedIo) -> io::Result<Self> {
        #[cfg(unix)]
        if super::broker::active() {
            return super::broker::Remote::spawn(&command, &io);
        }
        Self::spawn_with_io_local(command, own_tree, io)
    }

    pub(super) fn spawn_with_io_local(
        mut command: Command,
        own_tree: bool,
        io: ManagedIo,
    ) -> io::Result<Self> {
        command
            .stdin(io.stdin.into_stdio())
            .stdout(io.stdout.into_stdio())
            .stderr(io.stderr.into_stdio());
        command.env_remove("DRMR_TOOL_BROKER_V1");
        Self::spawn(command, own_tree)
    }

    #[cfg(unix)]
    pub(super) fn remote(
        child: super::broker::Remote,
        stdin: Option<ChildStdin>,
        stdout: Option<ChildStdout>,
        stderr: Option<ChildStderr>,
    ) -> Self {
        Self {
            child: UnixChild::Remote(child),
            broker: None,
            own_tree: true,
            termination_attempted: false,
            reaped_status: None,
            cleanup: CleanupState::Pending,
            stdin,
            stdout,
            stderr,
        }
    }

    /// The top-level child's process identifier.
    pub fn id(&self) -> u32 {
        #[cfg(unix)]
        {
            match &self.child {
                UnixChild::Local(child) => child.id(),
                UnixChild::Remote(child) => child.id(),
            }
        }
        #[cfg(windows)]
        {
            self.child.id()
        }
    }

    /// Poll the direct child, retaining the jobs until this supervisor drops it.
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        #[cfg(unix)]
        {
            if let Some(status) = self.reaped_status {
                return Ok(Some(status));
            }
            match &mut self.child {
                UnixChild::Local(child) => {
                    if self.own_tree {
                        observe(child, false)
                    } else {
                        child.try_wait()
                    }
                }
                UnixChild::Remote(child) => child.try_wait(),
            }
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
            if let Some(status) = self.reaped_status {
                return Ok(status);
            }
            match &mut self.child {
                UnixChild::Local(child) => {
                    if self.own_tree {
                        observe(child, true)?
                            .ok_or_else(|| io::Error::other("child status unavailable"))
                    } else {
                        child.wait()
                    }
                }
                UnixChild::Remote(child) => child.wait(),
            }
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
        if self.broker.is_some() {
            return self.finish();
        }
        self.kill_inner()
    }

    fn kill_inner(&mut self) -> io::Result<()> {
        #[cfg(unix)]
        {
            let child = match &mut self.child {
                UnixChild::Remote(child) => return child.finish(),
                UnixChild::Local(child) => child,
            };
            if self.own_tree {
                terminate_group(
                    &mut self.termination_attempted,
                    self.reaped_status.is_some(),
                    || {
                        rustix::process::kill_process_group(
                            rustix::process::Pid::from_child(child),
                            rustix::process::Signal::KILL,
                        )
                        .map_err(Into::into)
                    },
                )
            } else {
                child.kill()
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
        #[cfg(unix)]
        if let Some(broker) = &self.broker {
            broker.close();
        }
        let result = self.finish_inner();
        #[cfg(unix)]
        let result = {
            let broker = self
                .broker
                .as_mut()
                .map_or(Ok(()), super::broker::Broker::finish);
            result.and(broker)
        };
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
            if let UnixChild::Remote(child) = &mut self.child {
                return child.finish();
            }
            // Inherited worker groups remain owned by the worker supervisor.
            // A reaped direct child needs no further direct-child kill.
            if !self.own_tree && self.try_wait()?.is_some() {
                return Ok(());
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            let killed = self.kill_inner();
            if self.own_tree
                && let Err(error) = &killed
                && !matches!(error.raw_os_error(), Some(code) if code == rustix::io::Errno::SRCH.raw_os_error() || code == rustix::io::Errno::PERM.raw_os_error())
            {
                return Err(io::Error::new(
                    error.kind(),
                    "process group termination unavailable",
                ));
            }
            let UnixChild::Local(child) = &mut self.child else {
                unreachable!()
            };
            if self.own_tree && self.reaped_status.is_none() {
                while observe(child, false)?.is_none() {
                    if std::time::Instant::now() >= deadline {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "process group cleanup timed out",
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            }
            if self.reaped_status.is_none() {
                self.reaped_status = Some(child.wait()?);
            }
            if self.own_tree {
                // SIGKILL delivery and the direct child's reap do not finish descendants.
                // The completion loop only observes: after this reap, the identifier could
                // be reused once the owned group disappears, so it must not signal again.
                confirm_group_completion(
                    killed,
                    || {
                        rustix::process::test_kill_process_group(rustix::process::Pid::from_child(
                            child,
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
fn terminate_group(
    sent: &mut bool,
    reaped: bool,
    signal: impl FnOnce() -> io::Result<()>,
) -> io::Result<()> {
    if reaped || *sent {
        return Ok(());
    }
    let result = signal();
    // A failed request may be retried only while the unreaped direct child
    // still pins this identifier. A successful signal is never repeated.
    if result.is_ok() {
        *sent = true;
    }
    result
}

#[cfg(unix)]
fn observe(child: &mut std::process::Child, blocking: bool) -> io::Result<Option<ExitStatus>> {
    use rustix::process::{WaitId, WaitIdOptions, waitid};
    use std::os::unix::process::ExitStatusExt;
    let mut options = WaitIdOptions::EXITED | WaitIdOptions::NOWAIT;
    if !blocking {
        options |= WaitIdOptions::NOHANG;
    }
    loop {
        let result = match waitid(
            WaitId::Pid(rustix::process::Pid::from_child(child)),
            options,
        ) {
            Err(rustix::io::Errno::INTR) => continue,
            result => result?,
        };
        let Some(status) = result else {
            return Ok(None);
        };
        if let Some(code) = status.exit_status() {
            return Ok(Some(ExitStatus::from_raw(code << 8)));
        }
        if let Some(signal) = status.terminating_signal() {
            return Ok(Some(ExitStatus::from_raw(
                signal | if status.dumped() { 0x80 } else { 0 },
            )));
        }
        if !(status.stopped() || status.trapped() || status.continued()) {
            return Err(io::Error::other("child status unavailable"));
        }
        // Darwin can report a pending job-control event even with EXITED only.
        // NOWAIT keeps that event visible; it is not a terminal child status.
        if !blocking {
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
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

#[cfg(all(test, unix))]
mod cleanup_tests {
    use super::*;
    use rustix::io::Errno;

    #[test]
    fn a_blocking_wait_ignores_stopped_events_until_the_actual_terminal_signal() {
        use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process, waitid};
        use std::os::unix::process::ExitStatusExt;
        let root = tempfile::tempdir().unwrap();
        let tool = drmr_test_support::install(
            &root.path().join("stopped"),
            &drmr_test_support::ToolSpec {
                sleep_ms: 60_000,
                ..Default::default()
            },
        )
        .unwrap();
        let mut child = ManagedChild::spawn_with_io_local(
            Command::new(tool),
            true,
            ManagedIo {
                stdin: ManagedStdio::Null,
                stdout: ManagedStdio::Null,
                stderr: ManagedStdio::Null,
            },
        )
        .unwrap();
        let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        kill_process(pid, Signal::STOP).unwrap();
        assert!(
            waitid(
                WaitId::Pid(pid),
                WaitIdOptions::STOPPED | WaitIdOptions::NOWAIT
            )
            .unwrap()
            .unwrap()
            .stopped()
        );
        let (started, ready) = std::sync::mpsc::channel();
        let (finished, completion) = std::sync::mpsc::channel();
        let (allow_cleanup, cleanup_allowed) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            started.send(()).unwrap();
            let status = child.wait();
            finished.send(()).unwrap();
            // Keep the direct child unreaped until the controller's one signal.
            cleanup_allowed.recv().unwrap();
            let cleanup = child.finish();
            (status, cleanup)
        });
        ready.recv().unwrap();
        let early = completion.recv_timeout(std::time::Duration::from_millis(20));
        // Always release and join the owned fixture before evaluating failure.
        let killed = kill_process(pid, Signal::KILL);
        allow_cleanup.send(()).unwrap();
        let (status, cleanup) = worker.join().unwrap();
        assert_eq!(early, Err(std::sync::mpsc::RecvTimeoutError::Timeout));
        killed.unwrap();
        assert_eq!(status.unwrap().signal(), Some(Signal::KILL.as_raw()));
        cleanup.unwrap();
        assert_eq!(
            waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG
            )
            .unwrap_err(),
            Errno::CHILD
        );
    }

    #[test]
    fn observed_direct_exit_stays_unreaped_until_owned_group_termination() {
        let root = tempfile::tempdir().unwrap();
        let tool = drmr_test_support::install(
            &root.path().join("exit"),
            &drmr_test_support::ToolSpec {
                exit_code: 17,
                ..Default::default()
            },
        )
        .unwrap();
        let mut command = Command::new(tool);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = ManagedChild::spawn(command, true).unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(17));
        assert_eq!(child.try_wait().unwrap().unwrap().code(), Some(17));
        let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        let pinned = rustix::process::waitid(
            rustix::process::WaitId::Pid(pid),
            rustix::process::WaitIdOptions::EXITED
                | rustix::process::WaitIdOptions::NOWAIT
                | rustix::process::WaitIdOptions::NOHANG,
        )
        .unwrap();
        assert_eq!(pinned.unwrap().exit_status(), Some(17));
        child.finish().unwrap();
        child.finish().unwrap();
        assert_eq!(child.wait().unwrap().code(), Some(17));
        assert_eq!(
            rustix::process::waitid(
                rustix::process::WaitId::Pid(pid),
                rustix::process::WaitIdOptions::EXITED
                    | rustix::process::WaitIdOptions::NOWAIT
                    | rustix::process::WaitIdOptions::NOHANG
            )
            .unwrap_err(),
            Errno::CHILD
        );
    }

    #[test]
    fn failed_signal_can_retry_only_while_the_direct_child_pins_the_identifier() {
        let mut sent = false;
        let calls = std::cell::Cell::new(0);
        assert!(
            terminate_group(&mut sent, false, || {
                calls.set(calls.get() + 1);
                Err(Errno::INTR.into())
            })
            .is_err()
        );
        terminate_group(&mut sent, false, || {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .unwrap();
        terminate_group(&mut sent, false, || {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.get(), 2);
        let mut unsuccessful = false;
        terminate_group(&mut unsuccessful, true, || {
            calls.set(calls.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(
            calls.get(),
            2,
            "a reaped identifier must never be signalled"
        );
    }

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
