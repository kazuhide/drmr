use drmr_core::events::NoSink;
use drmr_core::toolrun::{Completed, RunOptions, Secrets, ToolRunner};
use std::sync::Arc;
use std::time::Duration;

fn run(arguments: &[&str]) -> Completed {
    run_program(
        std::path::Path::new(env!("CARGO_BIN_EXE_drmr-desktop")),
        arguments,
    )
}

fn run_program(program: &std::path::Path, arguments: &[&str]) -> Completed {
    let argv: Vec<_> = std::iter::once(program.to_str().unwrap().to_owned())
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

#[test]
fn bundle_verification_requires_its_compiled_cli_pair_without_starting_the_host() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let host_name = format!("drmr-desktop{}", std::env::consts::EXE_SUFFIX);
    let cli_name = format!("drmr{}", std::env::consts::EXE_SUFFIX);
    let host = root.path().join(host_name);
    std::fs::copy(env!("CARGO_BIN_EXE_drmr-desktop"), &host).unwrap();
    let staged = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(format!(
            "drmr-{}{}",
            env!("DRMR_BUILD_TARGET"),
            std::env::consts::EXE_SUFFIX
        ));
    let cli = root.path().join(cli_name);
    std::fs::copy(staged, &cli).unwrap();
    let check = || run_program(&host, &["--verify-bundle"]);
    let valid = check();
    assert_eq!(valid.returncode, Some(0));
    assert_eq!(valid.stdout, b"bundle-cli-verified\n");
    std::fs::write(&cli, b"different CLI bytes").unwrap();
    let invalid = check();
    assert_eq!(invalid.returncode, Some(1));
    assert_eq!(invalid.stdout, [] as [u8; 0]);
    assert_eq!(invalid.stderr, b"drmr-desktop: bundled-cli-mismatch\n");
}

#[test]
fn bundle_verification_refuses_path_arguments() {
    let output = run(&["--verify-bundle", "SYNTHETIC-PRIVATE-VALUE"]);
    assert_eq!(output.returncode, Some(2));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        output.stderr,
        b"drmr-desktop: unsupported-arguments; use --help\n"
    );
}
