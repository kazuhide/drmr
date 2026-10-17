//! Batch completion, durable resume records, retries, timeouts, and worker supervision.
#![cfg(unix)] // the modules under test exist on unix only (open decision #23)

mod common;

use common::dash::{audio_rep, build_synthetic_dash_asset, h264_rep};
use common::{IV, KEY, KID, snapshot_tree, write, write_mode};
use drmr_core::batch::{
    AssetOutcome, AssetRunner, AssetWorkItem, AttemptError, BatchConfig, BatchError, CliCommand,
    EnvKeyProvider, FileKeyProvider, HlsSource, KeyNotAvailable, KeyProvider, PlaylistFetcher,
    ResumeEntry, RetryPolicy, RunError, Seams, append_resume_entry, commands, load_resume_log,
    plan_batch, preflight_source_graphs, read_work_list, run_batch_with, run_commands,
    source_overlaps_output, stage_inputs, supervise_attempt,
};
use drmr_core::batch_support::DefaultHlsSource;
use drmr_core::canonical::Dumps;
use drmr_core::config::KeyMaterial;
use drmr_core::events::{Collector, Record, Sink};
use drmr_core::source::platform::SystemPlatform;
use drmr_core::source::{
    AnalyzeOptions, Direct, Failure, FetchPolicy, ProbeStream, SourceAnalysis, SourceRequest,
    analyze_hls_with,
};
use drmr_core::storage::{
    LocalStorageBackend, ObjectRef, StorageBackend, StorageError, StorageLocation, TransferPolicy,
};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::fmt::Write as _;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------------------------------
// Fixtures (the `config` fixture, `item`, `runner` and `Keys` of the fixture).
// ------------------------------------------------------------------------------------------

fn key() -> KeyMaterial {
    KeyMaterial::new(KID, KEY, IV).unwrap()
}

struct Keys;

impl KeyProvider for Keys {
    fn key_material(&self, asset_id: &str) -> Result<KeyMaterial, KeyNotAvailable> {
        if asset_id == "missing" {
            Err(KeyNotAvailable)
        } else {
            Ok(key())
        }
    }
}

struct Fixture {
    temp: tempfile::TempDir,
    config: BatchConfig,
}

impl Fixture {
    fn root(&self) -> PathBuf {
        self.config.storage.root().unwrap().to_path_buf()
    }
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    std::fs::create_dir(&root).unwrap();
    let config = BatchConfig {
        retry: RetryPolicy {
            max_attempts: 2,
            backoff_base_s: 0.0,
            backoff_max_s: 0.0,
            ..RetryPolicy::default()
        },
        ..BatchConfig::new(
            Arc::new(LocalStorageBackend::new(&root).unwrap()),
            Arc::new(Keys),
            &temp.path().join("out"),
            &temp.path().join("resume.jsonl"),
        )
    };
    Fixture { temp, config }
}

fn item(asset_id: &str) -> AssetWorkItem {
    let manifest = ObjectRef::new(format!("host/{asset_id}/manifest.mpd")).unwrap();
    AssetWorkItem::new(asset_id, "dash-cenc", Some(manifest), None).unwrap()
}

// The seam's signature (`runner(work, config, stage, keys, on_heartbeat) -> int`).
#[allow(clippy::unnecessary_wraps)]
fn runner(
    work: &AssetWorkItem,
    config: &BatchConfig,
    _stage: &Path,
    _keys: &KeyMaterial,
    _on_heartbeat: &dyn Fn(),
) -> Result<i32, AttemptError> {
    write(
        &config
            .output_root
            .join(&work.asset_id)
            .join("cbcs/file.m4s"),
        work.asset_id.as_bytes(),
    );
    Ok(0)
}

// The seam's signature: the replaced `_preflight_source_graphs` may raise.
#[allow(clippy::unnecessary_wraps)]
fn no_preflight(_: &[AssetWorkItem], _: &BatchConfig) -> Result<(), RunError> {
    Ok(())
}

// The seam's signature: the replaced `_stage_inputs` may raise.
#[allow(clippy::unnecessary_wraps)]
fn no_stage(work: &AssetWorkItem, config: &BatchConfig) -> Result<PathBuf, AttemptError> {
    Ok(config
        .output_root
        .join(&work.asset_id)
        .join("clear/stage/download"))
}

/// The `config` fixture's dependency injection: no preflight.
fn seams() -> Seams<'static> {
    Seams {
        preflight: Some(&no_preflight),
        ..Seams::default()
    }
}

/// No preflight and no staging (`_stage_inputs` patched to a path).
fn seams_no_stage() -> Seams<'static> {
    Seams {
        preflight: Some(&no_preflight),
        stage_inputs: Some(&no_stage),
        ..Seams::default()
    }
}

fn run(
    work: &[AssetWorkItem],
    config: &BatchConfig,
    runner: &dyn AssetRunner,
    seams: &Seams<'_>,
) -> Result<drmr_core::batch::BatchReport, RunError> {
    run_batch_with(work, config, None, Some(runner), seams)
}

fn reasons(report: &drmr_core::batch::BatchReport) -> Vec<(&str, usize)> {
    report
        .reasons
        .iter()
        .map(|(r, c)| (r.as_str(), *c))
        .collect()
}

fn fail(
    _: &AssetWorkItem,
    _: &BatchConfig,
    _: &Path,
    _: &KeyMaterial,
    _: &dyn Fn(),
) -> Result<i32, AttemptError> {
    panic!("the runner must not run")
}

type PutFn =
    dyn Fn(&LocalStorageBackend, &[(PathBuf, ObjectRef)]) -> Result<(), StorageError> + Send + Sync;
type SizeFn = dyn Fn(&ObjectRef) -> Result<u64, StorageError> + Send + Sync;

/// A local store whose writes and lookups can be overridden (the tests subclass
/// `LocalStorageBackend`).
struct Wrapped {
    inner: LocalStorageBackend,
    put: Box<PutFn>,
    size: Option<Box<SizeFn>>,
    deletes: AtomicUsize,
}

impl Wrapped {
    fn new(
        root: &Path,
        put: impl Fn(&LocalStorageBackend, &[(PathBuf, ObjectRef)]) -> Result<(), StorageError>
        + Send
        + Sync
        + 'static,
    ) -> Wrapped {
        Wrapped {
            inner: LocalStorageBackend::new(root).unwrap(),
            put: Box::new(put),
            size: None,
            deletes: AtomicUsize::new(0),
        }
    }
}

impl StorageBackend for Wrapped {
    fn exists(&self, object: &ObjectRef) -> Result<bool, StorageError> {
        self.inner.exists(object)
    }
    fn get_many(&self, refs: &[ObjectRef], dest_root: &Path) -> Result<Vec<PathBuf>, StorageError> {
        self.inner.get_many(refs, dest_root)
    }
    fn put_many(&self, items: &[(PathBuf, ObjectRef)]) -> Result<(), StorageError> {
        (self.put)(&self.inner, items)
    }
    fn delete_prefix(&self, prefix: &ObjectRef) -> Result<(), StorageError> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.inner.delete_prefix(prefix)
    }
    fn list_prefix(&self, prefix: &ObjectRef) -> Result<Vec<ObjectRef>, StorageError> {
        self.inner.list_prefix(prefix)
    }
    fn object_size(&self, object: &ObjectRef) -> Result<u64, StorageError> {
        match &self.size {
            Some(size) => size(object),
            None => self.inner.object_size(object),
        }
    }
    fn root(&self) -> Option<&Path> {
        self.inner.root()
    }
    fn policy(&self) -> Option<TransferPolicy> {
        self.inner.policy()
    }
    fn location(&self) -> Option<StorageLocation> {
        self.inner.location()
    }
}

fn find_named(root: &Path, name: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)
            .into_iter()
            .flatten()
            .flatten()
        {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == name) {
                found.push(path.clone());
            }
            if path.is_dir() {
                pending.push(path);
            }
        }
    }
    found
}

fn relative_key(path: &Path, root: &Path) -> ObjectRef {
    ObjectRef::new(path.strip_prefix(root).unwrap().to_str().unwrap()).unwrap()
}

// ------------------------------------------------------------------------------------------
// The cases of the fixture cases.
// ------------------------------------------------------------------------------------------

#[test]
fn test_terminal_resume_latest_and_private_mode() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("resume");
    append_resume_entry(
        &path,
        &ResumeEntry::new("a", AssetOutcome::DoneFail, "content-fail", 1, "now").unwrap(),
    )
    .unwrap();
    append_resume_entry(
        &path,
        &ResumeEntry::new("a", AssetOutcome::DonePass, "ok", 2, "later").unwrap(),
    )
    .unwrap();
    assert_eq!(
        load_resume_log(&path).unwrap().get("a").unwrap().outcome,
        AssetOutcome::DonePass
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let first = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert_eq!(
        serde_json::from_str::<Value>(&first).unwrap(),
        json!({"schema_version": "1"})
    );
}

#[test]
fn test_worklist_rejects_keys_unknown_and_duplicates() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("work");
    let row = json!({"asset_id": "a", "kind": "dash-cenc", "dash_manifest": "host/a/main.mpd", "hls_master": null});
    std::fs::write(
        &path,
        format!(
            "{{\"schema_version\":\"1\"}}\n{}\n",
            Dumps::DEFAULT.dumps(&row)
        ),
    )
    .unwrap();
    assert_eq!(
        read_work_list(&path).unwrap()[0]
            .dash_manifest
            .as_ref()
            .unwrap()
            .key(),
        "host/a/main.mpd"
    );
    for extra in [json!({"key": "secret"}), json!({"kind": "unknown"})] {
        let mut row = row.clone();
        for (name, value) in extra.as_object().unwrap() {
            row[name] = value.clone();
        }
        std::fs::write(
            &path,
            format!(
                "{{\"schema_version\":\"1\"}}\n{}",
                Dumps::DEFAULT.dumps(&row)
            ),
        )
        .unwrap();
        assert_eq!(
            read_work_list(&path).unwrap_err().token(),
            "work-list-invalid"
        );
    }
}

#[test]
fn work_list_rows_follow_python_s_rules() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("work");
    let header = "{\"schema_version\": \"1\"}\n";
    let valid = [
        // A falsy location is None (`ObjectRef(x) if x else None`); hls-clear needs none.
        r#"{"asset_id": "n", "kind": "hls-clear", "dash_manifest": null, "hls_master": 0}"#,
        r#"{"asset_id": "m", "kind": "dash-cenc", "dash_manifest": "h/m.mpd", "hls_master": ""}"#,
    ];
    std::fs::write(&path, format!("{header}{}\n{}\n", valid[0], valid[1])).unwrap();
    let items = read_work_list(&path).unwrap();
    assert_eq!(items.len(), 2);
    assert!(items[0].dash_manifest.is_none() && items[0].hls_master.is_none());
    let invalid = [
        r#"{"asset_id": "a", "kind": "dash-cenc", "dash_manifest": null, "hls_master": null}"#,
        r#"{"asset_id": "a", "kind": "dash-cenc", "dash_manifest": "h/../x", "hls_master": null}"#,
        r#"{"asset_id": "a", "kind": "dash-cenc", "dash_manifest": 5, "hls_master": null}"#,
        r#"{"asset_id": "-a", "kind": "dash-cenc", "dash_manifest": "h/a", "hls_master": null}"#,
        r#"{"asset_id": 7, "kind": "dash-cenc", "dash_manifest": "h/a", "hls_master": null}"#,
        r#"["asset_id", "kind", "dash_manifest", "hls_master"]"#,
        r#"{"asset_id": "a", "kind": "dash-cenc", "dash_manifest": "h/a"}"#,
    ];
    for row in invalid {
        std::fs::write(&path, format!("{header}{row}\n")).unwrap();
        assert!(read_work_list(&path).is_err(), "{row}");
    }
    let duplicate =
        r#"{"asset_id": "a", "kind": "dash-cenc", "dash_manifest": "h/a", "hls_master": null}"#;
    std::fs::write(&path, format!("{header}{duplicate}\n{duplicate}\n")).unwrap();
    assert!(read_work_list(&path).is_err());
    for content in [
        "",
        "{\"schema_version\": \"2\"}\n",
        "\n",
        "{\"schema_version\": \"1\"}\n\n",
    ] {
        std::fs::write(&path, content).unwrap();
        assert!(read_work_list(&path).is_err(), "{content:?}");
    }
    std::fs::write(&path, "{\"schema_version\":\"1\"}\n").unwrap();
    assert_eq!(read_work_list(&path).unwrap(), Vec::<AssetWorkItem>::new());
}

#[test]
fn test_batch_marker_last_resume_and_no_contamination() {
    let fixture = fixture();
    let config = &fixture.config;
    let logs = Collector::default();
    let report = run_batch_with(
        &[item("a"), item("b")],
        config,
        Some(&logs),
        Some(&runner),
        &seams_no_stage(),
    )
    .unwrap();
    assert_eq!((report.passed, report.failed, report.skipped), (2, 0, 0));
    for asset in ["a", "b"] {
        let remote = fixture.root().join("output").join(asset);
        assert!(remote.join("_complete.json").exists());
        assert_eq!(
            std::fs::read(remote.join("cbcs/file.m4s")).unwrap(),
            asset.as_bytes()
        );
        assert!(!config.output_root.join(asset).exists());
        let marker: Value =
            serde_json::from_slice(&std::fs::read(remote.join("_complete.json")).unwrap()).unwrap();
        assert_eq!(
            marker,
            json!({"schema_version": "1", "asset_id": asset, "outcome": "done-pass", "object_count": 1})
        );
    }
    let report2 = run(&[item("a"), item("b")], config, &fail, &seams_no_stage()).unwrap();
    assert_eq!((report2.passed, report2.skipped), (2, 2));
    let records = logs.records();
    let starts = records
        .iter()
        .filter(|r| r["message"] == "asset-start")
        .count();
    assert_eq!(starts, 2);
}

#[test]
fn test_missing_key_and_exhausted_upload_are_local_quarantine() {
    let fixture = fixture();
    let broken = Wrapped::new(&fixture.root(), |_, _| {
        Err(StorageError::retryable("storage-transient"))
    });
    let config = BatchConfig {
        storage: Arc::new(broken),
        ..fixture.config.clone()
    };
    let report = run(
        &[item("missing"), item("upload")],
        &config,
        &runner,
        &seams_no_stage(),
    )
    .unwrap();
    assert_eq!(report.failed, 2);
    assert_eq!(
        reasons(&report),
        [
            ("missing-key", 1),
            ("retries-exhausted:storage-transient", 1)
        ]
    );
    assert_eq!(load_resume_log(&config.resume_log).unwrap().len(), 2);
    assert_eq!(
        find_named(&fixture.root(), "_complete.json"),
        Vec::<PathBuf>::new()
    );
}

#[test]
fn test_crash_no_terminal_and_retry_failed_explicit() {
    let fixture = fixture();
    let config = &fixture.config;
    let crash = |_: &AssetWorkItem,
                 _: &BatchConfig,
                 _: &Path,
                 _: &KeyMaterial,
                 _: &dyn Fn()|
     -> Result<i32, AttemptError> {
        Err(AttemptError::Interrupted("KeyboardInterrupt".to_string()))
    };
    let error = run(&[item("a")], config, &crash, &seams_no_stage()).unwrap_err();
    assert!(matches!(error, RunError::Interrupted(_)));
    assert!(load_resume_log(&config.resume_log).unwrap().is_empty());
    assert!(
        !config.output_root.join("a").exists(),
        "the scratch is removed on a crash too"
    );
    let content_fail = |_: &AssetWorkItem,
                        _: &BatchConfig,
                        _: &Path,
                        _: &KeyMaterial,
                        _: &dyn Fn()|
     -> Result<i32, AttemptError> { Ok(1) };
    let report = run(&[item("a")], config, &content_fail, &seams_no_stage()).unwrap();
    assert_eq!(report.failed, 1);
    assert_eq!(report.outcomes[0].reason, "content-fail");
    assert_eq!(
        run(&[item("a")], config, &runner, &seams_no_stage())
            .unwrap()
            .skipped,
        1
    );
    let retrying = BatchConfig {
        retry_failed: true,
        ..config.clone()
    };
    assert_eq!(
        run(&[item("a")], &retrying, &runner, &seams_no_stage())
            .unwrap()
            .passed,
        1
    );
}

#[test]
fn test_plan_has_no_storage_or_writes() {
    let fixture = fixture();
    let before = snapshot_tree(fixture.temp.path());
    let plan = plan_batch(&[item("a")], &fixture.config.resume_log, false).unwrap();
    assert_eq!(
        Value::Object(plan[0].clone()),
        json!({"asset_id": "a", "action": "run", "reason": null})
    );
    assert_eq!(snapshot_tree(fixture.temp.path()), before);
    assert!(!fixture.config.resume_log.exists());
}

#[test]
fn test_keyfile_permissions_location_and_missing() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("private.jsonl");
    let row = json!({"asset_id": "a", "kid": KID, "key": KEY, "iv": IV});
    write_mode(
        &path,
        format!("{}\n", Dumps::DEFAULT.dumps(&row)).as_bytes(),
        0o600,
    );
    let provider = FileKeyProvider::new(&path, &temp.path().join("output")).unwrap();
    assert_eq!(provider.key_material("a").unwrap(), key());
    assert_eq!(
        provider.key_material("absent").unwrap_err(),
        KeyNotAvailable
    );
    let shown = format!("{provider:?}");
    assert!(
        !shown.contains(path.to_str().unwrap()) && !shown.contains(KEY),
        "{shown}"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let error = FileKeyProvider::new(&path, &temp.path().join("output")).unwrap_err();
    assert_eq!(error.token(), "key-provider-invalid");
    assert!(!error.to_string().contains(path.to_str().unwrap()));
}

#[test]
fn key_files_are_refused_under_the_output_root_or_malformed() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("output");
    let inside = output.join("keys.jsonl");
    let row = Dumps::DEFAULT.dumps(&json!({"asset_id": "a", "kid": KID, "key": KEY, "iv": IV}));
    write_mode(&inside, format!("{row}\n").as_bytes(), 0o600);
    assert!(FileKeyProvider::new(&inside, &output).is_err());
    let path = temp.path().join("keys.jsonl");
    for content in [
        format!("{row}\n{row}\n"),
        format!("{row}\n\n"),
        "{\"asset_id\": \"a\", \"kid\": \"00\", \"key\": \"00\", \"iv\": \"00\"}\n".to_string(),
        "{\"asset_id\": \"a\", \"kid\": 1, \"key\": 2, \"iv\": 3, \"x\": 4}\n".to_string(),
        // `load_key_material` of values that are not text: truthy (`bytes.fromhex(5)`, a
        // `TypeError`), falsy (missing) and empty (missing).
        Dumps::DEFAULT.dumps(&json!({"asset_id": "a", "kid": KID, "key": KEY, "iv": 5})) + "\n",
        Dumps::DEFAULT.dumps(&json!({"asset_id": "a", "kid": KID, "key": KEY, "iv": 0})) + "\n",
        Dumps::DEFAULT.dumps(&json!({"asset_id": "a", "kid": KID, "key": KEY, "iv": null})) + "\n",
        Dumps::DEFAULT.dumps(&json!({"asset_id": "a", "kid": KID, "key": KEY, "iv": ""})) + "\n",
        Dumps::DEFAULT.dumps(&json!({"asset_id": "a", "kid": [KID], "key": KEY, "iv": IV})) + "\n",
    ] {
        write_mode(&path, content.as_bytes(), 0o600);
        assert_eq!(
            FileKeyProvider::new(&path, &output).unwrap_err().token(),
            "key-provider-invalid"
        );
    }
    write_mode(&path, b"", 0o600);
    assert_eq!(
        FileKeyProvider::new(&path, &output)
            .unwrap()
            .key_material("a")
            .unwrap_err(),
        KeyNotAvailable
    );
    std::os::unix::fs::symlink(&path, temp.path().join("link.jsonl")).unwrap();
    assert!(FileKeyProvider::new(&temp.path().join("link.jsonl"), &output).is_err());
    // A key file under the run's output root is refused by the configuration too.
    let store = tempfile::tempdir().unwrap();
    let config_error = BatchConfig {
        keys: Arc::new(FileKeyProvider::new(&path, &output).unwrap()),
        ..BatchConfig::new(
            Arc::new(LocalStorageBackend::new(store.path()).unwrap()),
            Arc::new(EnvKeyProvider),
            temp.path(),
            &temp.path().join("r"),
        )
    };
    assert_eq!(
        config_error.validate().unwrap_err(),
        RunError::Batch(BatchError::new("key-provider-invalid"))
    );
}

#[test]
fn test_marker_exhaustion_is_quarantined_without_success() {
    let fixture = fixture();
    let marker_fails = Wrapped::new(&fixture.root(), |inner, items| {
        if items
            .iter()
            .any(|(_, object)| object.key().ends_with("/_complete.json"))
        {
            return Err(StorageError::retryable("storage-transient"));
        }
        inner.put_many(items)
    });
    let config = BatchConfig {
        storage: Arc::new(marker_fails),
        ..fixture.config.clone()
    };
    let report = run(&[item("a")], &config, &runner, &seams_no_stage()).unwrap();
    assert_eq!((report.failed, report.retried), (1, 1));
    assert_eq!(
        report.outcomes[0].reason,
        "retries-exhausted:storage-transient"
    );
    assert!(!fixture.root().join("output/a/_complete.json").exists());
}

#[test]
fn a_marker_the_store_does_not_show_is_retried_and_never_published() {
    // The marker upload is accepted but the object never becomes visible: `exists` is the
    // verification, so done-pass is never recorded.
    let fixture = fixture();
    let lost = Wrapped::new(&fixture.root(), |inner, items| {
        if items
            .iter()
            .any(|(_, object)| object.key().ends_with("/_complete.json"))
        {
            return Ok(());
        }
        inner.put_many(items)
    });
    let config = BatchConfig {
        storage: Arc::new(lost),
        ..fixture.config.clone()
    };
    let report = run(&[item("a")], &config, &runner, &seams_no_stage()).unwrap();
    assert_eq!(
        report.outcomes[0].reason,
        "retries-exhausted:storage-transient"
    );
    assert_eq!(report.outcomes[0].attempt_count(), Some(2));
}

#[test]
fn test_actual_stage_uses_download_mirror_and_preserves_source() {
    let fixture = fixture();
    let built = build_synthetic_dash_asset(
        &fixture.root(),
        &[h264_rep("v"), audio_rep("a")],
        "title-abc123",
    );
    let download = fixture.root().join("download");
    let config = BatchConfig {
        storage: Arc::new(LocalStorageBackend::new(&download).unwrap()),
        ..fixture.config.clone()
    };
    let manifest = relative_key(&built.mpd_path, &download);
    let work = AssetWorkItem::new("staged", "dash-cenc", Some(manifest.clone()), None).unwrap();
    let before = snapshot_tree(&download);
    let stage = stage_inputs(&work, &config, &DefaultHlsSource).unwrap();
    // Python loads the staged MPD with `config.load_asset`: 2 renditions of 3 segments.
    let asset = drmr_core::config::load_asset(
        &stage.join(manifest.key()),
        key(),
        &config.output_root,
        Some("staged"),
    )
    .unwrap();
    assert_eq!(asset.renditions.len(), 2);
    assert!(asset.renditions.iter().all(|r| r.segment_paths.len() == 3));
    // The mirror, file by file.
    assert_eq!(
        stage,
        config
            .output_root
            .join("staged/clear/_batch-stage/download")
    );
    assert_eq!(
        std::fs::read(stage.join(manifest.key())).unwrap(),
        std::fs::read(&built.mpd_path).unwrap()
    );
    for (kind, rep) in [("video", "v"), ("audio", "a")] {
        let directory = stage
            .join("cdn.example.test/title-abc123")
            .join(kind)
            .join(rep);
        assert_eq!(
            std::fs::read(directory.join("init.mp4")).unwrap(),
            format!("INIT:{rep}").as_bytes()
        );
        for index in 0..3 {
            let segment = directory.join(format!("seg_{index}.m4s"));
            assert_eq!(
                std::fs::read(&segment).unwrap(),
                format!("SEG:{rep}:{index}").as_bytes()
            );
            assert!(!segment.is_symlink());
        }
    }
    assert_eq!(snapshot_tree(&download), before);
}

#[test]
fn supervisor_heartbeat_handshake_releases_the_worker_after_two_events() {
    let fixture = fixture();
    let heartbeat = HeartbeatRelease {
        release: fixture.temp.path().join("release"),
        beats: AtomicUsize::new(0),
    };
    let seams = Seams {
        preflight: Some(&no_preflight),
        worker_command: Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            "cat > /dev/null; while [ ! -f \"$1\" ]; do sleep 0.01; done; printf '[\"ok\", false]\\n'".into(),
            "heartbeat-worker".into(),
            heartbeat.release.clone().into(),
        ]),
        ..Seams::default()
    };
    let config = BatchConfig {
        retry: RetryPolicy {
            max_attempts: 1,
            per_asset_timeout_s: 5.0,
            heartbeat_s: 0.03,
            ..RetryPolicy::default()
        },
        ..fixture.config.clone()
    };
    let report = run_batch_with(&[item("a")], &config, Some(&heartbeat), None, &seams).unwrap();
    assert_eq!(report.outcomes[0].reason, "ok");
    assert!(heartbeat.beats.load(Ordering::SeqCst) >= 2);
    assert!(heartbeat.release.is_file());
}

// Deadline checks take priority over missed heartbeat intervals. A scheduler pause can
// consume a short timeout, so heartbeat delivery needs a child-release handshake rather
// than a callback-count assumption within the descendant-kill tests' 0.3-second deadline.
struct HeartbeatRelease {
    release: PathBuf,
    beats: AtomicUsize,
}

impl HeartbeatRelease {
    fn beat(&self) {
        if self.beats.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
            std::fs::write(&self.release, b"release").unwrap();
        }
    }
}

impl Sink for HeartbeatRelease {
    fn emit(&self, record: &Record) {
        if record.get("message").and_then(Value::as_str) == Some("heartbeat") {
            self.beat();
        }
    }
}

#[test]
fn test_timeout_kills_descendants() {
    let fixture = fixture();
    let pidfile = fixture.temp.path().join("child.pid");
    let script = "sleep 60 & echo $! > \"$1\"; sleep 60";
    let seams = Seams {
        preflight: Some(&no_preflight),
        worker_command: Some(vec![
            "/bin/sh".into(),
            "-c".into(),
            script.into(),
            "sh".into(),
            pidfile.clone().into(),
        ]),
        ..Seams::default()
    };
    let config = BatchConfig {
        retry: RetryPolicy {
            max_attempts: 1,
            per_asset_timeout_s: 0.3,
            heartbeat_s: 0.03,
            ..RetryPolicy::default()
        },
        ..fixture.config.clone()
    };
    let start = Instant::now();
    let report = run_batch_with(&[item("a")], &config, None, None, &seams).unwrap();
    assert_eq!(report.outcomes[0].reason, "retries-exhausted:asset-timeout");
    assert!(start.elapsed() < Duration::from_secs(5));
    let pid = std::fs::read_to_string(&pidfile).unwrap();
    let state = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid.trim()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&state.stdout).trim().to_string();
    assert!(
        state.is_empty() || state.starts_with('Z'),
        "the grandchild survived: {state}"
    );
}

#[test]
fn test_whole_asset_budget_includes_storage_phases() {
    for hang in ["stage", "upload"] {
        let fixture = fixture();
        // A worker that reads its private configuration and stalls in either storage phase.
        let script = "cat > /dev/null; sleep 60";
        let seams = Seams {
            preflight: Some(&no_preflight),
            worker_command: Some(vec![
                "/bin/sh".into(),
                "-c".into(),
                script.into(),
                hang.into(),
            ]),
            ..Seams::default()
        };
        let config = BatchConfig {
            retry: RetryPolicy {
                max_attempts: 1,
                per_asset_timeout_s: 0.15,
                heartbeat_s: 0.03,
                ..RetryPolicy::default()
            },
            ..fixture.config.clone()
        };
        let start = Instant::now();
        let report = run_batch_with(&[item("a")], &config, None, None, &seams).unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{hang}: {:?}",
            start.elapsed()
        );
        assert_eq!(report.outcomes[0].reason, "retries-exhausted:asset-timeout");
        assert!(!fixture.root().join("output/a/_complete.json").exists());
    }
}

#[test]
fn test_only_incomplete_final_resume_record_is_recovered() {
    let fixture = fixture();
    let log = &fixture.config.resume_log;
    append_resume_entry(
        log,
        &ResumeEntry::new("a", AssetOutcome::DonePass, "ok", 1, "now").unwrap(),
    )
    .unwrap();
    append_bytes(log, b"{\"asset_id\":\"interrupted\",");
    assert_eq!(load_resume_log(log).unwrap().asset_ids(), ["a"]);
    append_resume_entry(
        log,
        &ResumeEntry::new("b", AssetOutcome::DoneFail, "missing-key", 1, "later").unwrap(),
    )
    .unwrap();
    assert_eq!(load_resume_log(log).unwrap().asset_ids(), ["a", "b"]);
    append_bytes(log, b"{\"invalid\":\n");
    assert_eq!(
        load_resume_log(log).unwrap_err().token(),
        "resume-log-invalid"
    );
}

fn append_bytes(path: &Path, data: &[u8]) {
    use std::io::Write as _;
    let mut handle = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    handle.write_all(data).unwrap();
}

#[test]
fn test_stage_cumulative_byte_budget_and_codec_alias_rejected() {
    let fixture = fixture();
    let built = build_synthetic_dash_asset(&fixture.root(), &[h264_rep("v")], "title-abc123");
    let download = fixture.root().join("download");
    let storage: Arc<dyn StorageBackend> = Arc::new(LocalStorageBackend::new(&download).unwrap());
    let source = relative_key(&built.mpd_path, &download);
    let work = AssetWorkItem::new("stage-limit", "dash-cenc", Some(source), None).unwrap();
    let size = i64::try_from(std::fs::metadata(&built.mpd_path).unwrap().len()).unwrap();
    let limited = BatchConfig {
        storage: Arc::clone(&storage),
        transfer: TransferPolicy {
            max_asset_bytes: size + 1,
            ..TransferPolicy::default()
        },
        ..fixture.config.clone()
    };
    let error = stage_inputs(&work, &limited, &DefaultHlsSource).unwrap_err();
    assert_eq!(
        error,
        AttemptError::Storage(StorageError::new("storage-byte-budget"))
    );
    let text = std::fs::read_to_string(&built.mpd_path).unwrap();
    std::fs::write(&built.mpd_path, text.replace("avc1.42C00C", "avc3.42C00C")).unwrap();
    let config = BatchConfig {
        storage,
        ..fixture.config.clone()
    };
    let error = stage_inputs(&work, &config, &DefaultHlsSource).unwrap_err();
    assert_eq!(
        error,
        AttemptError::Batch(BatchError::new("unsupported-codec"))
    );
}

#[test]
fn test_declared_source_and_resume_paths_cannot_be_deleted_as_output() {
    let fixture = fixture();
    let source = fixture.root().join("output/a/source.mpd");
    write(&source, b"must-survive");
    let declared = AssetWorkItem {
        dash_manifest: Some(ObjectRef::new("output/a/source.mpd").unwrap()),
        ..item("a")
    };
    let error = run(&[declared], &fixture.config, &runner, &seams()).unwrap_err();
    assert_eq!(
        error,
        RunError::Batch(BatchError::new("source-output-overlap"))
    );
    assert_eq!(std::fs::read(&source).unwrap(), b"must-survive");
    let overlapping = BatchConfig {
        resume_log: fixture.config.output_root.join("a/resume"),
        ..fixture.config.clone()
    };
    let error = run(&[item("a")], &overlapping, &runner, &seams()).unwrap_err();
    assert_eq!(
        error,
        RunError::Batch(BatchError::new("resume-output-overlap"))
    );
}

#[test]
fn test_entire_source_graph_checked_before_output_deletion() {
    for overlap in ["dash-base", "nested-hls"] {
        let fixture = fixture();
        let built = build_synthetic_dash_asset(&fixture.root(), &[h264_rep("v")], "title-abc123");
        let download = fixture.root().join("download");
        let manifest = relative_key(&built.mpd_path, &download);
        let protected = download.join("output/a/source-segment");
        write(&protected, b"original-source-never-delete");
        let mut master = None;
        if overlap == "dash-base" {
            let text = std::fs::read_to_string(&built.mpd_path).unwrap();
            std::fs::write(
                &built.mpd_path,
                text.replace(
                    "https://cdn.example.test/title-abc123/",
                    "https://output/a/",
                ),
            )
            .unwrap();
        } else {
            let hls = download.join("manifest.example.test/title-abc123/hls");
            write(
                &hls.join("master.m3u8"),
                "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nnested.m3u8\n",
            );
            write(
                &hls.join("nested.m3u8"),
                "#EXTM3U\n#EXTINF:6,\nhttps://output/a/source-segment\n#EXT-X-ENDLIST\n",
            );
            master = Some(relative_key(&hls.join("master.m3u8"), &download));
        }
        let config = BatchConfig {
            storage: Arc::new(LocalStorageBackend::new(&download).unwrap()),
            ..fixture.config.clone()
        };
        let work = AssetWorkItem::new("a", "dash-cenc", Some(manifest), master).unwrap();
        let report = run(&[work], &config, &runner, &seams()).unwrap();
        assert_eq!(report.failed, 1, "{overlap}");
        assert_eq!(report.outcomes[0].reason, "malformed-manifest", "{overlap}");
        assert_eq!(
            std::fs::read(&protected).unwrap(),
            b"original-source-never-delete"
        );
    }
}

#[test]
fn test_query_bearing_hls_references_lookup_queryless_object_and_preserve_manifest() {
    let fixture = fixture();
    let built = build_synthetic_dash_asset(&fixture.root(), &[h264_rep("v")], "title-abc123");
    let download = fixture.root().join("download");
    let hls = download.join("manifest.example.test/title-abc123/hls");
    let original = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nmedia.m3u8?vt=123\n";
    write(&hls.join("master.m3u8"), original);
    write(
        &hls.join("media.m3u8"),
        "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4?vt=123\"\n#EXTINF:6,\nseg.ts?vt=123\n#EXT-X-ENDLIST\n",
    );
    write(&hls.join("init.mp4"), b"init");
    write(&hls.join("seg.ts"), b"segment");
    let work = AssetWorkItem::new(
        "query",
        "dash-cenc",
        Some(relative_key(&built.mpd_path, &download)),
        Some(relative_key(&hls.join("master.m3u8"), &download)),
    )
    .unwrap();
    let config = BatchConfig {
        storage: Arc::new(LocalStorageBackend::new(&download).unwrap()),
        ..fixture.config.clone()
    };
    let stage = stage_inputs(&work, &config, &DefaultHlsSource).unwrap();
    assert_eq!(
        std::fs::read(stage.join(work.hls_master.as_ref().unwrap().key())).unwrap(),
        original
    );
    assert_eq!(
        std::fs::read(stage.join("manifest.example.test/title-abc123/hls/seg.ts")).unwrap(),
        b"segment"
    );
    assert_eq!(
        std::fs::read(stage.join("manifest.example.test/title-abc123/hls/init.mp4")).unwrap(),
        b"init"
    );
}

#[test]
fn test_segment_census_does_not_consume_playlist_budget() {
    let fixture = fixture();
    let built = build_synthetic_dash_asset(&fixture.root(), &[h264_rep("v")], "title-abc123");
    let download = fixture.root().join("download");
    let hls = download.join("manifest.example.test/title-abc123/hls");
    write(
        &hls.join("master.m3u8"),
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nsmall.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=200\nlarge.m3u8\n",
    );
    write(
        &hls.join("small.m3u8"),
        "#EXTM3U\n#EXTINF:6,\ns0.ts\n#EXT-X-ENDLIST\n",
    );
    write(&hls.join("s0.ts"), b"small");
    let large = (0..600).fold(String::new(), |mut out, index| {
        writeln!(out, "#EXTINF:6,\ns{index}.ts").unwrap();
        out
    });
    write(
        &hls.join("large.m3u8"),
        format!("#EXTM3U\n{large}#EXT-X-ENDLIST\n"),
    );
    for index in 1..600 {
        write(&hls.join(format!("s{index}.ts")), b"segment");
    }
    let work = AssetWorkItem::new(
        "large-playlist",
        "dash-cenc",
        Some(relative_key(&built.mpd_path, &download)),
        Some(relative_key(&hls.join("master.m3u8"), &download)),
    )
    .unwrap();
    let config = BatchConfig {
        storage: Arc::new(LocalStorageBackend::new(&download).unwrap()),
        ..fixture.config.clone()
    };
    let stage = stage_inputs(&work, &config, &DefaultHlsSource).unwrap();
    assert_eq!(
        std::fs::read(stage.join("manifest.example.test/title-abc123/hls/s599.ts")).unwrap(),
        b"segment"
    );
}

/// A clear-HLS snapshot frozen by the real `analyze_hls` (the source implementation) from a local source
/// under `inputs/`, its segment probed as one AAC stream (Python replaces
/// `source::_probe` the same way).
fn freeze_snapshot(output_root: &Path, inputs: &Path, asset_id: &str) -> SourceAnalysis {
    write(
        &inputs.join("master.m3u8"),
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:1,\ns.ts\n#EXT-X-ENDLIST\n",
    );
    write(&inputs.join("s.ts"), source_segment());
    let prober = |_: &Path, _: Option<&Path>| -> Result<Vec<ProbeStream>, Failure> {
        let stream = json!({"index": 0, "codec_type": "audio", "codec_name": "aac"});
        Ok(vec![stream.as_object().unwrap().clone()])
    };
    let policy = FetchPolicy {
        allowed_local_roots: vec![inputs.to_path_buf()],
        ..FetchPolicy::default()
    };
    let options = AnalyzeOptions {
        on_log: None,
        prober: &prober,
        platform: Arc::new(SystemPlatform),
        hooks: &Direct,
    };
    let request = SourceRequest::new(inputs.join("master.m3u8").to_str().unwrap());
    analyze_hls_with(&request, asset_id, output_root, &policy, &options).unwrap()
}

/// The source segment: one TS sync byte and 187 bytes.
fn source_segment() -> Vec<u8> {
    let mut segment = vec![0x47u8];
    segment.extend(std::iter::repeat_n(b'x', 187));
    segment
}

fn profiles(directory: &Path, entries: &[(&str, Value)]) {
    for (name, body) in entries {
        write_mode(
            &directory.join(name),
            Dumps::DEFAULT.dumps(body).as_bytes(),
            0o600,
        );
    }
}

#[test]
fn test_hls_adapter_reuses_verified_snapshot_and_recovers_only_partial() {
    let fixture = fixture();
    let directory = fixture.temp.path().join("profiles");
    profiles(
        &directory,
        &[
            (
                "a.json",
                json!({"source_config": "source.json", "drm_config": "drm.json"}),
            ),
            ("source.json", json!({"url": "unused"})),
            ("drm.json", json!({"widevine_pssh": "unused"})),
        ],
    );
    let inputs = fixture.temp.path().join("original");
    let analysis = freeze_snapshot(&fixture.config.output_root, &inputs, "a");
    let config = BatchConfig {
        hls_config_dir: Some(directory),
        ..fixture.config.clone()
    };
    let work = AssetWorkItem::new("a", "hls-clear", None, None).unwrap();
    let stage = fixture.temp.path().join("stage");
    let planned = commands(&work, &config, &stage, &DefaultHlsSource).unwrap();
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].subcommand, "package-hls");
    let segment = &analysis.tracks[0].segments[0].path;
    std::fs::write(segment, b"changed-admitted-bytes").unwrap();
    let error = commands(&work, &config, &stage, &DefaultHlsSource).unwrap_err();
    assert_eq!(
        error,
        AttemptError::Batch(BatchError::new("hls-config-invalid"))
    );
    assert_eq!(std::fs::read(segment).unwrap(), b"changed-admitted-bytes");
    std::fs::remove_file(&analysis.snapshot_path).unwrap();
    let planned = commands(&work, &config, &stage, &DefaultHlsSource).unwrap();
    let subcommands: Vec<&str> = planned.iter().map(|c| c.subcommand.as_str()).collect();
    assert_eq!(subcommands, ["analyze-hls", "package-hls"]);
    assert!(!analysis.snapshot_path.parent().unwrap().exists());
    assert_eq!(
        std::fs::read(inputs.join("s.ts")).unwrap(),
        source_segment()
    );
}

#[test]
fn test_all_graphs_preflight_before_first_asset_can_delete_another_input() {
    let fixture = fixture();
    let first = build_synthetic_dash_asset(&fixture.root(), &[h264_rep("v")], "first");
    let second = build_synthetic_dash_asset(&fixture.root(), &[h264_rep("v")], "second");
    let download = fixture.root().join("download");
    let protected = download.join("output/a/source-segment");
    write(&protected, b"future-asset-input");
    let hls = download.join("manifest.example.test/second/hls");
    write(
        &hls.join("master.m3u8"),
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nnested.m3u8\n",
    );
    write(
        &hls.join("nested.m3u8"),
        "#EXTM3U\n#EXTINF:6,\nhttps://output/a/source-segment\n#EXT-X-ENDLIST\n",
    );
    let work = [
        AssetWorkItem::new(
            "a",
            "dash-cenc",
            Some(relative_key(&first.mpd_path, &download)),
            None,
        )
        .unwrap(),
        AssetWorkItem::new(
            "b",
            "dash-cenc",
            Some(relative_key(&second.mpd_path, &download)),
            Some(relative_key(&hls.join("master.m3u8"), &download)),
        )
        .unwrap(),
    ];
    let config = BatchConfig {
        storage: Arc::new(LocalStorageBackend::new(&download).unwrap()),
        ..fixture.config.clone()
    };
    let error = run(&work, &config, &fail, &Seams::default()).unwrap_err();
    assert_eq!(
        error,
        RunError::Batch(BatchError::unsafe_source_graph("source-output-overlap"))
    );
    assert_eq!(std::fs::read(&protected).unwrap(), b"future-asset-input");
    assert!(!config.resume_log.exists());
    assert!(!download.join("output/a/cbcs").exists());
}

#[test]
fn test_baseurl_cache_query_uses_same_download_mirror() {
    let fixture = fixture();
    let built = build_synthetic_dash_asset(&fixture.root(), &[h264_rep("v")], "title-abc123");
    let text = std::fs::read_to_string(&built.mpd_path).unwrap();
    std::fs::write(
        &built.mpd_path,
        text.replace("title-abc123/</BaseURL>", "title-abc123/?vt=1</BaseURL>"),
    )
    .unwrap();
    let download = fixture.root().join("download");
    let config = BatchConfig {
        storage: Arc::new(LocalStorageBackend::new(&download).unwrap()),
        ..fixture.config.clone()
    };
    let work = AssetWorkItem::new(
        "cache",
        "dash-cenc",
        Some(relative_key(&built.mpd_path, &download)),
        None,
    )
    .unwrap();
    let stage = stage_inputs(&work, &config, &DefaultHlsSource).unwrap();
    assert!(stage.is_dir());
    assert!(
        stage
            .join("cdn.example.test/title-abc123/video/v/seg_2.m4s")
            .is_file()
    );
}

/// A store that only names a bucket (`class S3Location: bucket = …`): any call fails the test.
struct BucketOnly(&'static str);

impl StorageBackend for BucketOnly {
    fn exists(&self, _: &ObjectRef) -> Result<bool, StorageError> {
        panic!("no provider call")
    }
    fn get_many(&self, _: &[ObjectRef], _: &Path) -> Result<Vec<PathBuf>, StorageError> {
        panic!("no provider call")
    }
    fn put_many(&self, _: &[(PathBuf, ObjectRef)]) -> Result<(), StorageError> {
        panic!("no provider call")
    }
    fn delete_prefix(&self, _: &ObjectRef) -> Result<(), StorageError> {
        panic!("no provider call")
    }
    fn list_prefix(&self, _: &ObjectRef) -> Result<Vec<ObjectRef>, StorageError> {
        panic!("no provider call")
    }
    fn object_size(&self, _: &ObjectRef) -> Result<u64, StorageError> {
        panic!("no provider call")
    }
    fn bucket(&self) -> Option<&str> {
        Some(self.0)
    }
}

#[test]
fn test_hls_s3_source_ownership_rejects_owned_output_without_provider_calls() {
    let fixture = fixture();
    let config = BatchConfig {
        storage: Arc::new(BucketOnly("my-bucket")),
        ..fixture.config.clone()
    };
    for url in [
        "https://my-bucket.s3.ap-northeast-1.amazonaws.com/output/a/segment.ts",
        "https://s3.ap-northeast-1.amazonaws.com/my-bucket/%6futput/a/segment.ts",
    ] {
        assert!(source_overlaps_output(url, &config).unwrap(), "{url}");
    }
    for url in [
        "https://my-bucket.s3.ap-northeast-1.amazonaws.com/input/a/segment.ts",
        "https://other.s3.ap-northeast-1.amazonaws.com/output/a/segment.ts",
        "https://cdn.example.test/output2/a.ts",
    ] {
        assert!(!source_overlaps_output(url, &config).unwrap(), "{url}");
    }
    let error = source_overlaps_output(
        "https://x-123.s3-accesspoint.ap-northeast-1.amazonaws.com/o",
        &config,
    )
    .unwrap_err();
    assert_eq!(
        error,
        AttemptError::Batch(BatchError::unsafe_source_graph(
            "hls-source-ownership-unqualified"
        ))
    );
    // Any other URL is its mirror key: `netloc/path` under the prefix overlaps.
    assert!(source_overlaps_output("https://output/a.ts", &config).unwrap());
}

#[test]
fn test_global_preflight_unavailable_storage_never_quarantines_as_content() {
    let fixture = fixture();
    let mut unavailable = Wrapped::new(&fixture.root(), LocalStorageBackend::put_many);
    unavailable.size = Some(Box::new(|_| {
        Err(StorageError::retryable("storage-transient"))
    }));
    let unavailable = Arc::new(unavailable);
    let config = BatchConfig {
        storage: Arc::clone(&unavailable) as Arc<dyn StorageBackend>,
        ..fixture.config.clone()
    };
    let error = run(&[item("a")], &config, &runner, &Seams::default()).unwrap_err();
    assert_eq!(
        error,
        RunError::Storage(StorageError::retryable("storage-transient"))
    );
    assert!(matches!(&error, RunError::Storage(e) if e.is_retryable()));
    assert!(!config.resume_log.exists());
    assert_eq!(
        unavailable.deletes.load(Ordering::SeqCst),
        0,
        "unvalidated graph authorized deletion"
    );
}

/// The source implementation's functions with a fetcher that must never be read (the overlap is found
/// before any fetch).
struct NoFetch;

struct Unread;

impl PlaylistFetcher for Unread {
    fn read_playlist(&mut self, _: &str) -> Result<(Vec<u8>, String), AttemptError> {
        panic!("an unsafe graph was fetched")
    }
    fn close_all(&mut self) {}
}

impl HlsSource for NoFetch {
    fn read_analysis(&self, path: &Path) -> Result<String, AttemptError> {
        DefaultHlsSource.read_analysis(path)
    }
    fn open_fetcher(
        &self,
        policy: &Value,
        _: &Path,
    ) -> Result<Box<dyn PlaylistFetcher>, AttemptError> {
        assert!(policy.get("allowed_local_roots").is_some());
        Ok(Box::new(Unread))
    }
    fn lines(&self, body: &[u8]) -> Result<Vec<String>, AttemptError> {
        DefaultHlsSource.lines(body)
    }
    fn child_url(&self, base: &str, reference: &str) -> Result<String, AttemptError> {
        DefaultHlsSource.child_url(base, reference)
    }
}

#[test]
fn test_hls_sources_in_scratch_output_are_rejected_globally() {
    for storage_kind in ["local", "s3"] {
        let fixture = fixture();
        let original = fixture.config.output_root.join("a/original.m3u8");
        write(
            &original,
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:1,\noriginal.ts\n#EXT-X-ENDLIST\n",
        );
        write(
            &original.with_file_name("original.ts"),
            b"must-not-be-deleted",
        );
        let directory = fixture.temp.path().join("profiles");
        let root = fixture.config.output_root.to_str().unwrap();
        profiles(
            &directory,
            &[
                (
                    "b.json",
                    json!({"source_config": "source.json", "drm_config": "drm.json"}),
                ),
                (
                    "source.json",
                    json!({"url": original.to_str().unwrap(), "policy": {"allowed_local_roots": [root]}}),
                ),
                ("drm.json", json!({})),
            ],
        );
        let storage: Arc<dyn StorageBackend> = if storage_kind == "local" {
            Arc::clone(&fixture.config.storage)
        } else {
            Arc::new(BucketOnly("bucket"))
        };
        let config = BatchConfig {
            storage,
            hls_config_dir: Some(directory),
            ..fixture.config.clone()
        };
        let work = [AssetWorkItem::new("b", "hls-clear", None, None).unwrap()];
        // The real source implementation (the fetcher), and a fetcher that fails the test if it
        // is ever read: the overlap is found before any fetch.
        for source in [&DefaultHlsSource as &dyn HlsSource, &NoFetch] {
            let seams = Seams {
                source,
                ..Seams::default()
            };
            let error = run(&work, &config, &fail, &seams).unwrap_err();
            assert_eq!(
                error,
                RunError::Batch(BatchError::unsafe_source_graph("source-output-overlap")),
                "{storage_kind}"
            );
        }
        assert!(original.exists());
        assert_eq!(
            std::fs::read(original.with_file_name("original.ts")).unwrap(),
            b"must-not-be-deleted"
        );
    }
}

#[test]
fn test_unknown_preflight_branch_blocks_other_assets_before_deletion() {
    let fixture = fixture();
    let first = build_synthetic_dash_asset(&fixture.root(), &[h264_rep("v")], "first");
    let second = build_synthetic_dash_asset(&fixture.root(), &[h264_rep("v")], "second");
    let download = fixture.root().join("download");
    let protected = download.join("output/a/source-segment");
    write(&protected, b"future-input");
    let hls = download.join("manifest.example.test/second/hls");
    write(
        &hls.join("master.m3u8"),
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nhidden.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2\nmissing.m3u8\n",
    );
    write(
        &hls.join("hidden.m3u8"),
        "#EXTM3U\n#EXTINF:6,\nhttps://output/a/source-segment\n#EXT-X-ENDLIST\n",
    );
    let work = [
        AssetWorkItem::new(
            "a",
            "dash-cenc",
            Some(relative_key(&first.mpd_path, &download)),
            None,
        )
        .unwrap(),
        AssetWorkItem::new(
            "b",
            "dash-cenc",
            Some(relative_key(&second.mpd_path, &download)),
            Some(relative_key(&hls.join("master.m3u8"), &download)),
        )
        .unwrap(),
    ];
    let config = BatchConfig {
        storage: Arc::new(LocalStorageBackend::new(&download).unwrap()),
        ..fixture.config.clone()
    };
    let error = run(&work, &config, &fail, &Seams::default()).unwrap_err();
    assert_eq!(
        error,
        RunError::Batch(BatchError::new("source-preflight-unproven"))
    );
    assert_eq!(std::fs::read(&protected).unwrap(), b"future-input");
    assert!(!config.resume_log.exists());
}

/// A local clear-HLS source under `inputs/` (a master naming `media.m3u8`) and asset `a`'s
/// private profile pointing at it under `policy`.
fn local_clear_source(fixture: &Fixture, policy: &Value) -> (BatchConfig, PathBuf) {
    let inputs = fixture.temp.path().join("inputs");
    write(
        &inputs.join("master.m3u8"),
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nmedia.m3u8\n",
    );
    let directory = fixture.temp.path().join("profiles");
    let url = inputs.join("master.m3u8");
    profiles(
        &directory,
        &[
            (
                "a.json",
                json!({"source_config": "source.json", "drm_config": "drm.json"}),
            ),
            (
                "source.json",
                json!({"url": url.to_str().unwrap(), "policy": policy}),
            ),
            ("drm.json", json!({})),
        ],
    );
    let config = BatchConfig {
        hls_config_dir: Some(directory),
        ..fixture.config.clone()
    };
    (config, inputs)
}

fn clear_item() -> [AssetWorkItem; 1] {
    [AssetWorkItem::new("a", "hls-clear", None, None).unwrap()]
}

fn preflight_clear(config: &BatchConfig) -> Result<(), RunError> {
    preflight_source_graphs(&clear_item(), config, &DefaultHlsSource)
}

#[test]
fn clear_hls_graphs_preflight_through_the_source_port() {
    // the fetcher reads every playlist of a local graph its policy admits
    // (`allowed_local_roots`): a disjoint graph passes, a child inside the output is unsafe —
    // found only after both reads — and an unreadable playlist is unproven.
    let fixture = fixture();
    let roots = json!([fixture.temp.path().join("inputs").to_str().unwrap()]);
    let (config, inputs) = local_clear_source(&fixture, &json!({ "allowed_local_roots": roots }));
    let media = inputs.join("media.m3u8");
    assert_eq!(
        preflight_clear(&config).unwrap_err(),
        RunError::Batch(BatchError::new("source-preflight-unproven"))
    );
    write(
        &media,
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\ns.ts\n#EXT-X-ENDLIST\n",
    );
    preflight_clear(&config).unwrap();
    let owned = drmr_core::batch_support::as_uri(
        &drmr_core::batch_support::resolve(&config.output_root.join("a/s.ts")).unwrap(),
    )
    .unwrap();
    write(
        &media,
        format!("#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\n{owned}\n#EXT-X-ENDLIST\n"),
    );
    assert_eq!(
        preflight_clear(&config).unwrap_err(),
        RunError::Batch(BatchError::unsafe_source_graph("source-output-overlap"))
    );
    // Each graph's temporary mirror is reclaimed.
    assert_eq!(
        common::names_with_prefix(&config.output_root.join("_batch"), "preflight-"),
        Vec::<String>::new()
    );
}

#[test]
fn clear_hls_fetch_policies_are_python_s_fetch_policy() {
    let fixture = fixture();
    let roots = json!([fixture.temp.path().join("inputs").to_str().unwrap()]);
    let media = fixture.temp.path().join("inputs/media.m3u8");
    write(
        &media,
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\ns.ts\n#EXT-X-ENDLIST\n",
    );
    for policy in [
        // The Rust tool's own analysis bound is an unknown keyword to Python's `FetchPolicy`.
        json!({"allowed_local_roots": roots, "max_total_resources": 9}),
        json!({"allowed_local_roots": roots, "bogus": 1}),
        json!({"allowed_local_roots": roots, "attempts": 6}),
        json!({"allowed_local_roots": roots, "timeout": true}),
        json!({"allowed_local_roots": [1]}),
        Value::Null,
        json!("ab"),
        // `dict("")` is an empty policy: no local root is admitted.
        json!(""),
    ] {
        let (config, _) = local_clear_source(&fixture, &policy);
        assert_eq!(
            preflight_clear(&config).unwrap_err(),
            RunError::Batch(BatchError::new("source-preflight-unproven")),
            "{policy}"
        );
    }
    // `dict` of `[key, value]` pairs.
    let (config, _) = local_clear_source(&fixture, &json!([["allowed_local_roots", roots]]));
    preflight_clear(&config).unwrap();
}

#[test]
fn clear_hls_preflight_reads_a_transient_source_failure_as_unavailable() {
    use std::io::{BufRead as _, BufReader, Write as _};
    // An origin answering 503 to every request: `source-http-failed`, a transient source
    // failure (Python's own fixture server answers the same way).
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line == "\r\n" => break,
                    Ok(_) => {}
                }
            }
            let _ = stream.write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        }
    });
    let fixture = fixture();
    let origin = format!("http://127.0.0.1:{port}");
    let directory = fixture.temp.path().join("profiles");
    profiles(
        &directory,
        &[
            (
                "a.json",
                json!({"source_config": "source.json", "drm_config": "drm.json"}),
            ),
            (
                "source.json",
                json!({"url": format!("{origin}/master.m3u8"), "policy": {
                    "allowed_origins": [origin], "allow_http": true,
                    "allow_private_network": true, "attempts": 1}}),
            ),
            ("drm.json", json!({})),
        ],
    );
    let config = BatchConfig {
        hls_config_dir: Some(directory),
        ..fixture.config.clone()
    };
    assert_eq!(
        preflight_clear(&config).unwrap_err(),
        RunError::Batch(BatchError::new("source-preflight-unavailable"))
    );
}

#[test]
fn an_analysis_record_that_is_not_an_object_escapes_commands_unclassified() {
    // `read_analysis` reads `data.get(…)` of a JSON array: an `AttributeError`, which
    // `_commands`' `except (OSError, ValueError)` lets escape (unclassified); an object
    // without the snapshot fields is `hls-config-invalid`.
    let fixture = fixture();
    let directory = fixture.temp.path().join("profiles");
    profiles(
        &directory,
        &[
            (
                "a.json",
                json!({"source_config": "source.json", "drm_config": "drm.json"}),
            ),
            ("source.json", json!({"url": "unused"})),
            ("drm.json", json!({})),
        ],
    );
    let config = BatchConfig {
        hls_config_dir: Some(directory),
        ..fixture.config.clone()
    };
    let analysis = config.output_root.join("a/clear/source/analysis.json");
    let work = AssetWorkItem::new("a", "hls-clear", None, None).unwrap();
    let stage = fixture.temp.path().join("stage");
    write(&analysis, b"[]");
    assert_eq!(
        commands(&work, &config, &stage, &DefaultHlsSource).unwrap_err(),
        AttemptError::Other
    );
    let report = run(&clear_item(), &config, &fail, &seams()).unwrap();
    assert_eq!(reasons(&report), [("unclassified", 1)]);
    write(&analysis, b"{}");
    assert_eq!(
        commands(&work, &config, &stage, &DefaultHlsSource).unwrap_err(),
        AttemptError::Batch(BatchError::new("hls-config-invalid"))
    );
}

#[test]
fn test_real_analysis_child_transient_segment_failure_crosses_boundary_safely() {
    // Python runs the real `analyze-hls` against a fixture server answering 503 for the
    // segment. This crate's tests cannot spawn the `drmr` binary (the clear-HLS batch runs
    // end to end, real `drmr analyze-hls` / `package-hls` included, in the differential
    // harness's `batch-hls-local`); the child here prints what `analyze-hls` prints — tool
    // records naming the signed URL, then the closing analysis record — and the classification
    // crosses the boundary without the URL.
    let fixture = fixture();
    let script = "echo '{\"stage\": \"tool\", \"argv\": [\"https://media.invalid/master?token=sentinel\"]}'; \
                  echo '{\"stage\": \"analysis\", \"level\": \"error\", \"message\": \"source-http-failed\"}'; exit 1";
    let command = CliCommand::new(["/bin/sh", "-c", script], "analyze-hls");
    let error = run_commands(&[command], &fixture.config, None, &|| {}).unwrap_err();
    assert_eq!(error, AttemptError::Retry("storage-transient".to_string()));
    assert!(!format!("{error} {error:?}").contains("sentinel"));
}

#[test]
fn test_analysis_child_transient_failure_is_found_behind_large_tool_output() {
    let fixture = fixture();
    let script = "x=$(printf '%500s' '' | tr ' ' x); i=0; while [ $i -lt 400 ]; do \
                  echo \"{\\\"stage\\\": \\\"tool\\\", \\\"event\\\": \\\"tool-output\\\", \\\"stdout\\\": \\\"$x\\\"}\"; i=$((i+1)); done; \
                  echo '{\"stage\": \"analysis\", \"level\": \"error\", \"message\": \"source-fetch-failed\"}'; exit 1";
    let command = CliCommand::new(["/bin/sh", "-c", script], "analyze-hls");
    let error = run_commands(&[command], &fixture.config, None, &|| {}).unwrap_err();
    assert_eq!(error, AttemptError::Retry("storage-transient".to_string()));
}

#[test]
fn test_first_resume_header_interruption_recovers_only_known_prefix() {
    for partial in [&b""[..], b"{\"schema_", b"{\"schema_version\": \""] {
        let fixture = fixture();
        let log = &fixture.config.resume_log;
        std::fs::write(log, partial).unwrap();
        assert!(load_resume_log(log).unwrap().is_empty());
        append_resume_entry(
            log,
            &ResumeEntry::new("a", AssetOutcome::DonePass, "ok", 1, "now").unwrap(),
        )
        .unwrap();
        assert_eq!(load_resume_log(log).unwrap().asset_ids(), ["a"]);
        std::fs::write(log, b"{\"unexpected\":").unwrap();
        assert_eq!(
            load_resume_log(log).unwrap_err().token(),
            "resume-log-invalid"
        );
    }
}

#[test]
fn test_retry_time_values_must_be_finite() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let policies = [
            RetryPolicy {
                per_asset_timeout_s: value,
                ..RetryPolicy::default()
            },
            RetryPolicy {
                heartbeat_s: value,
                ..RetryPolicy::default()
            },
            RetryPolicy {
                backoff_base_s: value,
                ..RetryPolicy::default()
            },
            RetryPolicy {
                backoff_max_s: value,
                ..RetryPolicy::default()
            },
        ];
        for policy in policies {
            assert_eq!(
                policy.validate().unwrap_err().token(),
                "retry-policy-invalid"
            );
        }
    }
    for policy in [
        RetryPolicy {
            max_attempts: 0,
            ..RetryPolicy::default()
        },
        RetryPolicy {
            max_attempts: 11,
            ..RetryPolicy::default()
        },
        RetryPolicy {
            per_asset_timeout_s: 0.0,
            ..RetryPolicy::default()
        },
        RetryPolicy {
            heartbeat_s: -1.0,
            ..RetryPolicy::default()
        },
        RetryPolicy {
            backoff_max_s: -1.0,
            ..RetryPolicy::default()
        },
    ] {
        assert!(policy.validate().is_err());
    }
}

#[test]
fn test_1000_asset_controlled_mechanism_crashes_resume_quarantine() {
    let fixture = fixture();
    let config = &fixture.config;
    let work: Vec<AssetWorkItem> = (0..1000)
        .map(|index| item(&format!("a{index:04}")))
        .collect();
    let crashed = Mutex::new(false);
    let crash_processing = |work: &AssetWorkItem,
                            config: &BatchConfig,
                            stage: &Path,
                            keys: &KeyMaterial,
                            beat: &dyn Fn()|
     -> Result<i32, AttemptError> {
        let mut crashed = crashed.lock().unwrap();
        if work.asset_id == "a0123" && !*crashed {
            *crashed = true;
            return Err(AttemptError::Interrupted("KeyboardInterrupt".to_string()));
        }
        runner(work, config, stage, keys, beat)
    };
    assert!(matches!(
        run(&work, config, &crash_processing, &seams_no_stage()),
        Err(RunError::Interrupted(_))
    ));
    assert_eq!(load_resume_log(&config.resume_log).unwrap().len(), 123);
    assert!(!fixture.root().join("output/a0123/_complete.json").exists());
    // A crash mid-upload: the upload of a0456's media lands, then the process dies.
    let crash_upload = Wrapped::new(&fixture.root(), |inner, items| {
        inner.put_many(items)?;
        assert!(
            !items
                .iter()
                .any(|(_, object)| object.key() == "output/a0456/cbcs/file.m4s"),
            "KeyboardInterrupt"
        );
        Ok(())
    });
    let uploading = BatchConfig {
        storage: Arc::new(crash_upload),
        ..config.clone()
    };
    let crashed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run(&work, &uploading, &runner, &seams_no_stage())
    }));
    assert!(crashed.is_err());
    assert_eq!(load_resume_log(&config.resume_log).unwrap().len(), 456);
    assert!(!fixture.root().join("output/a0456/_complete.json").exists());
    assert!(
        !config.output_root.join("a0456").exists(),
        "the scratch is removed while unwinding"
    );
    let corrupted = |work: &AssetWorkItem,
                     config: &BatchConfig,
                     stage: &Path,
                     keys: &KeyMaterial,
                     beat: &dyn Fn()|
     -> Result<i32, AttemptError> {
        if work.asset_id == "a0800" {
            Ok(1)
        } else {
            runner(work, config, stage, keys, beat)
        }
    };
    let report = run(&work, config, &corrupted, &seams_no_stage()).unwrap();
    assert_eq!(
        (report.total, report.passed, report.failed, report.skipped),
        (1000, 999, 1, 456)
    );
    assert_eq!(load_resume_log(&config.resume_log).unwrap().len(), 1000);
    assert_eq!(reasons(&report), [("ok", 999), ("content-fail", 1)]);
    let resumed = run(&work, config, &fail, &seams_no_stage()).unwrap();
    assert_eq!((resumed.skipped, resumed.failed), (1000, 1));
    for ident in ["a0000", "a0123", "a0456", "a0999"] {
        assert_eq!(
            std::fs::read(fixture.root().join(format!("output/{ident}/cbcs/file.m4s"))).unwrap(),
            ident.as_bytes()
        );
    }
    let leftovers: Vec<_> = std::fs::read_dir(&config.output_root)
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().is_dir() && entry.file_name() != "_batch")
        .collect();
    assert!(leftovers.is_empty());
}

#[test]
fn test_hls_dispatch_uses_private_configs_and_environment() {
    let fixture = fixture();
    let directory = fixture.temp.path().join("profiles");
    profiles(
        &directory,
        &[
            (
                "a.json",
                json!({"source_config": "source.json", "drm_config": "drm.json"}),
            ),
            (
                "source.json",
                json!({"url": "https://media.example/master?token=secret-sentinel"}),
            ),
            ("drm.json", json!({"widevine_pssh": "private-signaling"})),
        ],
    );
    let config = BatchConfig {
        hls_config_dir: Some(directory.clone()),
        max_workers: Some(3),
        ..fixture.config.clone()
    };
    let work = AssetWorkItem::new("a", "hls-clear", None, None).unwrap();
    let planned = commands(
        &work,
        &config,
        &fixture.temp.path().join("stage"),
        &DefaultHlsSource,
    )
    .unwrap();
    assert_eq!(planned[0].subcommand, "analyze-hls");
    assert_eq!(planned[1].subcommand, "package-hls");
    assert_eq!(planned[0].argv[1], "analyze-hls");
    assert_eq!(planned[1].argv[1], "package-hls");
    let shown = format!("{planned:?}");
    assert!(!shown.contains("secret-sentinel") && !shown.contains("private-signaling"));
    assert!(!planned[0].argv.contains(&OsString::from("--max-workers")));
    let position = planned[1]
        .argv
        .iter()
        .position(|arg| arg == "--max-workers")
        .unwrap();
    assert_eq!(
        planned[1].argv[position + 1],
        "3",
        "the per-asset worker bound reaches the clear-HLS packaging as it reaches run"
    );
    std::fs::set_permissions(
        directory.join("drm.json"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let error = commands(
        &work,
        &config,
        &fixture.temp.path().join("stage"),
        &DefaultHlsSource,
    )
    .unwrap_err();
    assert_eq!(
        error,
        AttemptError::Batch(BatchError::new("hls-config-invalid"))
    );
}

#[test]
fn dash_commands_are_the_run_command_with_python_s_flags() {
    let fixture = fixture();
    let config = BatchConfig {
        max_workers: Some(2),
        packager_bin: Some(PathBuf::from("/opt/packager")),
        ..fixture.config.clone()
    };
    let stage = Path::new("/stage/download");
    let mut work = item("a");
    let planned = commands(&work, &config, stage, &DefaultHlsSource).unwrap();
    let argv: Vec<String> = planned[0].argv[1..]
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let output = config.output_root.to_string_lossy().into_owned();
    assert_eq!(
        argv,
        [
            "run",
            "--mpd",
            "/stage/download/host/a/manifest.mpd",
            "--asset-id",
            "a",
            "--output",
            &output,
            "--log-format",
            "jsonl",
            "--no-manifests",
            "--max-workers",
            "2",
            "--packager-bin",
            "/opt/packager"
        ]
    );
    work.hls_master = Some(ObjectRef::new("host/a/master.m3u8").unwrap());
    let planned = commands(&work, &fixture.config, stage, &DefaultHlsSource).unwrap();
    let argv: Vec<String> = planned[0].argv[1..]
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        &argv[8..],
        [
            "jsonl",
            "--hls-master",
            "/stage/download/host/a/master.m3u8"
        ]
    );
}

#[test]
fn test_unknown_dispatch_and_unclassified_errors_quarantine() {
    let fixture = fixture();
    let work = AssetWorkItem {
        kind: "unrecognized".to_string(),
        ..item("unknown")
    };
    let raising = |_: &AssetWorkItem,
                   _: &BatchConfig,
                   _: &Path,
                   _: &KeyMaterial,
                   _: &dyn Fn()|
     -> Result<i32, AttemptError> { Err(AttemptError::Other) };
    let report = run(
        &[work, item("exception")],
        &fixture.config,
        &raising,
        &seams_no_stage(),
    )
    .unwrap();
    assert_eq!(
        reasons(&report),
        [("unsupported-kind", 1), ("unclassified", 1)]
    );
    let written =
        std::fs::read_to_string(fixture.config.output_root.join("_batch/batch-report.json"))
            .unwrap();
    assert!(!written.contains("secret-sentinel"));
}

#[test]
fn test_the_batch_worker_bound_is_one_to_sixty_four() {
    let fixture = fixture();
    for (bound, valid) in [(1, true), (64, true), (0, false), (65, false)] {
        let config = BatchConfig {
            max_workers: Some(bound),
            ..fixture.config.clone()
        };
        if valid {
            assert!(config.validate().is_ok());
        } else {
            assert_eq!(
                config.validate().unwrap_err(),
                RunError::Batch(BatchError::new("worker-bound-invalid"))
            );
        }
    }
}

// ------------------------------------------------------------------------------------------
// Byte formats recorded in tests/fixtures/batch/formats.json.
// ------------------------------------------------------------------------------------------

fn formats() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/batch/formats.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn the_resume_log_is_python_s_byte_for_byte() {
    let oracle = formats();
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("nested/resume.jsonl");
    for (asset, outcome, reason, attempts, ts) in [
        (
            "a",
            AssetOutcome::DoneFail,
            "content-fail",
            1,
            "2026-10-17T00:00:00+00:00",
        ),
        (
            "b",
            AssetOutcome::DonePass,
            "ok",
            2,
            "2026-10-17T00:00:01.000001+00:00",
        ),
        (
            "c",
            AssetOutcome::DoneFail,
            "retries-exhausted:asset-timeout",
            3,
            "t\u{e9}",
        ),
    ] {
        append_resume_entry(
            &log,
            &ResumeEntry::new(asset, outcome, reason, attempts, ts).unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        oracle["resume_log"].as_str().unwrap()
    );
    let cases = [
        ("repaired_complete_tail", &b"{\"schema_version\": \"1\"}\n{\"asset_id\":\"a\",\"outcome\":\"done-pass\",\"reason\":\"ok\",\"attempts\":1,\"ts\":\"x\"}"[..]),
        ("repaired_cut_tail", b"{\"schema_version\": \"1\"}\n{\"asset_id\":\"a\",\"outcome\":\"done-pass\",\"reason\":\"ok\",\"attempts\":1,\"ts\":\"x\"}\n{\"asset_id\":\"inter"),
        // Compatibility behavior: garbage with no newline is cut to nothing, and the entry
        // is written without a header (the log is then unreadable).
        ("repaired_garbage_without_newline", b"{\"unexpected\":"),
    ];
    for (name, content) in cases {
        let path = temp.path().join(name);
        std::fs::write(&path, content).unwrap();
        append_resume_entry(
            &path,
            &ResumeEntry::new("b", AssetOutcome::DonePass, "ok", 1, "y").unwrap(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            oracle[name].as_str().unwrap(),
            "{name}"
        );
    }
}

#[test]
fn reports_and_plans_are_python_s_byte_for_byte() {
    let oracle = formats();
    let temp = tempfile::tempdir().unwrap();
    let log = temp.path().join("odd.jsonl");
    std::fs::write(&log, "{\"schema_version\":\"1\"}\n\
        {\"asset_id\":\"f\",\"outcome\":\"done-fail\",\"reason\":\"unclassified\",\"attempts\":1.5,\"ts\":null}\n\
        {\"asset_id\":\"g\",\"outcome\":\"done-pass\",\"reason\":\"ok\",\"attempts\":true,\"ts\":[1]}\n").unwrap();
    let loaded = load_resume_log(&log).unwrap();
    let report = drmr_core::batch::BatchReport {
        schema_version: "1".to_string(),
        total: 3,
        passed: 1,
        failed: 2,
        skipped: 2,
        retried: 1,
        outcomes: vec![
            loaded.get("f").unwrap().clone(),
            loaded.get("g").unwrap().clone(),
            ResumeEntry::new("h", AssetOutcome::DoneFail, "missing-key", 2, "z").unwrap(),
        ],
        reasons: vec![
            ("unclassified".into(), 1),
            ("ok".into(), 1),
            ("missing-key".into(), 1),
        ],
        cpu_count: 8,
        max_workers: None,
        wall_clock_s: 12.5,
        throughput_assets_per_h: 288.0,
    };
    assert_eq!(
        String::from_utf8(report.json_bytes()).unwrap(),
        oracle["report_json"].as_str().unwrap()
    );
    assert_eq!(report.markdown(), oracle["report_md"].as_str().unwrap());
    let zero = drmr_core::batch::BatchReport {
        schema_version: "1".to_string(),
        total: 0,
        passed: 0,
        failed: 0,
        skipped: 0,
        retried: 0,
        outcomes: vec![],
        reasons: vec![],
        cpu_count: 1,
        max_workers: Some(4),
        wall_clock_s: 0.0,
        throughput_assets_per_h: 0.0,
    };
    assert_eq!(
        String::from_utf8(zero.json_bytes()).unwrap(),
        oracle["report_zero_json"].as_str().unwrap()
    );
    let work = [
        AssetWorkItem::new(
            "f",
            "dash-cenc",
            Some(ObjectRef::new("h/f.mpd").unwrap()),
            None,
        )
        .unwrap(),
        AssetWorkItem::new(
            "g",
            "dash-cenc",
            Some(ObjectRef::new("h/g.mpd").unwrap()),
            None,
        )
        .unwrap(),
        AssetWorkItem::new("n", "hls-clear", None, None).unwrap(),
    ];
    for (name, retry_failed) in [("plan", false), ("plan_retry_failed", true)] {
        let rows: Vec<String> = plan_batch(&work, &log, retry_failed)
            .unwrap()
            .into_iter()
            .map(|row| Dumps::DEFAULT.dumps(&Value::Object(row)))
            .collect();
        let expected: Vec<&str> = oracle[name]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(rows, expected, "{name}");
    }
}

#[test]
fn a_run_writes_the_report_files_and_python_s_batch_records() {
    let fixture = fixture();
    let logs = Collector::default();
    let report = run_batch_with(
        &[item("a"), item("missing")],
        &fixture.config,
        Some(&logs),
        Some(&runner),
        &seams_no_stage(),
    )
    .unwrap();
    let written =
        std::fs::read(fixture.config.output_root.join("_batch/batch-report.json")).unwrap();
    assert_eq!(written, report.json_bytes());
    let markdown =
        std::fs::read_to_string(fixture.config.output_root.join("_batch/batch-report.md")).unwrap();
    assert_eq!(markdown, report.markdown());
    for name in ["batch-report.json", "batch-report.md"] {
        let mode = std::fs::metadata(fixture.config.output_root.join("_batch").join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let records = logs.records();
    let messages: Vec<&str> = records
        .iter()
        .map(|r| r["message"].as_str().unwrap())
        .collect();
    assert_eq!(
        messages,
        ["asset-start", "asset-end", "asset-start", "asset-end"]
    );
    for record in &records {
        let keys: Vec<&str> = record.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "schema_version",
                "ts",
                "stage",
                "level",
                "message",
                "asset_id",
                "index",
                "total",
                "elapsed_s",
                "throughput_assets_per_h",
                "eta_s"
            ]
        );
        assert_eq!(
            (
                record["schema_version"].as_str(),
                record["stage"].as_str(),
                record["level"].as_str()
            ),
            (Some("1"), Some("batch"), Some("info"))
        );
        assert_eq!(record["total"], json!(2));
    }
    assert_eq!(
        records[0]["eta_s"],
        Value::Null,
        "nothing processed yet: no throughput, no ETA"
    );
    assert!(records[1]["throughput_assets_per_h"].as_f64().unwrap() > 0.0);
    assert!(records[1]["eta_s"].is_f64());
    assert_eq!(records[3]["index"], json!(2));
    assert_eq!(report.cpu_count, drmr_core::batch_support::cpu_count());
}

// ------------------------------------------------------------------------------------------
// Retry, backoff and supervision.
// ------------------------------------------------------------------------------------------

#[test]
fn retryable_classes_are_retried_then_exhausted_and_fatal_ones_are_not() {
    let fixture = fixture();
    let config = BatchConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            backoff_base_s: 0.0,
            backoff_max_s: 0.0,
            ..RetryPolicy::default()
        },
        ..fixture.config.clone()
    };
    let calls = Mutex::new(Vec::<String>::new());
    let flaky = |work: &AssetWorkItem,
                 config: &BatchConfig,
                 stage: &Path,
                 keys: &KeyMaterial,
                 beat: &dyn Fn()|
     -> Result<i32, AttemptError> {
        let mut calls = calls.lock().unwrap();
        calls.push(work.asset_id.clone());
        let seen = calls.iter().filter(|id| **id == work.asset_id).count();
        match work.asset_id.as_str() {
            "flaky" if seen < 3 => Err(AttemptError::Retry("packager-timeout".to_string())),
            "stuck" => Err(AttemptError::Storage(StorageError::retryable(
                "storage-read-failed",
            ))),
            "fatal" => Err(AttemptError::Storage(StorageError::new(
                "storage-object-invalid",
            ))),
            "parse" => Err(AttemptError::Value("parse-error".to_string())),
            _ => runner(work, config, stage, keys, beat),
        }
    };
    let work = [item("flaky"), item("stuck"), item("fatal"), item("parse")];
    let report = run(&work, &config, &flaky, &seams_no_stage()).unwrap();
    let outcomes: Vec<(&str, &str, Option<i64>)> = report
        .outcomes
        .iter()
        .map(|e| (e.asset_id.as_str(), e.reason.as_str(), e.attempt_count()))
        .collect();
    assert_eq!(
        outcomes,
        [
            ("flaky", "ok", Some(3)),
            ("stuck", "retries-exhausted:storage-transient", Some(3)),
            ("fatal", "unclassified", Some(1)),
            ("parse", "malformed-manifest", Some(1)),
        ]
    );
    assert_eq!(report.retried, 2);
}

#[test]
fn backoff_waits_between_attempts_within_the_cap() {
    let fixture = fixture();
    let config = BatchConfig {
        retry: RetryPolicy {
            max_attempts: 3,
            backoff_base_s: 10.0,
            backoff_max_s: 0.2,
            ..RetryPolicy::default()
        },
        ..fixture.config.clone()
    };
    let times = Mutex::new(Vec::new());
    let failing = |_: &AssetWorkItem,
                   _: &BatchConfig,
                   _: &Path,
                   _: &KeyMaterial,
                   _: &dyn Fn()|
     -> Result<i32, AttemptError> {
        times.lock().unwrap().push(Instant::now());
        Err(AttemptError::Retry("asset-timeout".to_string()))
    };
    let start = Instant::now();
    let report = run(&[item("a")], &config, &failing, &seams_no_stage()).unwrap();
    assert_eq!(report.outcomes[0].reason, "retries-exhausted:asset-timeout");
    assert_eq!(times.lock().unwrap().len(), 3);
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "the backoff is capped at backoff_max_s"
    );
}

#[test]
fn run_commands_classifies_timeouts_codes_and_the_deadline() {
    let fixture = fixture();
    let sh =
        |script: &str, subcommand: &str| CliCommand::new(["/bin/sh", "-c", script], subcommand);
    let config = &fixture.config;
    assert_eq!(
        run_commands(
            &[
                sh("exit 0", "run"),
                sh("exit 3", "run"),
                sh("exit 1", "run")
            ],
            config,
            None,
            &|| {}
        )
        .unwrap(),
        3
    );
    let error = run_commands(
        &[sh(
            "echo 'packager: Timed Out after 30s' >&2; exit 2",
            "run",
        )],
        config,
        None,
        &|| {},
    )
    .unwrap_err();
    assert_eq!(error, AttemptError::Retry("packager-timeout".to_string()));
    // Not an analysis command: its records are not read.
    let script = "echo '{\"stage\": \"analysis\", \"message\": \"source-timeout\"}'; exit 1";
    assert_eq!(
        run_commands(&[sh(script, "package-hls")], config, None, &|| {}).unwrap(),
        1
    );
    assert_eq!(
        run_commands(&[sh("kill -9 $$", "run")], config, None, &|| {}).unwrap(),
        -9
    );
    // The key material reaches the child's environment, never its argv.
    let witness = fixture.temp.path().join("env");
    let script = format!(
        "printf '%s %s %s' \"$DRM_KID\" \"$DRM_KEY\" \"$DRM_IV\" > '{}'",
        witness.display()
    );
    assert_eq!(
        run_commands(&[sh(&script, "run")], config, Some(&key()), &|| {}).unwrap(),
        0
    );
    assert_eq!(
        std::fs::read_to_string(&witness).unwrap(),
        format!("{KID} {KEY} {IV}")
    );
    // The per-asset deadline kills the child's whole group.
    let deadline = BatchConfig {
        retry: RetryPolicy {
            per_asset_timeout_s: 0.3,
            heartbeat_s: 0.05,
            ..RetryPolicy::default()
        },
        ..config.clone()
    };
    let pidfile = fixture.temp.path().join("grandchild.pid");
    let script = format!("sleep 60 & echo $! > '{}'; sleep 60", pidfile.display());
    let start = Instant::now();
    let error = run_commands(&[sh(&script, "run")], &deadline, None, &|| {}).unwrap_err();
    assert_eq!(error, AttemptError::Retry("asset-timeout".to_string()));
    assert!(start.elapsed() < Duration::from_secs(5));
    let pid = std::fs::read_to_string(&pidfile).unwrap();
    let state = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid.trim()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&state.stdout).trim().to_string();
    assert!(
        state.is_empty() || state.starts_with('Z'),
        "the grandchild survived: {state}"
    );
}

#[test]
fn command_heartbeat_handshake_releases_the_child_after_two_callbacks() {
    let fixture = fixture();
    let heartbeat = HeartbeatRelease {
        release: fixture.temp.path().join("release"),
        beats: AtomicUsize::new(0),
    };
    let config = BatchConfig {
        retry: RetryPolicy {
            per_asset_timeout_s: 5.0,
            heartbeat_s: 0.05,
            ..RetryPolicy::default()
        },
        ..fixture.config.clone()
    };
    let command = CliCommand::new(
        [
            OsString::from("/bin/sh"),
            "-c".into(),
            "while [ ! -f \"$1\" ]; do sleep 0.01; done".into(),
            "heartbeat-child".into(),
            heartbeat.release.clone().into(),
        ],
        "run",
    );
    assert_eq!(
        run_commands(&[command], &config, None, &|| heartbeat.beat()).unwrap(),
        0
    );
    assert!(heartbeat.beats.load(Ordering::SeqCst) >= 2);
    assert!(heartbeat.release.is_file());
}

fn answer_worker(answer: &str) -> Vec<OsString> {
    let script = format!("cat > /dev/null; printf '%s\\n' '{answer}'");
    vec!["/bin/sh".into(), "-c".into(), script.into()]
}

#[test]
fn supervision_reads_the_worker_answer_and_treats_garbage_as_a_crash() {
    let fixture = fixture();
    let work = item("a");
    let config = &fixture.config;
    let ok = supervise_attempt(&work, config, &|| {}, &answer_worker("[\"ok\", false]")).unwrap();
    assert_eq!(ok, ("ok".to_string(), false));
    let retry = supervise_attempt(
        &work,
        config,
        &|| {},
        &answer_worker("[\"storage-transient\", true]"),
    )
    .unwrap();
    assert_eq!(retry, ("storage-transient".to_string(), true));
    for invalid in ["[\"bogus\", false]", "[\"ok\", 1]", "[5, false]"] {
        let error = supervise_attempt(&work, config, &|| {}, &answer_worker(invalid)).unwrap_err();
        assert_eq!(
            error,
            AttemptError::Batch(BatchError::new("worker-result-invalid")),
            "{invalid}"
        );
    }
    assert_eq!(
        supervise_attempt(&work, config, &|| {}, &answer_worker("[[1], false]")).unwrap_err(),
        AttemptError::Other
    );
    let long = format!("[\"ok\", false]{}", " ".repeat(300));
    for crash in ["not json", "[\"ok\"]", "5", long.as_str()] {
        let error = supervise_attempt(&work, config, &|| {}, &answer_worker(crash)).unwrap_err();
        assert_eq!(
            error,
            AttemptError::Interrupted("batch-worker-interrupted".to_string()),
            "{crash:?}"
        );
    }
    let exit = vec![
        "/bin/sh".into(),
        "-c".into(),
        "cat > /dev/null; echo '[\"ok\", false]'; exit 3".into(),
    ];
    let error = supervise_attempt(&work, config, &|| {}, &exit).unwrap_err();
    assert!(matches!(error, AttemptError::Interrupted(_)));
    // In run_batch, a crashed worker ends the run with no terminal line.
    let seams = Seams {
        preflight: Some(&no_preflight),
        worker_command: Some(answer_worker("garbage")),
        ..Seams::default()
    };
    assert!(matches!(
        run_batch_with(&[work], config, None, None, &seams),
        Err(RunError::Interrupted(_))
    ));
    assert!(!config.resume_log.exists());
}

#[test]
fn the_private_configuration_travels_over_stdin_only() {
    let fixture = fixture();
    let captured = fixture.temp.path().join("payload.json");
    let script = format!(
        "cat > '{}'; echo '[\"missing-key\", false]'",
        captured.display()
    );
    let command: Vec<OsString> = vec!["/bin/sh".into(), "-c".into(), script.into()];
    let answer = supervise_attempt(&item("a"), &fixture.config, &|| {}, &command).unwrap();
    assert_eq!(answer, ("missing-key".to_string(), false));
    let payload: Value = serde_json::from_slice(&std::fs::read(&captured).unwrap()).unwrap();
    let keys: Vec<&str> = payload
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "work",
            "storage",
            "provider",
            "transfer",
            "storage_policy",
            "retry",
            "output_root",
            "resume_log",
            "output_prefix",
            "max_workers",
            "hls_config_dir",
            "packager_bin"
        ]
    );
    assert_eq!(
        payload["work"],
        json!({"asset_id": "a", "kind": "dash-cenc", "dash_manifest": "host/a/manifest.mpd", "hls_master": null})
    );
    assert_eq!(payload["storage"]["kind"], "local");
    // A custom provider's key material is resolved by the supervisor and sent over the pipe.
    assert_eq!(
        payload["provider"],
        json!({"kind": "provided", "kid": KID, "key": KEY, "iv": IV})
    );
    assert_eq!(payload["retry"]["max_attempts"], json!(2));
    // In a run, a supervised `missing-key` is quarantined like the in-process one.
    let seams = Seams {
        preflight: Some(&no_preflight),
        worker_command: Some(answer_worker("[\"missing-key\", false]")),
        ..Seams::default()
    };
    let report = run_batch_with(&[item("a")], &fixture.config, None, None, &seams).unwrap();
    assert_eq!(report.outcomes[0].reason, "missing-key");
    // A store the worker cannot rebuild is refused before anything runs.
    let config = BatchConfig {
        storage: Arc::new(BucketOnly("b")),
        ..fixture.config.clone()
    };
    let error = supervise_attempt(&item("a"), &config, &|| {}, &command).unwrap_err();
    assert_eq!(
        error,
        AttemptError::Batch(BatchError::new("storage-config-invalid"))
    );
    // An oversized configuration is refused.
    let config = BatchConfig {
        packager_bin: Some(PathBuf::from("p".repeat(70_000))),
        ..fixture.config.clone()
    };
    let error = supervise_attempt(&item("a"), &config, &|| {}, &command).unwrap_err();
    assert_eq!(
        error,
        AttemptError::Batch(BatchError::new("worker-config-invalid"))
    );
}

#[test]
fn every_attempt_starts_from_clean_scratch_and_keeps_a_frozen_clear_source() {
    let fixture = fixture();
    let asset = fixture.config.output_root.join("h");
    write(&asset.join("clear/source/analysis.json"), b"{}");
    write(&asset.join("clear/_work/x"), b"scratch");
    write(&asset.join("cbcs/old.m4s"), b"stale");
    let seen = Mutex::new(Vec::new());
    let inspect = |work: &AssetWorkItem,
                   config: &BatchConfig,
                   _: &Path,
                   _: &KeyMaterial,
                   _: &dyn Fn()|
     -> Result<i32, AttemptError> {
        let dir = config.output_root.join(&work.asset_id);
        seen.lock().unwrap().push((
            dir.join("clear/source/analysis.json").exists(),
            dir.join("clear/_work").exists(),
            dir.join("cbcs/old.m4s").exists(),
        ));
        Err(AttemptError::Retry("asset-timeout".to_string()))
    };
    let work = AssetWorkItem::new("h", "hls-clear", None, None).unwrap();
    run(&[work], &fixture.config, &inspect, &seams_no_stage()).unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        [(true, false, false), (true, false, false)]
    );
    assert!(
        !asset.exists(),
        "the asset's scratch is removed when it ends"
    );
}

#[test]
fn absurd_but_finite_timeouts_are_accepted_like_python() {
    // Python's deadline is float arithmetic: any finite timeout works (the policy only requires
    // finite and positive). The Rust deadline saturates instead of overflowing.
    let fixture = fixture();
    let config = BatchConfig {
        retry: RetryPolicy {
            per_asset_timeout_s: 1e19,
            heartbeat_s: 1e300,
            ..RetryPolicy::default()
        },
        ..fixture.config.clone()
    };
    assert!(config.validate().is_ok());
    let quick = CliCommand::new(["/bin/sh", "-c", "exit 0"], "run");
    assert_eq!(run_commands(&[quick], &config, None, &|| {}).unwrap(), 0);
    let worker = answer_worker("[\"ok\", false]");
    let answer = supervise_attempt(&item("a"), &config, &|| {}, &worker).unwrap();
    assert_eq!(answer, ("ok".to_string(), false));
}
