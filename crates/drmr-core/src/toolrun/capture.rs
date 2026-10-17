//! Hard byte capture limits and owned pipe-reader lifetime for one managed process tree.

use super::{Completed, Failure, MAX_CAPTURE_BYTES, RunOptions, exit_code, seconds_text};
use crate::native::ManagedChild;
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
const POLL: Duration = Duration::from_millis(1);
const PIPE_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
struct State {
    flags: AtomicU8,
    stop: AtomicBool,
}

#[cfg(unix)]
fn prepare_pipe(pipe: &impl std::os::fd::AsFd) -> io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(pipe)?;
    rustix::fs::fcntl_setfl(pipe, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}

fn read_bounded(mut pipe: impl Read, limit: usize, state: &State, done: u8) -> Vec<u8> {
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
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(POLL),
            Err(_) => {
                state.flags.fetch_or(READ_ERROR, Ordering::Release);
                break;
            }
        }
    }
    state.flags.fetch_or(done, Ordering::Release);
    bytes
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
) -> Result<Completed, Failure> {
    execute_with_cleanup(argv, options, path, ManagedChild::finish)
}

fn execute_with_cleanup(
    argv: &[String],
    options: &RunOptions<'_>,
    path: Option<&OsStr>,
    mut cleanup: impl FnMut(&mut ManagedChild) -> io::Result<()>,
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
    // Asset workers inherit one supervisor-owned Unix group. Creating a subgroup here
    // would let media tools survive a hard worker kill; Windows retains nested jobs.
    let mut child = ManagedChild::spawn(command, !crate::native::in_asset_worker())
        .map_err(|error| failure("unavailable", format!("could not run: {error}")))?;
    let stdout = child.stdout.take().expect("stdout is piped");
    let stderr = child.stderr.take().expect("stderr is piped");
    // Windows uses the managed job to close inherited writers. Unix pipes are
    // nonblocking so a descendant outside the process group cannot retain a reader.
    #[cfg(unix)]
    if let Err(error) = prepare_pipe(&stdout).and_then(|()| prepare_pipe(&stderr)) {
        confirm_cleanup(&mut child, &mut cleanup)?;
        return Err(failure(
            "unavailable",
            format!("could not configure captured output: {error}"),
        ));
    }
    let state = Arc::new(State::default());
    std::thread::scope(|scope| {
        let out_state = Arc::clone(&state);
        let stdout_limit = options.max_stdout_bytes.min(MAX_CAPTURE_BYTES);
        let output = std::thread::Builder::new()
            .name("tool-stdout".into())
            .spawn_scoped(scope, move || {
                read_bounded(stdout, stdout_limit, &out_state, OUT_DONE)
            });
        let output = match output {
            Ok(reader) => reader,
            Err(error) => {
                confirm_cleanup(&mut child, &mut cleanup)?;
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
                let confirmed = confirm_cleanup(&mut child, &mut cleanup);
                state.stop.store(true, Ordering::Release);
                let _ = output.join();
                confirmed?;
                return Err(failure(
                    "unavailable",
                    format!("could not start captured-output reader: {error}"),
                ));
            }
        };
        let result = control(&mut child, &state, options.timeout, options.stop);
        let confirmed = confirm_cleanup(&mut child, &mut cleanup);
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
