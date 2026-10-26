//! Source directories are automatic; DASH addressing belongs to new intent.
#[path = "support/output_options_acceptance.rs"]
mod acceptance;
use std::process::{Command, Output};

fn invoke(args: &[&str]) -> Output {
    let directory = drmr_core::native::private_tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .current_dir(directory.path())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn help_explains_source_layout_and_dash_addressing_choices() {
    for command in ["analyze", "plan", "convert"] {
        let result = invoke(&[command, "--help"]);
        assert!(result.status.success());
        let help = String::from_utf8(result.stdout).unwrap();
        assert!(!help.contains("--layout"), "{command}: {help}");
        assert!(
            help.to_lowercase().contains("source directories"),
            "{command}: {help}"
        );
        if command != "analyze" {
            assert!(help.contains("--dash-addressing"), "{command}: {help}");
            for mode in ["template", "template-timeline", "list", "base"] {
                assert!(help.contains(mode), "{command}: {mode}: {help}");
            }
        }
    }
}

#[test]
fn layout_is_not_a_public_override_for_new_operations() {
    for mode in ["source", "normalized"] {
        for mut args in [
            vec![
                "convert",
                "missing.m3u8",
                "--to",
                "clear",
                "--format",
                "hls",
                "--output",
                "result",
                "--json",
            ],
            vec![
                "plan",
                "missing.m3u8",
                "--to",
                "clear",
                "--format",
                "hls",
                "--output",
                "result",
                "--save",
                "plan.json",
                "--json",
            ],
        ] {
            args.extend(["--layout", mode]);
            let result = invoke(&args);
            assert_eq!(result.status.code(), Some(2), "{args:?}");
            assert_eq!(result.stdout, [] as [u8; 0]);
        }
    }
}

#[test]
fn dash_addressing_reaches_new_dash_intent() {
    for mode in ["template", "template-timeline", "list", "base"] {
        for mut args in [
            vec![
                "convert",
                "missing.m3u8",
                "--to",
                "clear",
                "--format",
                "dash",
                "--output",
                "result",
                "--json",
            ],
            vec![
                "plan",
                "missing.m3u8",
                "--to",
                "clear",
                "--format",
                "dash",
                "--output",
                "result",
                "--save",
                "plan.json",
                "--json",
            ],
        ] {
            args.extend(["--dash-addressing", mode]);
            let result = invoke(&args);
            assert_eq!(
                result.status.code(),
                Some(1),
                "{args:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert_eq!(report["command"], args[0]);
            assert_eq!(report["status"], "failed");
        }
    }
}

#[test]
fn saved_plan_execution_rejects_layout_and_addressing_overrides() {
    for (flag, modes) in [
        ("--layout", &["source", "normalized"][..]),
        (
            "--dash-addressing",
            &["template", "template-timeline", "list", "base"][..],
        ),
    ] {
        for mode in modes {
            let result = invoke(&[
                "convert",
                "--plan",
                "missing-plan.json",
                flag,
                mode,
                "--json",
            ]);
            assert_eq!(result.status.code(), Some(2));
            assert_eq!(result.stdout, [] as [u8; 0]);
            let error = String::from_utf8(result.stderr).unwrap();
            assert!(error.contains(flag), "{error}");
        }
    }
}

#[test]
fn metadata_only_and_unsaved_plans_refuse_output_options() {
    for args in [
        vec![
            "analyze",
            "missing.m3u8",
            "--metadata-only",
            "--layout",
            "source",
        ],
        vec![
            "plan",
            "missing.m3u8",
            "--to",
            "clear",
            "--layout",
            "source",
        ],
        vec![
            "plan",
            "missing.m3u8",
            "--to",
            "clear",
            "--dash-addressing",
            "list",
        ],
        vec![
            "analyze",
            "missing.m3u8",
            "--metadata-only",
            "--dash-addressing",
            "template",
        ],
        vec![
            "convert",
            "missing.m3u8",
            "--to",
            "clear",
            "--output",
            "result",
            "--dash-addressing",
            "template-duration",
        ],
    ] {
        let result = invoke(&args);
        assert_eq!(result.status.code(), Some(2), "{args:?}");
        assert_eq!(result.stdout, [] as [u8; 0]);
    }
}
