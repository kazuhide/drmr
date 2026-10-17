//! Worker entry points and per-asset child processes with isolated environments.

mod common;

use common::dash::{build_synthetic_dash_asset, h264_rep};
use common::{IV, KEY, KID, write, write_mode};
use drmr_core::batch::{
    AssetOutcome, AssetWorkItem, BatchConfig, EnvKeyProvider, FileKeyProvider, KeyNotAvailable,
    KeyProvider, RetryPolicy, RunError, Seams, WORKER_ARG, load_resume_log, run_batch_with,
    worker_main, worker_payload,
};
use drmr_core::canonical::Dumps;
use drmr_core::config::KeyMaterial;
use drmr_core::storage::{LocalStorageBackend, ObjectRef};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;

type Scenario = fn();

/// The environment variable that makes this binary a crashing worker (set for one scenario).
const CRASH: &str = "DRMR_BATCH_TEST_WORKER_CRASH";

const SCENARIOS: &[(&str, Scenario, bool)] = &[
    (
        "test_real_exec_worker_missing_key_clears_orphan_marker",
        missing_key_clears_orphan_marker,
        false,
    ),
    (
        "real_exec_worker_publishes_a_whole_dash_attempt",
        publishes_a_whole_attempt,
        false,
    ),
    (
        "real_exec_worker_rebuilds_a_file_key_provider",
        rebuilds_a_file_key_provider,
        false,
    ),
    (
        "real_exec_worker_quarantines_a_failed_conversion",
        quarantines_a_failed_conversion,
        false,
    ),
    (
        "real_exec_worker_reads_invalid_provided_keys_as_missing_key_after_staging",
        provided_invalid_keys_are_missing_after_staging,
        false,
    ),
    (
        "cmd_batch_runs_the_real_worker_and_prints_python_s_records",
        cmd_batch_end_to_end,
        false,
    ),
    (
        "cmd_batch_exits_130_when_a_worker_crashes",
        cmd_batch_worker_crash,
        true,
    ),
];

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == [WORKER_ARG] {
        if std::env::var_os(CRASH).is_some() {
            std::process::exit(3);
        }
        std::process::exit(worker_main());
    }
    if args.first().map(String::as_str) == Some("run") {
        std::process::exit(fake_run(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("--scenario") {
        let name = &args[1];
        let (_, scenario, _) = SCENARIOS
            .iter()
            .find(|(n, _, _)| n == name)
            .expect("known scenario");
        scenario();
        return;
    }
    let filter = args.iter().find(|arg| !arg.starts_with('-'));
    let exe = std::env::current_exe().unwrap();
    let mut failed = Vec::new();
    let mut passed = 0;
    for (name, _, crash) in SCENARIOS
        .iter()
        .filter(|(n, _, _)| filter.is_none_or(|f| n.contains(f.as_str())))
    {
        let mut command = std::process::Command::new(&exe);
        command
            .args(["--scenario", name])
            .env_remove("DRM_KID")
            .env_remove("DRM_KEY")
            .env_remove("DRM_IV")
            .env_remove(CRASH);
        if *crash {
            command.env(CRASH, "1");
        }
        let status = command.status().unwrap();
        if status.success() {
            println!("test {name} ... ok");
            passed += 1;
        } else {
            println!("test {name} ... FAILED");
            failed.push(*name);
        }
    }
    let verdict = if failed.is_empty() { "ok" } else { "FAILED" };
    println!(
        "\ntest result: {verdict}. {passed} passed; {} failed; 0 ignored; 0 measured; 0 filtered out",
        failed.len()
    );
    if !failed.is_empty() {
        std::process::exit(101);
    }
}

/// The test double of `run`: checks it got the staged manifest and the key material through
/// its environment only, then writes a converted asset (or exits 1 for `fail-*` assets).
fn fake_run(args: &[String]) -> i32 {
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .map(|i| args[i + 1].clone())
    };
    let (Some(mpd), Some(asset), Some(output)) =
        (value("--mpd"), value("--asset-id"), value("--output"))
    else {
        return 2;
    };
    let keyed = std::env::var("DRM_KEY").ok().as_deref() == Some(KEY)
        && std::env::var("DRM_IV").ok().as_deref() == Some(IV)
        && std::env::var("DRM_KID").ok().as_deref() == Some(KID);
    if !keyed || args.iter().any(|arg| arg.contains(KEY) || arg.contains(IV)) {
        return 2;
    }
    if value("--log-format").as_deref() != Some("jsonl")
        || !args.iter().any(|a| a == "--no-manifests")
    {
        return 2;
    }
    if !Path::new(&mpd).is_file() {
        return 2;
    }
    if asset.starts_with("fail") {
        return 1;
    }
    let root = Path::new(&output).join(&asset);
    write(&root.join("cbcs/v/seg_1.m4s"), format!("CBCS:{asset}"));
    write(
        &root.join("reports/phase1-summary.json"),
        b"{\"all_pass\": true}",
    );
    0
}

struct Fixture {
    temp: common::PrivateTempDir,
    download: std::path::PathBuf,
}

fn fixture(title: &str) -> (Fixture, ObjectRef) {
    let temp = common::private_tempdir().unwrap();
    let store = temp.path().join("store");
    let built = build_synthetic_dash_asset(&store, &[h264_rep("v")], title);
    let download = store.join("download");
    let manifest = built
        .mpd_path
        .strip_prefix(&download)
        .unwrap()
        .components()
        .map(|part| part.as_os_str().to_str().unwrap())
        .collect::<Vec<_>>()
        .join("/");
    (
        Fixture { temp, download },
        ObjectRef::new(manifest).unwrap(),
    )
}

fn config(fixture: &Fixture, keys: Arc<dyn KeyProvider>) -> BatchConfig {
    BatchConfig {
        retry: RetryPolicy {
            max_attempts: 2,
            backoff_base_s: 0.0,
            backoff_max_s: 0.0,
            per_asset_timeout_s: 60.0,
            ..RetryPolicy::default()
        },
        ..BatchConfig::new(
            Arc::new(LocalStorageBackend::new(&fixture.download).unwrap()),
            keys,
            &fixture.temp.path().join("out"),
            &fixture.temp.path().join("resume.jsonl"),
        )
    }
}

/// Seams of the `config` fixture (no global preflight) and the real worker command.
fn seams() -> Seams<'static> {
    // The seam's signature: the replaced `_preflight_source_graphs` may raise.
    #[allow(clippy::unnecessary_wraps)]
    fn no_preflight(_: &[AssetWorkItem], _: &BatchConfig) -> Result<(), RunError> {
        Ok(())
    }
    Seams {
        preflight: Some(&no_preflight),
        ..Seams::default()
    }
}

fn missing_key_clears_orphan_marker() {
    let (fixture, manifest) = fixture("title-abc123");
    let work = AssetWorkItem::new("missing", "dash-cenc", Some(manifest), None).unwrap();
    let marker = fixture.download.join("output/missing/_complete.json");
    write(&marker, b"{\"outcome\":\"done-pass\"}");
    let config = config(&fixture, Arc::new(EnvKeyProvider));
    let report = run_batch_with(&[work], &config, None, None, &seams()).unwrap();
    assert_eq!(report.outcomes[0].reason, "missing-key");
    assert!(
        !marker.exists(),
        "the orphan success marker is invalidated before the key lookup"
    );
}

struct Provided;

impl KeyProvider for Provided {
    fn key_material(&self, asset_id: &str) -> Result<KeyMaterial, KeyNotAvailable> {
        if asset_id.starts_with("none") {
            return Err(KeyNotAvailable);
        }
        Ok(KeyMaterial::new(KID, KEY, IV).unwrap())
    }
}

fn publishes_a_whole_attempt() {
    let (fixture, manifest) = fixture("title-abc123");
    let work = AssetWorkItem::new("a", "dash-cenc", Some(manifest.clone()), None).unwrap();
    let none = AssetWorkItem::new("none", "dash-cenc", Some(manifest), None).unwrap();
    let config = config(&fixture, Arc::new(Provided));
    let before = common::snapshot_tree(&fixture.download);
    let report = run_batch_with(&[work, none], &config, None, None, &seams()).unwrap();
    let reasons: Vec<&str> = report.outcomes.iter().map(|e| e.reason.as_str()).collect();
    assert_eq!(reasons, ["ok", "missing-key"]);
    let remote = fixture.download.join("output/a");
    assert_eq!(
        std::fs::read(remote.join("cbcs/v/seg_1.m4s")).unwrap(),
        b"CBCS:a"
    );
    let marker: Value =
        serde_json::from_slice(&std::fs::read(remote.join("_complete.json")).unwrap()).unwrap();
    assert_eq!(
        marker,
        json!({"schema_version": "1", "asset_id": "a", "outcome": "done-pass", "object_count": 2})
    );
    assert!(!config.output_root.join("a").exists());
    let log = load_resume_log(&config.resume_log).unwrap();
    assert_eq!(log.get("a").unwrap().outcome, AssetOutcome::DonePass);
    let after: Vec<_> = common::snapshot_tree(&fixture.download)
        .into_iter()
        .filter(|(path, _)| !path.starts_with("output/"))
        .collect();
    assert_eq!(after, before, "the sources are read-only");
    let resume = std::fs::read_to_string(&config.resume_log).unwrap();
    assert!(!resume.contains(KEY) && !resume.contains(IV));
}

/// Python's worker puts a "provided" provider's texts in its environment
/// (`os.environ.update`) and reads them back with an `EnvKeyProvider`: texts that are not key
/// material fail only when the attempt asks for its keys — after staging and the owned-prefix
/// delete — as `missing-key`; a value that is not text fails `os.environ.update` itself (a
/// `TypeError`: unclassified) before anything is staged.
fn provided_invalid_keys_are_missing_after_staging() {
    let (fixture, manifest) = fixture("title-abc123");
    let work = AssetWorkItem::new("a", "dash-cenc", Some(manifest.clone()), None).unwrap();
    let marker = fixture.download.join("output/a/_complete.json");
    write(&marker, b"{\"outcome\":\"done-pass\"}");
    let config = config(&fixture, Arc::new(Provided));
    let mut envelope: Value =
        serde_json::from_slice(&worker_payload(&work, &config).unwrap()).unwrap();
    assert_eq!(envelope["provider"]["kind"], "provided");
    envelope["provider"]["kid"] = json!("not-hex");
    assert_eq!(
        run_worker(&Dumps::DEFAULT.dumps(&envelope)),
        "[\"missing-key\", false]\n"
    );
    let staged = config
        .output_root
        .join("a/clear/_batch-stage/download")
        .join(manifest.key());
    assert!(
        staged.is_file(),
        "the inputs were staged before the key lookup"
    );
    assert!(
        !marker.exists(),
        "the owned prefix was cleared before the key lookup"
    );
    std::fs::remove_dir_all(&config.output_root).unwrap();
    envelope["provider"]["kid"] = json!(5);
    assert_eq!(
        run_worker(&Dumps::DEFAULT.dumps(&envelope)),
        "[\"unclassified\", false]\n"
    );
    assert!(!config.output_root.join("a").exists());
}

/// One asset worker fed `payload` on its stdin: its answer line.
fn run_worker(payload: &str) -> String {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .arg(WORKER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap()
}

fn rebuilds_a_file_key_provider() {
    let (fixture, manifest) = fixture("title-abc123");
    let key_file = fixture.temp.path().join("keys.jsonl");
    let row = Dumps::DEFAULT.dumps(&json!({"asset_id": "a", "kid": KID, "key": KEY, "iv": IV}));
    write_mode(&key_file, format!("{row}\n").as_bytes(), 0o600);
    let out = fixture.temp.path().join("out");
    let provider = FileKeyProvider::new(&key_file, &out).unwrap();
    let config = config(&fixture, Arc::new(provider));
    let work = [
        AssetWorkItem::new("a", "dash-cenc", Some(manifest.clone()), None).unwrap(),
        AssetWorkItem::new("b", "dash-cenc", Some(manifest), None).unwrap(),
    ];
    let report = run_batch_with(&work, &config, None, None, &seams()).unwrap();
    let reasons: Vec<&str> = report.outcomes.iter().map(|e| e.reason.as_str()).collect();
    assert_eq!(reasons, ["ok", "missing-key"]);
}

fn quarantines_a_failed_conversion() {
    let (fixture, manifest) = fixture("title-abc123");
    let work = AssetWorkItem::new("fail-a", "dash-cenc", Some(manifest), None).unwrap();
    let config = config(&fixture, Arc::new(Provided));
    let report = run_batch_with(&[work], &config, None, None, &seams()).unwrap();
    assert_eq!(report.outcomes[0].reason, "content-fail");
    assert_eq!(report.outcomes[0].attempt_count(), Some(1));
    assert!(
        !fixture
            .download
            .join("output/fail-a/_complete.json")
            .exists()
    );
}

/// `cmd_batch` with `--key-provider file`: the real worker runs each attempt.
fn cli_args(
    fixture: &Fixture,
    work_list: &[(&str, &ObjectRef)],
) -> drmr_core::batch_cli::BatchArgs {
    use clap::FromArgMatches as _;
    let mut content = String::from("{\"schema_version\":\"1\"}\n");
    for (id, manifest) in work_list {
        let row = json!({"asset_id": id, "kind": "dash-cenc", "dash_manifest": manifest.key(), "hls_master": null});
        content.push_str(&Dumps::DEFAULT.dumps(&row));
        content.push('\n');
    }
    let path = |name: &str| fixture.temp.path().join(name).to_str().unwrap().to_string();
    write(Path::new(&path("work.jsonl")), content);
    let mut keys = String::new();
    for (id, _) in work_list {
        keys.push_str(
            &Dumps::DEFAULT.dumps(&json!({"asset_id": id, "kid": KID, "key": KEY, "iv": IV})),
        );
        keys.push('\n');
    }
    write_mode(Path::new(&path("keys.jsonl")), keys.as_bytes(), 0o600);
    let argv = [
        "batch".to_string(),
        "--work-list".into(),
        path("work.jsonl"),
        "--output".into(),
        path("out"),
        "--resume-log".into(),
        path("resume.jsonl"),
        "--storage-root".into(),
        fixture.download.to_str().unwrap().into(),
        "--key-provider".into(),
        "file".into(),
        "--key-file".into(),
        path("keys.jsonl"),
        "--max-attempts".into(),
        "1".into(),
    ];
    let matches = drmr_core::batch_cli::command()
        .try_get_matches_from(argv)
        .unwrap();
    drmr_core::batch_cli::BatchArgs::from_arg_matches(&matches).unwrap()
}

fn printed(args: &drmr_core::batch_cli::BatchArgs) -> (i32, Vec<Value>, String) {
    let writer = drmr_core::events::JsonLinesWriter::new(
        Vec::new(),
        Arc::new(drmr_core::toolrun::Secrets::with([KEY, IV])),
    );
    let code = drmr_core::batch_cli::cmd_batch(args, &writer);
    let text = String::from_utf8(writer.into_inner()).unwrap();
    let records = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (code, records, text)
}

fn cmd_batch_end_to_end() {
    let (fixture, manifest) = fixture("title-abc123");
    let args = cli_args(&fixture, &[("a", &manifest)]);
    let (code, records, text) = printed(&args);
    assert_eq!(code, 0, "{text}");
    let messages: Vec<&str> = records
        .iter()
        .map(|r| r["message"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(messages, ["asset-start", "asset-end", ""]);
    assert_eq!(
        text.lines().last().unwrap(),
        r#"{"schema_version": "1", "stage": "batch-report", "total": 1, "passed": 1, "failed": 0, "skipped": 0}"#
    );
    assert!(fixture.download.join("output/a/_complete.json").is_file());
    assert!(!text.contains(KEY) && !text.contains("keys.jsonl"));
    let (fixture, manifest) = fixture_failing();
    let args = cli_args(&fixture, &[("fail-b", &manifest)]);
    let (code, records, _) = printed(&args);
    assert_eq!(code, 1);
    assert_eq!(records.last().unwrap()["failed"], json!(1));
}

fn fixture_failing() -> (Fixture, ObjectRef) {
    fixture("title-fail")
}

fn cmd_batch_worker_crash() {
    let (fixture, manifest) = fixture("title-abc123");
    let args = cli_args(&fixture, &[("a", &manifest)]);
    let (code, records, _) = printed(&args);
    assert_eq!(code, drmr_core::batch_cli::EXIT_INTERRUPTED);
    let stages: Vec<&str> = records
        .iter()
        .map(|r| r["stage"].as_str().unwrap())
        .collect();
    assert_eq!(
        stages,
        ["batch"],
        "only asset-start: no report record after a crash"
    );
    assert!(
        load_resume_log(&fixture.temp.path().join("resume.jsonl"))
            .unwrap()
            .is_empty()
    );
    assert!(!fixture.temp.path().join("out/a").exists());
}
