//! The supported CLI surface and its usage errors.
use std::process::{Command, Output};

fn drmr(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn version_prints_the_workspace_version() {
    for args in [["version"], ["--version"]] {
        let output = drmr(&args);
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("drmr {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
}

#[test]
fn help_exposes_the_common_commands() {
    let output = drmr(&["--help"]);
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).unwrap();
    for command in [
        "doctor", "inspect", "analyze", "plan", "convert", "verify", "serve", "version",
    ] {
        assert!(help.contains(&format!("  {command}")), "{help}");
    }
    for command in [
        "run",
        "analyze-hls",
        "package-hls",
        "batch",
        "verify-snapshot",
        "canonical",
    ] {
        assert!(!help.contains(&format!("  {command}")), "{help}");
        assert_eq!(
            drmr(&[command, "--help"]).status.code(),
            Some(2),
            "{command}"
        );
    }
}

#[test]
fn planning_requires_a_media_input_and_rejects_catalog_only_flags() {
    assert_eq!(
        drmr(&["doctor", "--route", "prepared-dash"]).status.code(),
        Some(2)
    );
    for args in [
        vec!["plan", "--mpd", "manifest.mpd"],
        vec!["plan", "media.mp4", "--to", "clear", "--asset-id", "asset"],
        vec!["plan", "--to", "clear"],
    ] {
        assert_eq!(drmr(&args).status.code(), Some(2), "{args:?}");
    }
}
