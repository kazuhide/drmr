//! Batch CLI validation, planning, and optional dependency isolation.

mod common;

use clap::{CommandFactory as _, Parser, Subcommand};
use common::dash::{build_synthetic_dash_asset, h264_rep};
use common::{snapshot_tree, write};
use drmr_core::batch::{AssetOutcome, ResumeEntry, append_resume_entry};
use drmr_core::batch_cli::{BATCH_HELP, BatchArgs, KeyProviderChoice, StorageKind, cmd_batch};
use drmr_core::canonical::Dumps;
use drmr_core::events::JsonLinesWriter;
use drmr_core::toolrun::Secrets;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// `drmr` with the batch subcommand registered (the binary is wired later).
#[derive(Parser)]
#[command(name = "drmr")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = BATCH_HELP)]
    Batch(BatchArgs),
}

fn parse(args: &[&str]) -> Result<BatchArgs, clap::Error> {
    let mut command_line = vec!["drmr", "batch"];
    command_line.extend_from_slice(args);
    Cli::try_parse_from(command_line).map(|cli| match cli.command {
        Command::Batch(args) => args,
    })
}

/// Run `cmd_batch` with a JSON-Lines writer; the exit code and the lines it printed.
fn batch(args: &BatchArgs) -> (i32, Vec<String>) {
    let writer = JsonLinesWriter::new(Vec::new(), Arc::new(Secrets::new()));
    let code = cmd_batch(args, &writer);
    let output = String::from_utf8(writer.into_inner()).unwrap();
    (code, output.lines().map(str::to_string).collect())
}

const CONFIG_INVALID: &str = r#"{"schema_version": "1", "stage": "batch", "level": "error", "message": "batch-config-invalid"}"#;

fn work_list(path: &Path, ids: &[&str]) {
    let rows: Vec<(String, String)> = ids
        .iter()
        .map(|id| ((*id).to_string(), format!("host/{id}/manifest.mpd")))
        .collect();
    work_list_of(path, &rows);
}

fn work_list_of(path: &Path, rows: &[(String, String)]) {
    let mut content = String::from("{\"schema_version\":\"1\"}\n");
    for (id, manifest) in rows {
        let row = json!({"asset_id": id, "kind": "dash-cenc", "dash_manifest": manifest, "hls_master": null});
        content.push_str(&Dumps::DEFAULT.dumps(&row));
        content.push('\n');
    }
    write(path, content);
}

struct Paths {
    temp: tempfile::TempDir,
}

impl Paths {
    fn new() -> Paths {
        Paths {
            temp: tempfile::tempdir().unwrap(),
        }
    }
    fn path(&self, name: &str) -> PathBuf {
        self.temp.path().join(name)
    }
    fn s(&self, name: &str) -> String {
        self.path(name).to_str().unwrap().to_string()
    }
}

#[test]
fn test_cli_plan_does_not_construct_storage_or_keys() {
    let paths = Paths::new();
    work_list(&paths.path("worklist.jsonl"), &["a"]);
    let args = parse(&[
        "--work-list",
        &paths.s("worklist.jsonl"),
        "--output",
        &paths.s("out"),
        "--resume-log",
        &paths.s("resume.jsonl"),
        "--key-provider",
        "file",
        "--plan",
    ])
    .unwrap();
    let before = snapshot_tree(paths.temp.path());
    let (code, lines) = batch(&args);
    assert_eq!(code, 0);
    assert_eq!(
        lines,
        [r#"{"asset_id": "a", "action": "run", "reason": null}"#]
    );
    assert!(!paths.path("out").exists() && !paths.path("resume.jsonl").exists());
    assert_eq!(snapshot_tree(paths.temp.path()), before);
}

#[test]
fn test_batch_command_registered_without_loading_optional_sdk() {
    let error = Cli::try_parse_from(["drmr", "batch", "--help"])
        .err()
        .unwrap();
    assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    assert_eq!(error.exit_code(), 0);
    let help = error.to_string();
    for flag in [
        "--work-list",
        "--output",
        "--resume-log",
        "--storage",
        "--storage-root",
        "--bucket",
        "--region",
        "--output-prefix",
        "--key-provider",
        "--key-file",
        "--hls-config-dir",
        "--packager-bin",
        "--max-workers",
        "--retry-failed",
        "--plan",
        "--max-attempts",
        "--per-asset-timeout-s",
        "--heartbeat-s",
    ] {
        assert!(help.contains(flag), "{flag}");
    }
    let mut command = Cli::command();
    let about = command
        .find_subcommand_mut("batch")
        .unwrap()
        .get_about()
        .unwrap()
        .to_string();
    assert_eq!(about, "sequential restartable fleet repackaging");
    let standalone = drmr_core::batch_cli::command();
    assert_eq!(standalone.get_name(), "batch");
    assert_eq!(standalone.get_about().unwrap().to_string(), BATCH_HELP);
}

#[test]
fn the_argument_surface_has_python_s_names_and_defaults() {
    let args = parse(&["--work-list", "w", "--output", "o", "--resume-log", "r"]).unwrap();
    assert_eq!(
        (
            args.work_list.as_path(),
            args.output.as_path(),
            args.resume_log.as_path()
        ),
        (Path::new("w"), Path::new("o"), Path::new("r"))
    );
    assert_eq!(args.storage, StorageKind::Local);
    assert_eq!(args.key_provider, KeyProviderChoice::Env);
    assert_eq!(
        (args.region.as_str(), args.output_prefix.as_str()),
        ("ap-northeast-1", "output")
    );
    assert_eq!(
        (
            args.max_attempts,
            args.per_asset_timeout_s,
            args.heartbeat_s
        ),
        (3, 3600.0, 60.0)
    );
    assert!(args.storage_root.is_none() && args.bucket.is_none() && args.key_file.is_none());
    assert!(
        args.hls_config_dir.is_none() && args.packager_bin.is_none() && args.max_workers.is_none()
    );
    assert!(!args.retry_failed && !args.plan);
    let args = parse(&[
        "--work-list=w",
        "--output",
        "./o/",
        "--resume-log",
        "r",
        "--storage",
        "s3",
        "--bucket",
        "b",
        "--key-provider",
        "file",
        "--key-file",
        "k",
        "--max-workers",
        " 4 ",
        "--max-attempts",
        "1_0",
        "--per-asset-timeout-s",
        "1e3",
        "--heartbeat-s",
        ".5",
        "--retry-failed",
        "--plan",
        "--plan",
    ])
    .unwrap();
    assert_eq!(args.output, Path::new("o"), "argparse's type=Path spelling");
    assert_eq!(
        (args.storage, args.key_provider),
        (StorageKind::S3, KeyProviderChoice::File)
    );
    assert_eq!((args.max_workers, args.max_attempts), (Some(4), 10));
    assert_eq!((args.per_asset_timeout_s, args.heartbeat_s), (1000.0, 0.5));
}

#[test]
fn prefixes_repeats_and_bad_values_follow_argparse() {
    // argparse accepts unambiguous prefixes and repeated options (the last wins).
    let args = parse(&[
        "--work",
        "w",
        "--output",
        "o",
        "--resume",
        "r",
        "--storage-r",
        "s",
        "--output",
        "p",
    ])
    .unwrap();
    assert_eq!(args.output, Path::new("p"));
    assert_eq!(args.storage_root.as_deref(), Some(Path::new("s")));
    for bad in [
        &["--output", "o", "--resume-log", "r"][..],
        &[
            "--work-list",
            "w",
            "--output",
            "o",
            "--resume-log",
            "r",
            "--storage",
            "gcs",
        ],
        &[
            "--work-list",
            "w",
            "--output",
            "o",
            "--resume-log",
            "r",
            "--max-attempts",
            "x",
        ],
        &[
            "--work-list",
            "w",
            "--output",
            "o",
            "--resume-log",
            "r",
            "--max",
            "1",
        ],
        &["--work-list", "w", "--out", "o", "--resume-log", "r"],
    ] {
        let error = parse(bad).err().unwrap();
        assert_eq!(error.exit_code(), 2, "{bad:?}");
    }
}

#[test]
fn configuration_errors_print_one_record_and_exit_2() {
    let paths = Paths::new();
    work_list(&paths.path("w.jsonl"), &["a"]);
    std::fs::create_dir(paths.path("store")).unwrap();
    let base = [
        "--work-list",
        &paths.s("w.jsonl"),
        "--output",
        &paths.s("out"),
        "--resume-log",
        &paths.s("r.jsonl"),
    ]
    .map(str::to_string);
    let key_file = paths.s("keys.jsonl");
    let store = paths.s("store");
    let cases: Vec<Vec<&str>> = vec![
        vec!["--key-provider", "file", "--storage-root", &store],
        vec!["--key-file", &key_file, "--storage-root", &store],
        vec![],
        vec!["--storage", "s3"],
        vec!["--storage", "s3", "--bucket", ""],
        vec!["--storage", "s3", "--bucket", "b", "--region", "us-east-1"],
        vec!["--storage-root", &store, "--max-attempts", "0"],
        vec!["--storage-root", &store, "--max-workers", "65"],
        vec!["--storage-root", &store, "--heartbeat-s", "nan"],
        vec!["--storage-root", &store, "--output-prefix", "../x"],
    ];
    for extra in cases {
        let mut argv: Vec<&str> = base.iter().map(String::as_str).collect();
        argv.extend(extra.iter().copied());
        let (code, lines) = batch(&parse(&argv).unwrap());
        assert_eq!(
            (code, lines.as_slice()),
            (2, [CONFIG_INVALID.to_string()].as_slice()),
            "{extra:?}"
        );
    }
    write(&paths.path("bad.jsonl"), "{\"schema_version\": \"2\"}\n");
    let argv = [
        "--work-list",
        &paths.s("bad.jsonl"),
        "--output",
        "o",
        "--resume-log",
        "r",
        "--plan",
    ];
    assert_eq!(
        batch(&parse(&argv).unwrap()),
        (2, vec![CONFIG_INVALID.to_string()])
    );
    assert!(!paths.path("out").exists(), "nothing ran");
}

fn terminal(log: &Path, entries: &[(&str, AssetOutcome, &str)]) {
    for (asset, outcome, reason) in entries {
        append_resume_entry(
            log,
            &ResumeEntry::new(asset, *outcome, reason, 1, "then").unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn resumed_runs_exit_0_when_every_asset_passed_and_1_when_any_failed() {
    let paths = Paths::new();
    // The global preflight reads every work-list asset's sources, skipped ones included, so
    // the sources exist (as they would for a resumed production run).
    let mut rows = Vec::new();
    for id in ["a", "b"] {
        let built = build_synthetic_dash_asset(
            &paths.path("store"),
            &[h264_rep("v")],
            &format!("title-{id}"),
        );
        let key = built
            .mpd_path
            .strip_prefix(paths.path("store/download"))
            .unwrap();
        // Object keys use URL separators, independent of native filesystem paths.
        rows.push((
            id.to_string(),
            key.to_str()
                .unwrap()
                .replace(std::path::MAIN_SEPARATOR, "/"),
        ));
    }
    work_list_of(&paths.path("w.jsonl"), &rows);
    let args = parse(&[
        "--work-list",
        &paths.s("w.jsonl"),
        "--output",
        &paths.s("out"),
        "--resume-log",
        &paths.s("r.jsonl"),
        "--storage-root",
        &paths.s("store/download"),
    ])
    .unwrap();
    // A resumed run that processed nothing new: every asset done-pass, exit 0.
    terminal(
        &paths.path("r.jsonl"),
        &[
            ("a", AssetOutcome::DonePass, "ok"),
            ("b", AssetOutcome::DonePass, "ok"),
        ],
    );
    let (code, lines) = batch(&args);
    assert_eq!(code, 0);
    assert_eq!(
        lines,
        [
            r#"{"schema_version": "1", "stage": "batch-report", "total": 2, "passed": 2, "failed": 0, "skipped": 2}"#
        ]
    );
    let report = std::fs::read_to_string(paths.path("out/_batch/batch-report.json")).unwrap();
    assert!(report.contains("\"skipped\": 2"));
    // A done-fail from an earlier run still makes the exit code 1.
    terminal(
        &paths.path("r.jsonl"),
        &[("b", AssetOutcome::DoneFail, "missing-key")],
    );
    let (code, lines) = batch(&args);
    assert_eq!(code, 1);
    assert_eq!(
        lines,
        [
            r#"{"schema_version": "1", "stage": "batch-report", "total": 2, "passed": 1, "failed": 1, "skipped": 2}"#
        ]
    );
    let plan = parse(&[
        "--work-list",
        &paths.s("w.jsonl"),
        "--output",
        &paths.s("out"),
        "--resume-log",
        &paths.s("r.jsonl"),
        "--plan",
    ])
    .unwrap();
    let (code, lines) = batch(&plan);
    assert_eq!(code, 0, "a plan listing quarantined assets is still exit 0");
    assert_eq!(
        lines,
        [
            r#"{"asset_id": "a", "action": "skip", "reason": "ok"}"#,
            r#"{"asset_id": "b", "action": "quarantine", "reason": "missing-key"}"#,
        ]
    );
    let retry = parse(&[
        "--work-list",
        &paths.s("w.jsonl"),
        "--output",
        "o",
        "--resume-log",
        &paths.s("r.jsonl"),
        "--plan",
        "--retry-failed",
    ])
    .unwrap();
    assert_eq!(
        batch(&retry).1[1],
        r#"{"asset_id": "b", "action": "run", "reason": "missing-key"}"#
    );
}
