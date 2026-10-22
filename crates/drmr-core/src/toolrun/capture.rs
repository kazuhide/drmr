//! Hard byte capture limits and owned pipe-reader lifetime for one managed process tree.

use super::{Completed, Failure, MAX_CAPTURE_BYTES, RunOptions, exit_code, seconds_text};
use crate::native::{CapturedStderr, CapturedStdout, ManagedChild};
use std::ffi::OsStr;
use std::io::{self, Read};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};

const OUT_DONE: u8 = 1;
const ERR_DONE: u8 = 2;
const EXCEEDED: u8 = 4;
const READ_ERROR: u8 = 8;
const STREAM_ERROR: u8 = 16;
const POLL: Duration = Duration::from_millis(1);
const PIPE_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
struct State {
    flags: AtomicU8,
    stop: AtomicBool,
}

trait PipeReader: Read {
    fn wait_readable(&self) -> io::Result<()>;
}

#[cfg(unix)]
impl<T: Read + std::os::fd::AsFd> PipeReader for T {
    fn wait_readable(&self) -> io::Result<()> {
        use rustix::event::{PollFd, PollFlags, Timespec, poll};
        let mut descriptors = [PollFd::new(self, PollFlags::IN)];
        let timeout = Timespec::try_from(POLL).map_err(io::Error::other)?;
        match poll(&mut descriptors, Some(&timeout)) {
            Ok(_) if descriptors[0].revents().contains(PollFlags::NVAL) => {
                Err(io::Error::other("captured pipe became unavailable"))
            }
            // EOF and pipe errors are resolved by the next nonblocking read.
            Ok(_) | Err(rustix::io::Errno::INTR) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

#[cfg(not(unix))]
impl<T: Read> PipeReader for T {
    fn wait_readable(&self) -> io::Result<()> {
        std::thread::sleep(POLL);
        Ok(())
    }
}

fn read_bounded(mut pipe: impl PipeReader, limit: usize, state: &State, done: u8) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    while !state.stop.load(Ordering::Acquire) {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(amount) => {
                let keep = amount.min(limit.saturating_sub(bytes.len()));
                let needed = bytes.len() + keep;
                if bytes.capacity() < needed {
                    let capacity = bytes
                        .capacity()
                        .max(chunk.len())
                        .saturating_mul(2)
                        .min(limit)
                        .max(needed);
                    bytes.reserve_exact(capacity - bytes.len());
                }
                bytes.extend_from_slice(&chunk[..keep]);
                if keep != amount {
                    state.flags.fetch_or(EXCEEDED, Ordering::Release);
                }
                // Continue draining without retaining bytes until the controller kills the tree.
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if pipe.wait_readable().is_err() {
                    state.flags.fetch_or(READ_ERROR, Ordering::Release);
                    break;
                }
            }
            Err(_) => {
                state.flags.fetch_or(READ_ERROR, Ordering::Release);
                break;
            }
        }
    }
    state.flags.fetch_or(done, Ordering::Release);
    bytes
}

fn read_streamed(
    mut pipe: impl PipeReader,
    state: &State,
    consume: &mut impl FnMut(&[u8]) -> Result<(), ()>,
) -> Vec<u8> {
    let mut chunk = [0_u8; 8192];
    while !state.stop.load(Ordering::Acquire) {
        match pipe.read(&mut chunk) {
            Ok(0) => break,
            Ok(amount) => {
                if consume(&chunk[..amount]).is_err() {
                    state.flags.fetch_or(STREAM_ERROR, Ordering::Release);
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if pipe.wait_readable().is_err() {
                    state.flags.fetch_or(READ_ERROR, Ordering::Release);
                    break;
                }
            }
            Err(_) => {
                state.flags.fetch_or(READ_ERROR, Ordering::Release);
                break;
            }
        }
    }
    state.flags.fetch_or(OUT_DONE, Ordering::Release);
    Vec::new()
}

fn failure(kind: &'static str, text: impl Into<String>) -> Failure {
    Failure {
        kind,
        text: text.into(),
    }
}

fn control(
    child: &mut ManagedChild,
    state: &State,
    timeout: Option<Duration>,
    stop: Option<&crate::concurrency::Stop>,
) -> Result<ExitStatus, Failure> {
    let deadline = timeout.map(|limit| Instant::now() + limit);
    let mut pipe_deadline = None;
    let mut status = None;
    loop {
        if stop.is_some_and(crate::concurrency::Stop::is_set) {
            return Err(failure("cancelled", "tool execution cancelled"));
        }
        let flags = state.flags.load(Ordering::Acquire);
        if flags & EXCEEDED != 0 {
            return Err(failure(
                "output-limit",
                "captured output exceeded its byte limit",
            ));
        }
        if flags & STREAM_ERROR != 0 {
            return Err(failure("stream-invalid", "streamed output is invalid"));
        }
        if flags & READ_ERROR != 0 {
            return Err(failure("unavailable", "could not read captured output"));
        }
        if status.is_none() {
            status = child
                .try_wait()
                .map_err(|error| failure("unavailable", format!("could not run: {error}")))?;
            if status.is_some() {
                pipe_deadline = Some(Instant::now() + PIPE_GRACE);
            }
        }
        if let Some(status) = status
            && flags & (OUT_DONE | ERR_DONE) == OUT_DONE | ERR_DONE
        {
            return Ok(status);
        }
        if deadline.is_some_and(|end| Instant::now() >= end) {
            return Err(failure(
                "timeout",
                format!("timed out after {}s", seconds_text(timeout.unwrap())),
            ));
        }
        if pipe_deadline.is_some_and(|end| Instant::now() >= end) {
            return Err(failure(
                "pipe-timeout",
                "output pipes remained open after the tool exited",
            ));
        }
        std::thread::sleep(POLL);
    }
}

pub(super) fn execute(
    argv: &[String],
    options: &RunOptions<'_>,
    path: Option<&OsStr>,
    lease: Option<&super::cleanup::Lease<'_>>,
) -> Result<Completed, Failure> {
    execute_owned_with_reader(
        argv,
        options,
        path,
        lease,
        ManagedChild::finish,
        |stdout, limit, state| read_bounded(stdout, limit, state, OUT_DONE),
    )
}

pub(super) fn execute_streamed(
    argv: &[String],
    options: &RunOptions<'_>,
    path: Option<&OsStr>,
    lease: Option<&super::cleanup::Lease<'_>>,
    mut consume: impl FnMut(&[u8]) -> Result<(), ()> + Send,
) -> Result<Completed, Failure> {
    execute_owned_with_reader(
        argv,
        options,
        path,
        lease,
        ManagedChild::finish,
        move |stdout, _, state| read_streamed(stdout, state, &mut consume),
    )
}

#[cfg(test)]
pub(super) fn execute_injected_cleanup(
    argv: &[String],
    options: &RunOptions<'_>,
    path: Option<&OsStr>,
    lease: Option<&super::cleanup::Lease<'_>>,
    cleanup: impl FnMut(&mut ManagedChild) -> io::Result<()>,
) -> Result<Completed, Failure> {
    execute_owned_with_reader(
        argv,
        options,
        path,
        lease,
        cleanup,
        |stdout, limit, state| read_bounded(stdout, limit, state, OUT_DONE),
    )
}

#[cfg(test)]
fn execute_with_cleanup(
    argv: &[String],
    options: &RunOptions<'_>,
    path: Option<&OsStr>,
    cleanup: impl FnMut(&mut ManagedChild) -> io::Result<()>,
) -> Result<Completed, Failure> {
    execute_with_reader(argv, options, path, cleanup, |stdout, limit, state| {
        read_bounded(stdout, limit, state, OUT_DONE)
    })
}

#[cfg(test)]
fn execute_with_reader(
    argv: &[String],
    options: &RunOptions<'_>,
    path: Option<&OsStr>,
    cleanup: impl FnMut(&mut ManagedChild) -> io::Result<()>,
    read_stdout: impl FnOnce(CapturedStdout, usize, &State) -> Vec<u8> + Send,
) -> Result<Completed, Failure> {
    execute_owned_with_reader(argv, options, path, None, cleanup, read_stdout)
}

fn execute_owned_with_reader(
    argv: &[String],
    options: &RunOptions<'_>,
    path: Option<&OsStr>,
    lease: Option<&super::cleanup::Lease<'_>>,
    cleanup: impl FnMut(&mut ManagedChild) -> io::Result<()>,
    read_stdout: impl FnOnce(CapturedStdout, usize, &State) -> Vec<u8> + Send,
) -> Result<Completed, Failure> {
    if options.stop.is_some_and(crate::concurrency::Stop::is_set) {
        return Err(failure("cancelled", "tool execution cancelled"));
    }
    let Some((program, arguments)) = argv.split_first() else {
        return Err(failure("unavailable", "could not run: empty command"));
    };
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    // Each tool retains an owned process group on Unix or a nested job on Windows.
    let mut child = ManagedChild::spawn_with_io(
        command,
        true,
        crate::native::ManagedIo {
            stdin: crate::native::ManagedStdio::Null,
            stdout: crate::native::ManagedStdio::Piped,
            stderr: crate::native::ManagedStdio::Piped,
        },
    )
    .map_err(|error| failure("unavailable", format!("could not run: {error}")))?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    let result = capture_pipes(&mut child, stdout, stderr, options, cleanup, read_stdout);
    if result
        .as_ref()
        .is_err_and(|error| error.kind == "cleanup-unconfirmed")
        && let Some(lease) = lease
    {
        // Reader scopes are joined before transferring the live native owner.
        lease.retain(child);
    }
    result
}

fn capture_pipes(
    child: &mut ManagedChild,
    stdout: std::process::ChildStdout,
    stderr: std::process::ChildStderr,
    options: &RunOptions<'_>,
    mut cleanup: impl FnMut(&mut ManagedChild) -> io::Result<()>,
    read_stdout: impl FnOnce(CapturedStdout, usize, &State) -> Vec<u8> + Send,
) -> Result<Completed, Failure> {
    let pipes = crate::native::prepare_stdout(stdout)
        .and_then(|stdout| crate::native::prepare_stderr(stderr).map(|stderr| (stdout, stderr)));
    let (stdout, stderr) = match pipes {
        Ok(pipes) => pipes,
        Err(error) => {
            confirm_cleanup(child, &mut cleanup)?;
            return Err(failure(
                "unavailable",
                format!("could not configure captured output: {error}"),
            ));
        }
    };
    capture_child(child, stdout, stderr, options, cleanup, read_stdout)
}

fn capture_child(
    child: &mut ManagedChild,
    stdout: CapturedStdout,
    stderr: CapturedStderr,
    options: &RunOptions<'_>,
    mut cleanup: impl FnMut(&mut ManagedChild) -> io::Result<()>,
    read_stdout: impl FnOnce(CapturedStdout, usize, &State) -> Vec<u8> + Send,
) -> Result<Completed, Failure> {
    let state = Arc::new(State::default());
    std::thread::scope(|scope| {
        let out_state = Arc::clone(&state);
        let stdout_limit = options.max_stdout_bytes.min(MAX_CAPTURE_BYTES);
        let output = std::thread::Builder::new()
            .name("tool-stdout".into())
            .spawn_scoped(scope, move || read_stdout(stdout, stdout_limit, &out_state));
        let output = match output {
            Ok(reader) => reader,
            Err(error) => {
                confirm_cleanup(child, &mut cleanup)?;
                return Err(failure(
                    "unavailable",
                    format!("could not start captured-output reader: {error}"),
                ));
            }
        };
        let err_state = Arc::clone(&state);
        let stderr_limit = options.max_stderr_bytes.min(MAX_CAPTURE_BYTES);
        let error_output = match std::thread::Builder::new()
            .name("tool-stderr".into())
            .spawn_scoped(scope, move || {
                read_bounded(stderr, stderr_limit, &err_state, ERR_DONE)
            }) {
            Ok(reader) => reader,
            Err(error) => {
                let confirmed = confirm_cleanup(child, &mut cleanup);
                state.stop.store(true, Ordering::Release);
                let _ = output.join();
                confirmed?;
                return Err(failure(
                    "unavailable",
                    format!("could not start captured-output reader: {error}"),
                ));
            }
        };
        let result = control(child, &state, options.timeout, options.stop);
        let confirmed = confirm_cleanup(child, &mut cleanup);
        state.stop.store(true, Ordering::Release);
        let stdout = output
            .join()
            .map_err(|_| failure("unavailable", "captured-output reader panicked"));
        let stderr = error_output
            .join()
            .map_err(|_| failure("unavailable", "captured-output reader panicked"));
        confirmed?;
        let status = result?;
        Ok(Completed {
            returncode: exit_code(status),
            stdout: stdout?,
            stderr: stderr?,
        })
    })
}

fn confirm_cleanup(
    child: &mut ManagedChild,
    cleanup: &mut impl FnMut(&mut ManagedChild) -> io::Result<()>,
) -> Result<(), Failure> {
    cleanup(child).map_err(|_| {
        failure(
            "cleanup-unconfirmed",
            "process tree cleanup could not be confirmed",
        )
    })
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn response(root: &std::path::Path, spec: &drmr_test_support::ToolSpec) -> Vec<String> {
        let tool = drmr_test_support::install(&root.join("response"), spec).unwrap();
        vec![tool.to_str().unwrap().into()]
    }

    fn capture_harness(name: &str) -> bool {
        const HARNESS: &str = "DRMR_TEST_CAPTURE_JOIN_CHILD";
        if std::env::var_os(HARNESS).as_deref() != Some(std::ffi::OsStr::new(name)) {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args(["--exact", name, "--nocapture"])
                .env(HARNESS, name)
                .stdin(Stdio::null());
            // A blocking-reader regression must fail the parent test, never hang
            // the entire suite or abandon its child harness and fixture process.
            crate::test_harness::run(command, name, Duration::from_secs(20));
            return true;
        }
        false
    }

    #[cfg(unix)]
    #[test]
    fn readers_join_while_parent_owned_writers_remain_open() {
        use std::io::Write as _;
        #[cfg(unix)]
        type PipeHandle = std::os::fd::OwnedFd;
        #[cfg(windows)]
        type PipeHandle = std::os::windows::io::OwnedHandle;

        if capture_harness(
            "toolrun::capture::lifecycle_tests::readers_join_while_parent_owned_writers_remain_open",
        ) {
            return;
        }
        for (case, expected) in [
            ("timeout", "timeout"),
            ("cancelled", "cancelled"),
            ("cleanup", "cleanup-unconfirmed"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let ready = root.path().join("ready");
            let argv = response(
                root.path(),
                &drmr_test_support::ToolSpec::default()
                    .with("ready_file", ready.to_str().unwrap())
                    .with(
                        "release_file",
                        root.path().join("release").to_str().unwrap(),
                    ),
            );
            let mut command = Command::new(&argv[0]);
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let mut child = ManagedChild::spawn(command, true).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !ready.is_file() {
                assert!(Instant::now() < deadline, "fixture did not become ready");
                std::thread::sleep(POLL);
            }
            let (out_read, mut out_write) = std::io::pipe().unwrap();
            let (err_read, mut err_write) = std::io::pipe().unwrap();
            let stdout = crate::native::prepare_stdout(std::process::ChildStdout::from(
                PipeHandle::from(out_read),
            ))
            .unwrap();
            let stderr = crate::native::prepare_stderr(std::process::ChildStderr::from(
                PipeHandle::from(err_read),
            ))
            .unwrap();
            let stop = crate::concurrency::Stop::new();
            if case == "cancelled" {
                stop.set();
            }
            let options = RunOptions {
                timeout: Some(Duration::ZERO),
                stop: Some(&stop),
                ..RunOptions::default()
            };
            let started = Instant::now();
            let mut cleanup_attempts = 0;
            let error = capture_child(
                &mut child,
                stdout,
                stderr,
                &options,
                |child| {
                    cleanup_attempts += 1;
                    child.finish()?;
                    if case == "cleanup" {
                        Err(io::Error::other("private fixture detail"))
                    } else {
                        Ok(())
                    }
                },
                |stdout, limit, state| read_bounded(stdout, limit, state, OUT_DONE),
            )
            .unwrap_err();
            // The reader scopes have joined before capture_child returns. These
            // writers were NEVER passed to the child and remain open throughout.
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "reader join exceeded its bound"
            );
            assert_eq!(error.kind, expected);
            assert_eq!(cleanup_attempts, 1);
            assert!(child.try_wait().unwrap().is_some());
            assert_eq!(
                out_write.write(b"x").unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
            assert_eq!(
                err_write.write(b"x").unwrap_err().kind(),
                io::ErrorKind::BrokenPipe
            );
        }
    }

    #[test]
    fn actual_managed_stdio_readers_join_when_cleanup_cannot_close_live_writers() {
        if capture_harness(
            "toolrun::capture::lifecycle_tests::actual_managed_stdio_readers_join_when_cleanup_cannot_close_live_writers",
        ) {
            return;
        }
        for cancelled in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let ready = root.path().join("ready");
            let argv = response(
                root.path(),
                &drmr_test_support::ToolSpec::default()
                    .with("ready_file", ready.to_str().unwrap())
                    .with(
                        "release_file",
                        root.path().join("release").to_str().unwrap(),
                    ),
            );
            let mut command = Command::new(&argv[0]);
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = ManagedChild::spawn(command, true).unwrap();
            let stdout = crate::native::prepare_stdout(child.stdout.take().unwrap()).unwrap();
            let stderr = crate::native::prepare_stderr(child.stderr.take().unwrap()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !ready.is_file() {
                assert!(
                    Instant::now() < deadline,
                    "actual stdio fixture did not become ready"
                );
                std::thread::sleep(POLL);
            }
            let stop = crate::concurrency::Stop::new();
            if cancelled {
                stop.set();
            }
            let options = RunOptions {
                timeout: Some(Duration::ZERO),
                stop: Some(&stop),
                ..RunOptions::default()
            };
            let started = Instant::now();
            let mut attempts = 0;
            let result = capture_child(
                &mut child,
                stdout,
                stderr,
                &options,
                |_| {
                    attempts += 1;
                    Err(io::Error::other("injected termination failure"))
                },
                |stdout, limit, state| read_bounded(stdout, limit, state, OUT_DONE),
            );
            let elapsed = started.elapsed();
            // The actual process and BOTH actual Stdio::piped writers still exist
            // when the capture scopes have returned and joined. Keep the retained
            // native owner and finish it explicitly before making assertions.
            let still_running = child.try_wait().unwrap().is_none();
            child.finish().unwrap();
            assert!(still_running);
            assert!(elapsed < Duration::from_secs(5));
            assert_eq!(result.unwrap_err().kind, "cleanup-unconfirmed");
            assert_eq!(attempts, 1);
        }
    }

    #[cfg(windows)]
    #[test]
    fn pipe_preparation_failure_confirms_cleanup_before_returning_its_error() {
        use std::os::windows::io::OwnedHandle;
        for uncertain in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let argv = response(
                root.path(),
                &drmr_test_support::ToolSpec {
                    sleep_ms: 60_000,
                    ..Default::default()
                },
            );
            let mut command = Command::new(&argv[0]);
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let mut child = ManagedChild::spawn(command, true).unwrap();
            drop(child.stdout.take());
            let invalid =
                std::process::ChildStdout::from(OwnedHandle::from(tempfile::tempfile().unwrap()));
            let stderr = child.stderr.take().unwrap();
            let mut attempts = 0;
            let result = capture_pipes(
                &mut child,
                invalid,
                stderr,
                &RunOptions::default(),
                |child| {
                    attempts += 1;
                    child.finish()?;
                    if uncertain {
                        Err(io::Error::other("private cleanup error"))
                    } else {
                        Ok(())
                    }
                },
                |_, _, _| panic!("a reader started after pipe preparation failed"),
            );
            assert_eq!(attempts, 1);
            assert!(child.try_wait().unwrap().is_some());
            assert_eq!(
                result.unwrap_err().kind,
                if uncertain {
                    "cleanup-unconfirmed"
                } else {
                    "unavailable"
                }
            );
        }
    }

    #[test]
    fn streamed_stdout_exceeds_capture_limits_without_retaining_the_payload() {
        let root = tempfile::tempdir().unwrap();
        let argv = response(
            root.path(),
            &drmr_test_support::ToolSpec::default().with("stdout_repeat", "40000000"),
        );
        let options = RunOptions {
            max_stdout_bytes: 16 * 1024,
            timeout: Some(Duration::from_secs(20)),
            ..RunOptions::default()
        };
        let mut total = 0;
        let completed = execute_streamed(&argv, &options, None, None, |chunk| {
            assert!(chunk.len() <= 8192);
            total += chunk.len();
            Ok(())
        })
        .unwrap_or_else(|error| {
            panic!(
                "streamed response failed: kind={}, bytes={total}",
                error.kind
            )
        });
        assert_eq!(total, 40_000_000);
        assert_eq!(completed.returncode, Some(0));
        assert_eq!(completed.stdout.len(), 0);
    }

    #[test]
    fn streamed_parser_failure_stderr_overflow_and_timeout_do_not_complete() {
        for (case, spec, timeout, expected) in [
            (
                "invalid",
                drmr_test_support::ToolSpec {
                    stdout: b"private raw packet".to_vec(),
                    ..Default::default()
                },
                None,
                "stream-invalid",
            ),
            (
                "stderr",
                drmr_test_support::ToolSpec::default().with("stderr_repeat", "5000"),
                None,
                "output-limit",
            ),
            (
                "timeout",
                drmr_test_support::ToolSpec {
                    sleep_ms: 60_000,
                    ..Default::default()
                },
                Some(Duration::ZERO),
                "timeout",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let argv = response(root.path(), &spec);
            let options = RunOptions {
                max_stderr_bytes: 10,
                timeout,
                ..RunOptions::default()
            };
            let error = execute_streamed(&argv, &options, None, None, |_| {
                if case == "invalid" { Err(()) } else { Ok(()) }
            })
            .unwrap_err();
            assert_eq!(error.kind, expected);
            assert!(!error.text.contains("private raw packet"));
        }
    }

    #[test]
    fn streamed_active_cancellation_and_cleanup_error_preserve_failure_precedence() {
        let root = tempfile::tempdir().unwrap();
        let argv = response(
            root.path(),
            &drmr_test_support::ToolSpec::default().with("stdout_repeat", "40000000"),
        );
        let stop = crate::concurrency::Stop::new();
        let options = RunOptions {
            stop: Some(&stop),
            ..RunOptions::default()
        };
        let error = execute_streamed(&argv, &options, None, None, |_| {
            stop.set();
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.kind, "cancelled");
        let mut attempts = 0;
        let error = execute_with_reader(
            &argv,
            &RunOptions::default(),
            None,
            |child| {
                attempts += 1;
                child.finish()?;
                Err(io::Error::other("private cleanup error"))
            },
            |stdout, _, state| read_streamed(stdout, state, &mut |_| Err(())),
        )
        .unwrap_err();
        assert_eq!(error.kind, "cleanup-unconfirmed");
        assert_eq!(attempts, 1);
    }

    #[test]
    fn cleanup_failure_overrides_active_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let ready = root.path().join("ready");
        let tool = drmr_test_support::program_path(&root.path().join("response"));
        drmr_test_support::install(
            &tool,
            &drmr_test_support::ToolSpec::default()
                .with("ready_file", ready.to_str().unwrap())
                .with(
                    "release_file",
                    root.path().join("release").to_str().unwrap(),
                ),
        )
        .unwrap();
        let argv = vec![tool.to_str().unwrap().to_string()];
        let stop = crate::concurrency::Stop::new();
        let options = RunOptions {
            stop: Some(&stop),
            timeout: Some(Duration::from_secs(20)),
            ..RunOptions::default()
        };
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let deadline = Instant::now() + Duration::from_secs(15);
                while !ready.is_file() {
                    assert!(Instant::now() < deadline, "tool did not become ready");
                    std::thread::sleep(POLL);
                }
                stop.set();
            });
            let mut attempts = 0;
            let error = execute_with_cleanup(&argv, &options, None, |child| {
                attempts += 1;
                child.finish()?;
                Err(io::Error::other("private cleanup diagnostic"))
            })
            .expect_err("cancellation must not conceal unconfirmed cleanup");
            assert!(stop.is_set());
            assert_eq!(error.kind, "cleanup-unconfirmed");
            assert_eq!(error.text, "process tree cleanup could not be confirmed");
            assert_eq!(attempts, 1);
        });
    }

    #[test]
    fn cleanup_failure_overrides_completed_and_timed_out_tools() {
        let root = tempfile::tempdir().unwrap();
        let tool = drmr_test_support::program_path(&root.path().join("response"));
        drmr_test_support::install(&tool, &drmr_test_support::ToolSpec::default()).unwrap();
        let argv = vec![tool.to_str().unwrap().to_string()];
        for timeout in [None, Some(Duration::ZERO)] {
            let options = RunOptions {
                timeout,
                ..RunOptions::default()
            };
            let mut attempts = 0;
            let error = execute_with_cleanup(&argv, &options, None, |child| {
                attempts += 1;
                child.finish()?;
                Err(io::Error::other("injected cleanup uncertainty"))
            })
            .expect_err("cleanup failure must prevent a completed result");
            assert_eq!(error.kind, "cleanup-unconfirmed");
            assert_eq!(error.text, "process tree cleanup could not be confirmed");
            assert_eq!(attempts, 1);
        }
    }
}
