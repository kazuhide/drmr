//! Console supervision must fail before unsupported media work or argument handling.
#![cfg(unix)]

use drmr_core::native::ManagedChild;
use std::io::Read;
use std::process::{Command, Stdio};

const PREFIX: &str = "--internal-tool-broker-v1";
const MARKER: &str = "DRMR_TOOL_BROKER_V1";

#[test]
fn incomplete_or_invalid_context_is_refused_before_help_or_media_work() {
    for (arguments, marker) in [
        (vec![PREFIX, "run", "--help"], None),
        (vec![PREFIX, "run", "--help"], Some("invalid-parent")),
        (vec!["--help"], Some("1")),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
        command.args(arguments).env_remove(MARKER);
        if let Some(marker) = marker {
            command.env(MARKER, marker);
        }
        let result = command.output().unwrap();
        assert_eq!(result.status.code(), Some(2));
        assert!(result.stdout.is_empty());
        assert_eq!(
            result.stderr,
            b"drmr: the supervised job context could not be initialized\n"
        );
    }
}

#[test]
fn the_supervised_profile_refuses_other_commands_before_clap_help() {
    for command_name in ["batch", "serve", "convert", "doctor"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
        command
            .args([PREFIX, command_name, "--help"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let environment = std::env::vars_os().collect::<Vec<_>>();
        let mut child = ManagedChild::spawn_supervised_cli(command, &environment).unwrap();
        child.confirm_supervision().unwrap();
        let status = child.wait().unwrap();
        child.finish().unwrap();
        let mut stdout = String::new();
        let mut stderr = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut stdout)
            .unwrap();
        child
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut stderr)
            .unwrap();
        assert_eq!(status.code(), Some(2));
        assert!(stdout.is_empty());
        assert_eq!(
            stderr,
            "drmr: this command is unavailable in a supervised console job\n"
        );
    }
}
