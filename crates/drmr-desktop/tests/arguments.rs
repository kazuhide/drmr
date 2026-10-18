use drmr_core::events::NoSink;
use drmr_core::toolrun::{Completed, RunOptions, Secrets, ToolRunner};
use std::sync::Arc;
use std::time::Duration;

fn run(arguments: &[&str]) -> Completed {
    let argv: Vec<_> = std::iter::once(env!("CARGO_BIN_EXE_drmr-desktop").to_owned())
        .chain(arguments.iter().map(|argument| (*argument).to_owned()))
        .collect();
    ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
        .run_tool(
            &argv,
            "desktop-arguments",
            &RunOptions {
                timeout: Some(Duration::from_secs(5)),
                max_stdout_bytes: 4096,
                max_stderr_bytes: 4096,
                log_stdout: false,
                ..RunOptions::default()
            },
        )
        .unwrap()
}

#[test]
fn version_exits_without_a_native_window_or_server() {
    let output = run(&["--version"]);
    assert_eq!(output.returncode, Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        format!("drmr-desktop {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(output.stderr, [] as [u8; 0]);
}

#[test]
fn help_exits_without_a_native_window_or_server() {
    let output = run(&["--help"]);
    assert_eq!(output.returncode, Some(0));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("Use the File menu")
    );
    assert_eq!(output.stderr, [] as [u8; 0]);
}

#[test]
fn unsupported_arguments_refuse_without_echoing_input() {
    let output = run(&["--not-supported", "SYNTHETIC-PRIVATE-VALUE"]);
    assert_eq!(output.returncode, Some(2));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "drmr-desktop: unsupported-arguments; use --help\n"
    );
}
