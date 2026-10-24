//! Processing choices belong to new conversion intent, never saved-plan overrides.
use std::process::{Command, Output};

fn invoke(args: &[&str]) -> Output {
    let directory = drmr_core::native::private_tempdir().unwrap();
    if args
        .windows(2)
        .any(|pair| pair == ["--outputs", "outputs.json"])
    {
        std::fs::write(
            directory.path().join("outputs.json"),
            br#"{"schema_version":1,"outputs":[{"id":"clear","format":"dash","target":"clear","drms":[]}]}"#,
        )
        .unwrap();
    }
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .current_dir(directory.path())
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn intent_help_lists_processing_modes_and_explains_segmented_stitch_output() {
    for command in ["plan", "convert"] {
        let result = invoke(&[command, "--help"]);
        assert!(result.status.success());
        let help = String::from_utf8(result.stdout)
            .unwrap()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(help.contains("--processing"), "{command}: {help}");
        for mode in ["auto", "segments", "stitch"] {
            assert!(help.contains(mode), "{command}: {mode}: {help}");
        }
        assert!(help.contains("HLS/DASH"), "{command}: {help}");
        assert!(help.contains("segmented again"), "{command}: {help}");
        assert!(!help.contains("--segment-policy"));
    }
}

#[test]
fn processing_reaches_new_single_and_bundle_intent_without_becoming_a_usage_error() {
    for mode in ["auto", "segments", "stitch"] {
        for mut args in [
            vec![
                "plan",
                "missing.mp4",
                "--to",
                "clear",
                "--output",
                "result",
                "--save",
                "plan.json",
                "--json",
            ],
            vec![
                "plan",
                "missing.m3u8",
                "--outputs",
                "outputs.json",
                "--output",
                "result",
                "--save",
                "plan.json",
                "--json",
            ],
            vec![
                "convert",
                "missing.mp4",
                "--to",
                "clear",
                "--output",
                "result",
                "--json",
            ],
            vec![
                "convert",
                "missing.m3u8",
                "--outputs",
                "outputs.json",
                "--output",
                "result",
                "--json",
            ],
        ] {
            args.extend(["--processing", mode]);
            let result = invoke(&args);
            assert_eq!(
                result.status.code(),
                Some(1),
                "{args:?}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let report: serde_json::Value = serde_json::from_slice(&result.stdout)
                .unwrap_or_else(|_| panic!("{args:?}: result is not structured JSON"));
            assert_eq!(report["command"], args[0]);
            assert_eq!(report["status"], "failed");
        }
    }
}

#[test]
fn saved_plan_execution_cannot_override_processing_even_with_auto() {
    for mode in ["auto", "segments", "stitch"] {
        let result = invoke(&[
            "convert",
            "--plan",
            "missing-plan.json",
            "--processing",
            mode,
            "--json",
        ]);
        assert_eq!(result.status.code(), Some(2), "{mode}");
        assert!(result.stdout.is_empty());
        let error = String::from_utf8(result.stderr).unwrap();
        assert!(error.contains("--processing"), "{error}");
        assert!(error.contains("--plan"), "{error}");
    }
}

#[test]
fn metadata_only_commands_and_unsaved_plans_do_not_accept_processing_intent() {
    for args in [
        vec![
            "plan",
            "missing.mp4",
            "--to",
            "clear",
            "--processing",
            "stitch",
        ],
        vec![
            "analyze",
            "missing.mp4",
            "--metadata-only",
            "--processing",
            "segments",
        ],
        vec![
            "verify",
            "--plan",
            "missing-plan.json",
            "--processing",
            "stitch",
        ],
    ] {
        let result = invoke(&args);
        assert_eq!(result.status.code(), Some(2), "{args:?}");
        assert!(result.stdout.is_empty());
    }
}

#[test]
fn unknown_processing_values_and_unpublished_aliases_are_usage_errors() {
    for args in [
        vec!["--processing", "tracks"],
        vec!["--processing", "segment"],
        vec!["--processing", "STITCH"],
        vec!["--segment-policy", "preserve"],
    ] {
        let mut command = vec![
            "convert",
            "missing.mp4",
            "--to",
            "clear",
            "--output",
            "result",
        ];
        command.extend(args);
        let result = invoke(&command);
        assert_eq!(result.status.code(), Some(2), "{command:?}");
        assert!(result.stdout.is_empty());
    }
}
