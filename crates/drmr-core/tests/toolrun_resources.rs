//! Resource regressions and an opt-in, repeatable large-output capture measurement.

use drmr_core::events::NoSink;
use drmr_core::resources::{Claim, Limiter, Workload};
use drmr_core::toolrun::{RunOptions, Secrets, ToolRunner};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PYTHON: &str = if cfg!(windows) { "python" } else { "python3" };

#[test]
fn capture_measurement_worker() {
    let measurement = std::env::var("DRMR_CAPTURE_MEASURE_BYTES").ok();
    let amount: usize = measurement.as_deref().unwrap_or("65536").parse().unwrap();
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let started = Instant::now();
    let output = runner.run_tool(&[
        PYTHON.into(), "-c".into(),
        "import os,sys; n=int(sys.argv[1]); chunk=b'x'*65536; [(os.write(fd,chunk)) for fd in (1,2) for _ in range(n//len(chunk))]".into(),
        amount.to_string(),
    ], "capture-measurement", &RunOptions { timeout: Some(Duration::from_secs(30)), log_stdout: false, max_output_chars: 0, ..RunOptions::default() });
    match output {
        Ok(output) => {
            assert_eq!(output.stdout.len(), amount);
            assert_eq!(output.stderr.len(), amount);
            println!(
                "{}",
                serde_json::json!({"success": true, "stdout_bytes": output.stdout.len(), "stderr_bytes": output.stderr.len(), "elapsed_seconds": started.elapsed().as_secs_f64()})
            );
        }
        Err(error) => {
            assert!(
                measurement.is_some(),
                "normal bounded capture must succeed: {error}"
            );
            assert_eq!(
                error.kind, "output-limit",
                "the measurement must fail only because of the configured capture cap"
            );
            println!(
                "{}",
                serde_json::json!({"success": false, "error": error.to_string(), "elapsed_seconds": started.elapsed().as_secs_f64()})
            );
        }
    }
}

fn run_python(
    script: &str,
) -> Result<drmr_core::toolrun::Completed, drmr_core::toolrun::ToolError> {
    ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)).run_tool(
        &[PYTHON.into(), "-c".into(), script.into()],
        "resource-test",
        &RunOptions {
            timeout: Some(Duration::from_secs(5)),
            log_stdout: false,
            ..RunOptions::default()
        },
    )
}

#[test]
fn oversized_stdout_is_an_error_instead_of_partial_parseable_success() {
    let result = run_python("import sys; sys.stdout.write('x'*(20*1024*1024))");
    assert_eq!(result.unwrap_err().kind, "output-limit");
}

#[test]
fn stderr_has_its_own_limit_even_when_stdout_is_small() {
    let result =
        run_python("import sys; sys.stdout.write('{}'); sys.stderr.write('x'*(2*1024*1024))");
    assert_eq!(result.unwrap_err().kind, "output-limit");
}

#[test]
fn output_under_both_limits_remains_complete() {
    let result =
        run_python("import sys; sys.stdout.write('complete JSON'); sys.stderr.write('diagnostic')")
            .unwrap();
    assert_eq!(result.stdout, b"complete JSON");
    assert_eq!(result.stderr, b"diagnostic");
}

#[test]
fn exact_byte_boundaries_are_complete_and_one_extra_byte_fails() {
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let options = RunOptions {
        max_stdout_bytes: 3,
        max_stderr_bytes: 3,
        ..RunOptions::default()
    };
    let result = runner
        .run_tool(
            &[
                PYTHON.into(),
                "-c".into(),
                "import sys; sys.stdout.write('abc'); sys.stderr.write('def')".into(),
            ],
            "boundary",
            &options,
        )
        .unwrap();
    assert_eq!(result.stdout, b"abc");
    assert_eq!(result.stderr, b"def");
    let error = runner
        .run_tool(
            &[PYTHON.into(), "-c".into(), "print('abcd',end='')".into()],
            "boundary",
            &options,
        )
        .unwrap_err();
    assert_eq!(error.kind, "output-limit");
}

#[cfg(unix)]
#[test]
fn escaped_descendant_pipes_do_not_leave_blocked_reader_threads() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("child.pid");
    // This test measures reader cleanup after a real spawn, independently of other
    // tests' admission waits. Queued deadline behavior has a separate regression.
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
        .with_limiter(Arc::new(Limiter::new(1, 1024 * 1024 * 1024)));
    let started = Instant::now();
    let error = runner.run_tool(&[
        PYTHON.into(), "-c".into(),
        "import pathlib,subprocess,sys; child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(10)'],start_new_session=True); pathlib.Path(sys.argv[1]).write_text(str(child.pid))".into(),
        pid_file.to_string_lossy().into_owned(),
    ], "inherited-pipes", &RunOptions { timeout: Some(Duration::from_millis(200)), ..RunOptions::default() }).unwrap_err();
    let elapsed = started.elapsed();
    let pid = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
    let _ = rustix::process::kill_process(
        rustix::process::Pid::from_raw(pid).unwrap(),
        rustix::process::Signal::KILL,
    );
    assert_eq!(error.kind, "timeout");
    assert!(
        elapsed < Duration::from_secs(2),
        "the call owns and joins both reader threads"
    );
}

#[test]
fn inherited_pipes_get_a_close_deadline_even_without_a_command_timeout() {
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
        .with_limiter(Arc::new(Limiter::new(1, 1024 * 1024 * 1024)));
    let started = Instant::now();
    let error = runner.run_tool(&[
        PYTHON.into(), "-c".into(),
        "import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'])".into(),
    ], "inherited-pipes", &RunOptions::default()).unwrap_err();
    assert_eq!(error.kind, "pipe-timeout");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn runners_share_a_limit_across_real_child_processes_and_nested_pools() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("concurrency.sqlite");
    let init = run_python(&format!(
        "import sqlite3; c=sqlite3.connect({:?}); c.execute('create table counts(active integer, peak integer)'); c.execute('insert into counts values(0,0)'); c.commit()",
        database.to_string_lossy()
    )).unwrap();
    assert_eq!(init.returncode, Some(0));
    let workload = Workload::default();
    let claim = Claim::for_workload(workload, 16 * 1024 * 1024, 1024 * 1024);
    let limiter = Arc::new(Limiter::new(2, claim.memory_bytes * 2));
    let script = "import sqlite3,sys,time; c=sqlite3.connect(sys.argv[1],timeout=10); c.execute('begin immediate'); c.execute('update counts set active=active+1,peak=max(peak,active+1)'); c.commit(); time.sleep(0.08); c.execute('begin immediate'); c.execute('update counts set active=active-1'); c.commit()";
    std::thread::scope(|scope| {
        let outer = drmr_core::concurrency::Pool::new(scope, 2);
        let inner = Arc::new(drmr_core::concurrency::Pool::new(scope, 4));
        let mut parents = Vec::new();
        for _ in 0..2 {
            let database = &database;
            let inner = Arc::clone(&inner);
            let limiter = Arc::clone(&limiter);
            parents.push(outer.submit(move || {
                let mut children = Vec::new();
                for _ in 0..4 {
                    let limiter = Arc::clone(&limiter);
                    children.push(inner.submit(move || {
                        let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
                            .with_limiter(limiter);
                        let result = runner
                            .run_tool(
                                &[
                                    PYTHON.into(),
                                    "-c".into(),
                                    script.into(),
                                    database.to_string_lossy().into_owned(),
                                ],
                                "counted-child",
                                &RunOptions::default(),
                            )
                            .unwrap();
                        assert_eq!(result.returncode, Some(0));
                    }));
                }
                for child in children {
                    child.wait();
                    child.take().unwrap().into_value();
                }
            }));
        }
        for parent in parents {
            parent.wait();
            parent.take().unwrap().into_value();
        }
    });
    let result = run_python(&format!("import sqlite3; print(*sqlite3.connect({:?}).execute('select active,peak from counts').fetchone())", database.to_string_lossy())).unwrap();
    assert_eq!(String::from_utf8(result.stdout).unwrap().trim(), "0 2");
}

#[test]
fn a_larger_capture_override_is_charged_before_spawning() {
    let normal = Claim::for_workload(Workload::default(), 16 * 1024 * 1024, 1024 * 1024);
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
        .with_limiter(Arc::new(Limiter::new(1, normal.memory_bytes)));
    let argv = [PYTHON.into(), "-c".into(), "print('complete')".into()];
    assert_eq!(
        runner
            .run_tool(&argv, "metadata", &RunOptions::default())
            .unwrap()
            .returncode,
        Some(0)
    );
    assert_eq!(
        runner
            .run_tool(
                &argv,
                "packet-probe",
                &RunOptions {
                    max_stdout_bytes: 32 * 1024 * 1024,
                    ..RunOptions::default()
                }
            )
            .unwrap_err()
            .kind,
        "resource-budget"
    );
    assert_eq!(
        runner
            .run_tool(&argv, "metadata", &RunOptions::default())
            .unwrap()
            .returncode,
        Some(0)
    );
}

#[test]
fn admission_waits_obey_the_call_deadline_and_cancel_without_spawning() {
    use drmr_core::concurrency::Stop;
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("must-not-exist");
    let claim = Claim::for_workload(Workload::default(), 16 * 1024 * 1024, 1024 * 1024);
    let limiter = Arc::new(Limiter::new(1, claim.memory_bytes));
    let _held = limiter.acquire(claim, None, None).unwrap();
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)).with_limiter(limiter);
    let argv = [
        PYTHON.into(),
        "-c".into(),
        "import pathlib,sys; pathlib.Path(sys.argv[1]).write_text('spawned')".into(),
        marker.to_string_lossy().into_owned(),
    ];
    let started = Instant::now();
    assert_eq!(
        runner
            .run_tool(
                &argv,
                "waiting",
                &RunOptions {
                    timeout: Some(Duration::from_millis(40)),
                    ..RunOptions::default()
                }
            )
            .unwrap_err()
            .kind,
        "timeout"
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let stop = Stop::new();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            runner.run_tool(
                &argv,
                "waiting",
                &RunOptions {
                    stop: Some(&stop),
                    ..RunOptions::default()
                },
            )
        });
        stop.set();
        assert_eq!(waiter.join().unwrap().unwrap_err().kind, "cancelled");
    });
    assert!(!marker.exists());
}

#[test]
fn a_capture_failure_returns_its_reservation_for_the_next_child() {
    let claim = Claim::for_workload(Workload::default(), 16 * 1024 * 1024, 1024 * 1024);
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
        .with_limiter(Arc::new(Limiter::new(1, claim.memory_bytes)));
    assert_eq!(
        runner
            .run_tool(
                &[
                    PYTHON.into(),
                    "-c".into(),
                    "print('x'*(20*1024*1024))".into()
                ],
                "oversized",
                &RunOptions::default()
            )
            .unwrap_err()
            .kind,
        "output-limit"
    );
    assert_eq!(
        runner
            .run_tool(
                &[PYTHON.into(), "-c".into(), "print('complete')".into()],
                "next-child",
                &RunOptions {
                    timeout: Some(Duration::from_secs(1)),
                    ..RunOptions::default()
                }
            )
            .unwrap()
            .stdout,
        b"complete\n"
    );
}

#[cfg(unix)]
#[test]
fn captured_tools_stay_in_the_asset_worker_supervisor_group() {
    use std::os::unix::process::CommandExt;
    const CHILD: &str = "DRMR_TEST_CAPTURE_WORKER_GROUP";
    if std::env::var_os(CHILD).is_some() {
        // Empty configuration returns a classified failure result; entry still establishes
        // the real worker context before configuration handling.
        assert_eq!(drmr_core::batch::worker_main(), 0);
        assert!(drmr_core::native::in_asset_worker());
        let result = run_python("import os; print(os.getpgrp())").unwrap();
        let group: i32 = String::from_utf8(result.stdout)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(group, rustix::process::getpgrp().as_raw_nonzero().get());
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "captured_tools_stay_in_the_asset_worker_supervisor_group",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .stdin(std::process::Stdio::null())
        .process_group(0)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
