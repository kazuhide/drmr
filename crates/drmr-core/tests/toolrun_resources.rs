//! Resource regressions and an opt-in, repeatable large-output capture measurement.

use drmr_core::events::NoSink;
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
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
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
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let started = Instant::now();
    let error = runner.run_tool(&[
        PYTHON.into(), "-c".into(),
        "import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time;time.sleep(30)'])".into(),
    ], "inherited-pipes", &RunOptions::default()).unwrap_err();
    assert_eq!(error.kind, "pipe-timeout");
    assert!(started.elapsed() < Duration::from_secs(5));
}
