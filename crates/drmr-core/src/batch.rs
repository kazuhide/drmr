//! Sequential batch processing with durable resume logs and isolated asset workers.
//!
//! An asset is complete only after upload and completion-marker verification succeed.
//! Fatal and exhausted failures are recorded even when the object store is unavailable.
//! Each production attempt has its own process group and covers staging, conversion,
//! upload, and verification within one deadline. Worker configuration travels over stdin
//! rather than process arguments. Injected runners provide an in-process test seam.

use crate::batch_support::{
    DefaultHlsSource, is_relative_to, py_splitlines, resolve, universal_newlines,
};
use crate::canonical::{Dumps, python_float_repr};
use crate::config::{KeyMaterial, load_key_material};
use crate::events::{Record, Sink, now_iso};
use crate::storage::{ObjectRef, StorageBackend, StorageError, TransferPolicy};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

mod commands;
mod stage;
mod worker;

pub use commands::{CliCommand, CliRunner, commands, private_config, run_commands};
pub use stage::{preflight_source_graphs, source_overlaps_output, source_ref, stage_inputs};
pub use worker::{WORKER_ARG, supervise_attempt, worker_command, worker_main, worker_payload};

/// The worker records `schema_version` first in every batch record and the resume log header.
pub const SCHEMA_VERSION: &str = "1";

/// `_FATAL`: terminal reasons that are never retried.
pub const FATAL: &[&str] = &[
    "missing-key",
    "unsupported-subtitles",
    "unsupported-codec",
    "unsupported-kind",
    "malformed-manifest",
    "content-fail",
    "unclassified",
    "hls-config-invalid",
];

/// `_RETRYABLE`: transient classes retried up to `RetryPolicy.max_attempts`.
pub const RETRYABLE: &[&str] = &["storage-transient", "asset-timeout", "packager-timeout"];

/// The analysis-child messages `_run_commands` reads as a transient source failure.
pub(crate) const TRANSIENT_SOURCE: &[&str] = &[
    "source-fetch-failed",
    "source-timeout",
    "source-resolution-failed",
    "source-http-failed",
];

/// `_REASONS`: `ok`, every fatal token and `retries-exhausted:<retryable token>`.
pub fn is_reason(token: &str) -> bool {
    token == "ok"
        || FATAL.contains(&token)
        || token
            .strip_prefix("retries-exhausted:")
            .is_some_and(|class| RETRYABLE.contains(&class))
}

/// `BatchError`: a safe configuration classification token only.
#[derive(Clone, PartialEq, Eq, thiserror::Error)]
#[error("{token}")]
pub struct BatchError {
    token: String,
    unsafe_graph: bool,
}

impl fmt::Debug for BatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = if self.unsafe_graph {
            "UnsafeSourceGraph"
        } else {
            "BatchError"
        };
        write!(f, "{name}({:?})", self.token)
    }
}

impl BatchError {
    /// `BatchError(token)`.
    pub fn new(token: &str) -> BatchError {
        BatchError {
            token: token.to_string(),
            unsafe_graph: false,
        }
    }

    /// `_UnsafeSourceGraph(token)`: a source graph that cannot be proven disjoint from the
    /// output; the preflight raises it as-is.
    pub fn unsafe_source_graph(token: &str) -> BatchError {
        BatchError {
            token: token.to_string(),
            unsafe_graph: true,
        }
    }

    /// The token (`str(exc)`).
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Whether this is an `_UnsafeSourceGraph`.
    pub fn is_unsafe_source_graph(&self) -> bool {
        self.unsafe_graph
    }
}

/// `KeyNotAvailable`: the provider has no key material for the asset (`missing-key`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("missing-key")]
pub struct KeyNotAvailable;

/// What one per-asset step raised, as Python's exception classes, which `_classify` and
/// `run_batch` tell apart. Messages are classification tokens only.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AttemptError {
    /// `KeyNotAvailable`.
    #[error("missing-key")]
    KeyNotAvailable,
    /// `_RetryFailure(reason)`: a retryable class.
    #[error("{0}")]
    Retry(String),
    /// `StorageError`.
    #[error("{0}")]
    Storage(StorageError),
    /// `BatchError` (including `_UnsafeSourceGraph`).
    #[error("{0}")]
    Batch(BatchError),
    /// `ValueError` / `UnicodeError` / `ET.ParseError` (and the `SourceError`),
    /// with its token.
    #[error("{0}")]
    Value(String),
    /// Any other `Exception` (`OSError`, `KeyError`, `TypeError`, `RuntimeError`, …).
    #[error("unclassified")]
    Other,
    /// `KeyboardInterrupt` / any `BaseException`: never classified; it ends the run with no
    /// terminal line for the asset (a crash).
    #[error("{0}")]
    Interrupted(String),
}

impl From<StorageError> for AttemptError {
    fn from(error: StorageError) -> Self {
        AttemptError::Storage(error)
    }
}

impl From<BatchError> for AttemptError {
    fn from(error: BatchError) -> Self {
        AttemptError::Batch(error)
    }
}

impl From<KeyNotAvailable> for AttemptError {
    fn from(_: KeyNotAvailable) -> Self {
        AttemptError::KeyNotAvailable
    }
}

/// What `run_batch` (and the configuration around it) can fail with: the `ValueError` /
/// `OSError` family the CLI maps to exit 2, or a crash.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RunError {
    /// `BatchError`.
    #[error("{0}")]
    Batch(BatchError),
    /// `StorageError`.
    #[error("{0}")]
    Storage(StorageError),
    /// Another `ValueError` (a resume log that is not UTF-8).
    #[error("{0}")]
    Value(String),
    /// `OSError` (no path or message is kept).
    #[error("os-error")]
    Os,
    /// `KeyboardInterrupt`: the run stops; the interrupted asset has no terminal line.
    #[error("{0}")]
    Interrupted(String),
}

impl From<BatchError> for RunError {
    fn from(error: BatchError) -> Self {
        RunError::Batch(error)
    }
}

impl From<StorageError> for RunError {
    fn from(error: StorageError) -> Self {
        RunError::Storage(error)
    }
}

impl From<io::Error> for RunError {
    fn from(_: io::Error) -> Self {
        RunError::Os
    }
}

/// `_asset_id`: `[A-Za-z0-9][A-Za-z0-9_.-]{0,127}`, else `asset-id-invalid`.
pub fn validate_asset_id(value: &str) -> Result<(), BatchError> {
    #[cfg(windows)]
    crate::native::validate_windows_relative_path(value)
        .map_err(|_| BatchError::new("asset-id-invalid"))?;
    let bytes = value.as_bytes();
    let valid = !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(BatchError::new("asset-id-invalid"))
    }
}

/// `AssetWorkItem`: an asset's locations and kind — never key material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetWorkItem {
    /// The asset id (`output/<asset_id>/`).
    pub asset_id: String,
    /// `dash-cenc` or `hls-clear` (any other kind is quarantined `unsupported-kind`).
    pub kind: String,
    /// The source DASH manifest object.
    pub dash_manifest: Option<ObjectRef>,
    /// The source HLS master playlist object.
    pub hls_master: Option<ObjectRef>,
}

impl AssetWorkItem {
    /// `AssetWorkItem(asset_id, kind, dash_manifest, hls_master)`; the asset id is validated.
    pub fn new(
        asset_id: &str,
        kind: &str,
        dash_manifest: Option<ObjectRef>,
        hls_master: Option<ObjectRef>,
    ) -> Result<AssetWorkItem, BatchError> {
        validate_asset_id(asset_id)?;
        Ok(AssetWorkItem {
            asset_id: asset_id.to_string(),
            kind: kind.to_string(),
            dash_manifest,
            hls_master,
        })
    }
}

/// How the asset worker rebuilds a key provider (`_worker_payload`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyProviderKind<'a> {
    /// [`EnvKeyProvider`]: the worker reads `DRM_*` from the environment it inherits.
    Env,
    /// [`FileKeyProvider`] with this path: the worker re-reads and re-validates the file.
    File(&'a Path),
    /// Any other provider: its result for the asset is resolved in the parent and travels to
    /// the worker over the private stdin pipe, never argv or files.
    Provided,
}

/// `KeyProvider`: per-asset key resolution; never logs or echoes key material.
pub trait KeyProvider: Send + Sync {
    /// The asset's key material, or [`KeyNotAvailable`].
    fn key_material(&self, asset_id: &str) -> Result<KeyMaterial, KeyNotAvailable>;
    /// How the worker rebuilds this provider.
    fn kind(&self) -> KeyProviderKind<'_> {
        KeyProviderKind::Provided
    }
}

/// `EnvKeyProvider`: `DRM_KID` / `DRM_KEY` / `DRM_IV` (`--key-provider env`, the default).
#[derive(Clone, Copy, Debug, Default)]
pub struct EnvKeyProvider;

impl KeyProvider for EnvKeyProvider {
    fn key_material(&self, _asset_id: &str) -> Result<KeyMaterial, KeyNotAvailable> {
        load_key_material(None).map_err(|_| KeyNotAvailable)
    }

    fn kind(&self) -> KeyProviderKind<'_> {
        KeyProviderKind::Env
    }
}

/// `FileKeyProvider`: a host-local, mode-0600 JSON-Lines file of `{asset_id, kid, key, iv}`
/// outside the output root. `Debug` prints nothing (hidden debug fields on every field).
pub struct FileKeyProvider {
    path: PathBuf,
    keys: HashMap<String, KeyMaterial>,
}

impl fmt::Debug for FileKeyProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileKeyProvider").finish_non_exhaustive()
    }
}

impl FileKeyProvider {
    /// Read and validate the file; every failure is `key-provider-invalid`.
    pub fn new(path: &Path, output_root: &Path) -> Result<FileKeyProvider, BatchError> {
        #[cfg(unix)]
        use std::os::unix::fs::PermissionsExt;
        let invalid = || BatchError::new("key-provider-invalid");
        if path.is_symlink() {
            return Err(invalid());
        }
        let metadata = std::fs::metadata(path).map_err(|_| invalid())?;
        #[cfg(unix)]
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(invalid());
        }
        if is_relative_to(
            &resolve(path).map_err(|_| invalid())?,
            &resolve(output_root).map_err(|_| invalid())?,
        ) || metadata.len() > 128 * 1024 * 1024
        {
            return Err(invalid());
        }
        let file = crate::native::open_private_read(path).map_err(|_| invalid())?;
        let mut bytes = Vec::new();
        file.take(128 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() > 128 * 1024 * 1024 {
            return Err(invalid());
        }
        let text = universal_newlines(&String::from_utf8(bytes).map_err(|_| invalid())?);
        let mut keys = HashMap::new();
        for line in py_splitlines(&text) {
            let Ok(Value::Object(row)) = serde_json::from_str::<Value>(line) else {
                return Err(invalid());
            };
            if !same_keys(&row, &["asset_id", "kid", "key", "iv"]) {
                return Err(invalid());
            }
            let identifier = row["asset_id"].as_str().ok_or_else(invalid)?;
            validate_asset_id(identifier).map_err(|_| invalid())?;
            if keys.contains_key(identifier) {
                return Err(invalid());
            }
            // `load_key_material({"DRM_KID": row["kid"], …})`: a value that is not text fails
            // there (a falsy one as missing, a truthy one in `bytes.fromhex`), and every
            // failure is `key-provider-invalid`.
            let mut env = HashMap::new();
            for (name, field) in [("DRM_KID", "kid"), ("DRM_KEY", "key"), ("DRM_IV", "iv")] {
                let text = row[field].as_str().ok_or_else(invalid)?;
                env.insert(name.to_string(), text.to_string());
            }
            let material = load_key_material(Some(&env)).map_err(|_| invalid())?;
            keys.insert(identifier.to_string(), material);
        }
        Ok(FileKeyProvider {
            path: path.to_path_buf(),
            keys,
        })
    }

    /// The key file's path (never logged).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl KeyProvider for FileKeyProvider {
    fn key_material(&self, asset_id: &str) -> Result<KeyMaterial, KeyNotAvailable> {
        self.keys.get(asset_id).cloned().ok_or(KeyNotAvailable)
    }

    fn kind(&self) -> KeyProviderKind<'_> {
        KeyProviderKind::File(&self.path)
    }
}

/// Whether a JSON object has exactly these keys (`set(row) == {...}`).
pub(crate) fn same_keys(row: &Map<String, Value>, keys: &[&str]) -> bool {
    row.len() == keys.len() && keys.iter().all(|key| row.contains_key(*key))
}

/// `AssetOutcome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AssetOutcome {
    /// `done-pass`: skipped on every later run.
    DonePass,
    /// `done-fail`: quarantined; re-run only with `retry_failed`.
    DoneFail,
}

impl AssetOutcome {
    /// The token as the log spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            AssetOutcome::DonePass => "done-pass",
            AssetOutcome::DoneFail => "done-fail",
        }
    }

    fn parse(value: &Value) -> Option<AssetOutcome> {
        match value.as_str()? {
            "done-pass" => Some(AssetOutcome::DonePass),
            "done-fail" => Some(AssetOutcome::DoneFail),
            _ => None,
        }
    }
}

/// `ResumeEntry`: one terminal asset in the resume log.
///
/// `attempts` and `ts` keep the JSON values the log holds: Python validates `attempts` only by
/// `attempts < 1` (an integer, a float or a boolean passes when not below 1) and `ts` not at
/// all, and writes back what it read into the report.
#[derive(Clone, Debug, PartialEq)]
pub struct ResumeEntry {
    /// The asset id.
    pub asset_id: String,
    /// The terminal outcome.
    pub outcome: AssetOutcome,
    /// A `_REASONS` token (`ok` exactly for `done-pass`).
    pub reason: String,
    /// The attempts used (a positive integer when this tool writes the entry).
    pub attempts: Value,
    /// When the entry was written (`datetime.now(timezone.utc).isoformat`).
    pub ts: Value,
}

impl ResumeEntry {
    /// `ResumeEntry(asset_id, outcome, reason, attempts, ts)`, validated.
    pub fn new(
        asset_id: &str,
        outcome: AssetOutcome,
        reason: &str,
        attempts: u32,
        ts: &str,
    ) -> Result<ResumeEntry, BatchError> {
        let entry = ResumeEntry {
            asset_id: asset_id.to_string(),
            outcome,
            reason: reason.to_string(),
            attempts: Value::from(attempts),
            ts: Value::String(ts.to_string()),
        };
        entry.validate()?;
        Ok(entry)
    }

    fn validate(&self) -> Result<(), BatchError> {
        validate_asset_id(&self.asset_id)?;
        let below_one = match &self.attempts {
            Value::Number(number) => match number.as_i64() {
                Some(integer) => integer < 1,
                None => number.as_f64().is_some_and(|float| float < 1.0),
            },
            Value::Bool(flag) => !flag,
            _ => true,
        };
        if !is_reason(&self.reason)
            || below_one
            || (self.outcome == AssetOutcome::DonePass) != (self.reason == "ok")
        {
            return Err(BatchError::new("resume-entry-invalid"));
        }
        Ok(())
    }

    /// `asdict(entry)`: `asset_id`, `outcome`, `reason`, `attempts`, `ts` in that order.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("asset_id".into(), Value::String(self.asset_id.clone()));
        map.insert(
            "outcome".into(),
            Value::String(self.outcome.as_str().into()),
        );
        map.insert("reason".into(), Value::String(self.reason.clone()));
        map.insert("attempts".into(), self.attempts.clone());
        map.insert("ts".into(), self.ts.clone());
        Value::Object(map)
    }

    /// The attempts as an integer, when they are one.
    pub fn attempt_count(&self) -> Option<i64> {
        self.attempts.as_i64()
    }
}

/// `RetryPolicy`: the per-asset retry, backoff, deadline and heartbeat (physical tuning).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RetryPolicy {
    /// Attempts per asset (1–10).
    pub max_attempts: i64,
    /// Backoff base, seconds.
    pub backoff_base_s: f64,
    /// Backoff cap, seconds.
    pub backoff_max_s: f64,
    /// The whole-asset wall clock of one attempt, seconds (`asset-timeout`).
    pub per_asset_timeout_s: f64,
    /// Heartbeat record interval during an attempt, seconds.
    pub heartbeat_s: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            max_attempts: 3,
            backoff_base_s: 1.0,
            backoff_max_s: 60.0,
            per_asset_timeout_s: 3600.0,
            heartbeat_s: 60.0,
        }
    }
}

impl RetryPolicy {
    /// The policy if every bound holds, else `retry-policy-invalid`.
    pub fn validate(self) -> Result<RetryPolicy, BatchError> {
        let finite = [
            self.backoff_base_s,
            self.backoff_max_s,
            self.per_asset_timeout_s,
            self.heartbeat_s,
        ]
        .iter()
        .all(|value| value.is_finite());
        if !finite
            || !(1..=10).contains(&self.max_attempts)
            || self.backoff_base_s.min(self.backoff_max_s) < 0.0
            || self.per_asset_timeout_s.min(self.heartbeat_s) <= 0.0
        {
            return Err(BatchError::new("retry-policy-invalid"));
        }
        Ok(self)
    }
}

/// `BatchConfig`. Build it with [`BatchConfig::new`] and the struct-update syntax
/// (`BatchConfig { retry_failed: true,..config }`, Python's `dataclasses.replace`), then
/// [`BatchConfig::validate`]; `run_batch` validates again.
#[derive(Clone)]
pub struct BatchConfig {
    /// The object store.
    pub storage: Arc<dyn StorageBackend>,
    /// The key provider.
    pub keys: Arc<dyn KeyProvider>,
    /// The local output root (`output/<asset_id>/`, `output/_batch/`).
    pub output_root: PathBuf,
    /// The JSON-Lines resume / quarantine log.
    pub resume_log: PathBuf,
    /// Per-asset retry.
    pub retry: RetryPolicy,
    /// Staging budgets.
    pub transfer: TransferPolicy,
    /// The per-asset worker bound forwarded to `run` / `package-hls` (1–64).
    pub max_workers: Option<i64>,
    /// Re-run quarantined (`done-fail`) assets.
    pub retry_failed: bool,
    /// Private clear-HLS configurations (`<asset_id>.json`).
    pub hls_config_dir: Option<PathBuf>,
    /// The output prefix in the store.
    pub output_prefix: String,
    /// A Shaka Packager override forwarded to the per-asset commands.
    pub packager_bin: Option<PathBuf>,
}

impl fmt::Debug for BatchConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BatchConfig")
            .field("storage", &"…")
            .field("output_root", &self.output_root)
            .field("resume_log", &self.resume_log)
            .field("retry", &self.retry)
            .field("transfer", &self.transfer)
            .field("max_workers", &self.max_workers)
            .field("retry_failed", &self.retry_failed)
            .field("output_prefix", &self.output_prefix)
            .finish_non_exhaustive()
    }
}

impl BatchConfig {
    /// A configuration with Python's defaults for everything but the four required fields.
    pub fn new(
        storage: Arc<dyn StorageBackend>,
        keys: Arc<dyn KeyProvider>,
        output_root: &Path,
        resume_log: &Path,
    ) -> BatchConfig {
        BatchConfig {
            storage,
            keys,
            output_root: output_root.to_path_buf(),
            resume_log: resume_log.to_path_buf(),
            retry: RetryPolicy::default(),
            transfer: TransferPolicy::default(),
            max_workers: None,
            retry_failed: false,
            hls_config_dir: None,
            output_prefix: "output".to_string(),
            packager_bin: None,
        }
    }

    /// `BatchConfig.__post_init__` (and the policies' own validation).
    pub fn validate(&self) -> Result<(), RunError> {
        if self
            .max_workers
            .is_some_and(|bound| !(1..=64).contains(&bound))
        {
            return Err(BatchError::new("worker-bound-invalid").into());
        }
        ObjectRef::new(self.output_prefix.as_str())?;
        let root = resolve(&self.output_root)?;
        if let Some(storage_root) = self.storage.root()
            && (is_relative_to(&root, storage_root) || is_relative_to(storage_root, &root))
        {
            return Err(BatchError::new("storage-output-overlap").into());
        }
        if let KeyProviderKind::File(path) = self.keys.kind()
            && is_relative_to(&resolve(path)?, &root)
        {
            return Err(BatchError::new("key-provider-invalid").into());
        }
        if let Some(directory) = &self.hls_config_dir
            && is_relative_to(&resolve(directory)?, &root)
        {
            return Err(BatchError::new("hls-config-invalid").into());
        }
        self.retry.validate()?;
        self.transfer.validate()?;
        Ok(())
    }
}

/// `BatchReport`.
#[derive(Clone, Debug, PartialEq)]
pub struct BatchReport {
    /// `"1"`.
    pub schema_version: String,
    /// Work-list size.
    pub total: usize,
    /// `done-pass` outcomes.
    pub passed: usize,
    /// `done-fail` outcomes.
    pub failed: usize,
    /// Assets already terminal (not run).
    pub skipped: usize,
    /// Assets that used more than one attempt.
    pub retried: usize,
    /// Every asset's terminal entry, in work-list order.
    pub outcomes: Vec<ResumeEntry>,
    /// Outcomes per reason, in order of first appearance.
    pub reasons: Vec<(String, usize)>,
    /// `os.cpu_count or 1`.
    pub cpu_count: usize,
    /// The per-asset worker bound.
    pub max_workers: Option<i64>,
    /// Wall clock of the processing loop, seconds.
    pub wall_clock_s: f64,
    /// Processed assets per hour.
    pub throughput_assets_per_h: f64,
}

impl BatchReport {
    /// `asdict(report)` in field order. A zero wall clock writes the integer `0` for the
    /// throughput, as Python's `… if elapsed else 0` does.
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert(
            "schema_version".into(),
            Value::String(self.schema_version.clone()),
        );
        map.insert("total".into(), Value::from(self.total));
        map.insert("passed".into(), Value::from(self.passed));
        map.insert("failed".into(), Value::from(self.failed));
        map.insert("skipped".into(), Value::from(self.skipped));
        map.insert("retried".into(), Value::from(self.retried));
        map.insert(
            "outcomes".into(),
            Value::Array(self.outcomes.iter().map(ResumeEntry::to_value).collect()),
        );
        let reasons: Map<String, Value> = self
            .reasons
            .iter()
            .map(|(reason, count)| (reason.clone(), Value::from(*count)))
            .collect();
        map.insert("reasons".into(), Value::Object(reasons));
        map.insert("cpu_count".into(), Value::from(self.cpu_count));
        map.insert(
            "max_workers".into(),
            self.max_workers.map_or(Value::Null, Value::from),
        );
        map.insert("wall_clock_s".into(), Value::from(self.wall_clock_s));
        map.insert(
            "throughput_assets_per_h".into(),
            rate(self.wall_clock_s, self.throughput_assets_per_h),
        );
        Value::Object(map)
    }

    /// `batch-report.json`: `json.dumps(asdict(report), indent=2)` (no trailing newline).
    pub fn json_bytes(&self) -> Vec<u8> {
        Dumps::INDENT2.dumps(&self.to_value()).into_bytes()
    }

    /// `batch-report.md`.
    pub fn markdown(&self) -> String {
        let mut lines = vec![
            "# Batch report".to_string(),
            String::new(),
            format!(
                "Total: {}; passed: {}; failed: {}; skipped: {}",
                self.total, self.passed, self.failed, self.skipped
            ),
            String::new(),
            "| Asset | Outcome | Reason | Attempts |".to_string(),
            "|---|---|---|---|".to_string(),
        ];
        for entry in &self.outcomes {
            lines.push(format!(
                "| {} | {} | {} | {} |",
                entry.asset_id,
                entry.outcome.as_str(),
                entry.reason,
                py_str(&entry.attempts)
            ));
        }
        lines.join("\n") + "\n"
    }

    /// The count of one reason (`reasons[reason]`, 0 when absent).
    pub fn reason_count(&self, reason: &str) -> usize {
        self.reasons
            .iter()
            .find(|(token, _)| token == reason)
            .map_or(0, |(_, count)| *count)
    }
}

/// A rate over `elapsed` as Python writes it: the integer `0` when `elapsed` is zero.
fn rate(elapsed: f64, value: f64) -> Value {
    if elapsed == 0.0 {
        Value::from(0)
    } else {
        Value::from(value)
    }
}

/// `str(value)` of a JSON scalar as Python prints it in an f-string.
fn py_str(value: &Value) -> String {
    match value {
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => match number.as_i64().map(|i| i.to_string()) {
            Some(text) => text,
            None => number.as_u64().map_or_else(
                || python_float_repr(number.as_f64().unwrap_or(0.0)),
                |u| u.to_string(),
            ),
        },
        Value::String(text) => text.clone(),
        Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

// ------------------------------------------------------------------------------------------
// The work list and the resume log (JSON Lines v1).
// ------------------------------------------------------------------------------------------

const HEADER_SPACED: &str = r#"{"schema_version": "1"}"#;
const HEADER_COMPACT: &str = r#"{"schema_version":"1"}"#;

/// The compact separators of a resume line (`separators=(",", ":")`, insertion order).
const COMPACT: Dumps = Dumps {
    sort_keys: false,
    indent: None,
    item_separator: ",",
    key_separator: ":",
};

/// `_rows`: the records after the `{"schema_version": "1"}` header; every failure is
/// `record-file-invalid`. With `recover_tail`, a lone partial header is an empty log and an
/// unparsable final line without a trailing newline (a killed append) is dropped.
fn rows(path: &Path, missing_ok: bool, recover_tail: bool) -> Result<Vec<Value>, BatchError> {
    let invalid = || BatchError::new("record-file-invalid");
    if missing_ok && !path.exists() {
        return Ok(Vec::new());
    }
    let metadata = std::fs::metadata(path).map_err(|_| invalid())?;
    if metadata.len() > 256 * 1024 * 1024 {
        return Err(invalid());
    }
    let bytes = std::fs::read(path).map_err(|_| invalid())?;
    let content = universal_newlines(&String::from_utf8(bytes).map_err(|_| invalid())?);
    if recover_tail && partial_header(&content) {
        return Ok(Vec::new());
    }
    let lines = py_splitlines(&content);
    let header = lines
        .first()
        .and_then(|line| serde_json::from_str::<Value>(line).ok());
    if header != Some(serde_json::json!({"schema_version": "1"})) {
        return Err(invalid());
    }
    let mut rows = Vec::new();
    for (index, line) in lines.iter().enumerate().skip(1) {
        match serde_json::from_str::<Value>(line) {
            Ok(row) => rows.push(row),
            Err(_) if recover_tail && index == lines.len() - 1 && !content.ends_with('\n') => break,
            Err(_) => return Err(invalid()),
        }
    }
    Ok(rows)
}

/// `_partial_header`: a single line that is a proper prefix of either header spelling.
fn partial_header(content: &str) -> bool {
    !content.contains('\n')
        && [HEADER_SPACED, HEADER_COMPACT]
            .iter()
            .any(|header| header.starts_with(content) && content != *header)
}

/// `read_work_list`: the header, then one `{asset_id, kind, dash_manifest, hls_master}` per
/// line — exactly those keys, `kind` `dash-cenc` or `hls-clear`, unique ids, a manifest for
/// `dash-cenc`. Anything else, key material included, is `work-list-invalid`.
pub fn read_work_list(path: &Path) -> Result<Vec<AssetWorkItem>, BatchError> {
    let invalid = || BatchError::new("work-list-invalid");
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    for row in rows(path, false, false).map_err(|_| invalid())? {
        let Value::Object(row) = row else {
            return Err(invalid());
        };
        if !same_keys(&row, &["asset_id", "kind", "dash_manifest", "hls_master"]) {
            return Err(invalid());
        }
        let kind = row["kind"].as_str().ok_or_else(invalid)?;
        if kind != "dash-cenc" && kind != "hls-clear" {
            return Err(invalid());
        }
        let asset_id = row["asset_id"].as_str().ok_or_else(invalid)?;
        #[cfg(unix)]
        let comparison_id = asset_id.to_string();
        #[cfg(windows)]
        let comparison_id = asset_id.to_ascii_lowercase();
        if seen.contains(&comparison_id)
            || (kind == "dash-cenc" && !crate::batch_support::py_truthy(&row["dash_manifest"]))
        {
            return Err(invalid());
        }
        seen.insert(comparison_id);
        let dash = optional_ref(&row["dash_manifest"]).map_err(|_| invalid())?;
        let hls = optional_ref(&row["hls_master"]).map_err(|_| invalid())?;
        items.push(AssetWorkItem::new(asset_id, kind, dash, hls).map_err(|_| invalid())?);
    }
    Ok(items)
}

/// `ObjectRef(value) if value else None` of a JSON value.
fn optional_ref(value: &Value) -> Result<Option<ObjectRef>, StorageError> {
    if !crate::batch_support::py_truthy(value) {
        return Ok(None);
    }
    let key = value
        .as_str()
        .ok_or_else(|| StorageError::new("object-key-invalid"))?;
    ObjectRef::new(key).map(Some)
}

/// The terminal entries of a resume log: the latest entry per asset, in order of first
/// appearance (`load_resume_log`'s dict).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResumeLog {
    order: Vec<String>,
    entries: HashMap<String, ResumeEntry>,
}

impl ResumeLog {
    /// The latest entry of an asset.
    pub fn get(&self, asset_id: &str) -> Option<&ResumeEntry> {
        self.entries.get(asset_id)
    }

    /// Number of assets with an entry.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// Whether no asset has an entry.
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The asset ids in order of first appearance (`list(load_resume_log(path))`).
    pub fn asset_ids(&self) -> Vec<&str> {
        self.order.iter().map(String::as_str).collect()
    }

    fn insert(&mut self, entry: ResumeEntry) {
        if !self.entries.contains_key(&entry.asset_id) {
            self.order.push(entry.asset_id.clone());
        }
        self.entries.insert(entry.asset_id.clone(), entry);
    }
}

/// `load_resume_log`: a missing log is empty; a partial header or an incomplete final line
/// (a killed append) is recovered; anything else malformed is `resume-log-invalid`.
pub fn load_resume_log(path: &Path) -> Result<ResumeLog, BatchError> {
    let invalid = || BatchError::new("resume-log-invalid");
    let mut log = ResumeLog::default();
    for row in rows(path, true, true).map_err(|_| invalid())? {
        let Value::Object(row) = row else {
            return Err(invalid());
        };
        if !same_keys(&row, &["asset_id", "outcome", "reason", "attempts", "ts"]) {
            return Err(invalid());
        }
        let entry = ResumeEntry {
            asset_id: row["asset_id"].as_str().ok_or_else(invalid)?.to_string(),
            outcome: AssetOutcome::parse(&row["outcome"]).ok_or_else(invalid)?,
            reason: row["reason"].as_str().ok_or_else(invalid)?.to_string(),
            attempts: row["attempts"].clone(),
            ts: row["ts"].clone(),
        };
        entry.validate().map_err(|_| invalid())?;
        log.insert(entry);
    }
    Ok(log)
}

/// `append_resume_entry`: one fsynced write of one complete line, with the header first when
/// the log is new (or only a partial header). A killed earlier append is repaired first: an
/// unparsable incomplete tail is cut back to the last newline, a complete-but-unterminated
/// record gets its newline.
pub fn append_resume_entry(path: &Path, entry: &ResumeEntry) -> Result<(), RunError> {
    if path.is_symlink() {
        return Err(BatchError::new("resume-log-invalid").into());
    }
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        std::fs::create_dir_all(parent)?;
        #[cfg(windows)]
        crate::native::create_private_dir_all(parent)?;
    }
    #[cfg(unix)]
    let mut handle = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .open(path)?
    };
    // This Windows handle denies concurrent write/delete sharing. Validate the
    // same held object before repairing or appending any resume record.
    #[cfg(windows)]
    let mut handle = crate::native::create_private_update(path)?;
    let mut content = Vec::new();
    handle.read_to_end(&mut content)?;
    let text = std::str::from_utf8(&content)
        .map_err(|_| RunError::Value("resume-log-not-utf-8".to_string()))?;
    let created = partial_header(text);
    if created {
        handle.set_len(0)?;
    } else if !content.is_empty() && !content.ends_with(b"\n") {
        let cut = content
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |i| i + 1);
        if serde_json::from_slice::<Value>(&content[cut..]).is_ok() {
            handle.seek(io::SeekFrom::End(0))?;
            handle.write_all(b"\n")?;
        } else {
            handle.set_len(u64::try_from(cut).expect("a length fits u64"))?;
        }
    }
    handle.seek(io::SeekFrom::End(0))?;
    let mut payload = String::new();
    if created {
        payload.push_str(HEADER_SPACED);
        payload.push('\n');
    }
    payload.push_str(&COMPACT.dumps(&entry.to_value()));
    payload.push('\n');
    let written = handle.write(payload.as_bytes())?;
    if written != payload.len() {
        return Err(BatchError::new("resume-write-failed").into());
    }
    handle.sync_all()?;
    Ok(())
}

/// `plan_batch`: `{asset_id, action, reason}` per item — `run`, `skip` (done-pass) or
/// `quarantine` (done-fail) — with no staging, storage access or write.
pub fn plan_batch(
    work_list: &[AssetWorkItem],
    resume_log: &Path,
    retry_failed: bool,
) -> Result<Vec<Record>, BatchError> {
    let terminal = load_resume_log(resume_log)?;
    let mut plan = Vec::new();
    for work in work_list {
        let previous = terminal.get(&work.asset_id);
        let mut action = "run";
        if let Some(entry) = previous
            && !(retry_failed && entry.outcome == AssetOutcome::DoneFail)
        {
            action = if entry.outcome == AssetOutcome::DonePass {
                "skip"
            } else {
                "quarantine"
            };
        }
        let mut row = Record::new();
        row.insert("asset_id".into(), Value::String(work.asset_id.clone()));
        row.insert("action".into(), Value::String(action.into()));
        row.insert(
            "reason".into(),
            previous.map_or(Value::Null, |entry| Value::String(entry.reason.clone())),
        );
        plan.push(row);
    }
    Ok(plan)
}

/// `_write_private`: the bytes written to a new or truncated mode-0600 file, synced.
pub(crate) fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        std::fs::create_dir_all(parent)?;
        #[cfg(windows)]
        crate::native::create_private_dir_all(parent)?;
    }
    #[cfg(unix)]
    let mut handle = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?
    };
    #[cfg(windows)]
    let mut handle = crate::native::create_private_or_truncate(path)?;
    handle.write_all(data)?;
    handle.flush()?;
    handle.sync_all()
}

// ------------------------------------------------------------------------------------------
// Seams: the per-asset conversion, the clear-HLS source functions and Python's injected
// module functions.
// ------------------------------------------------------------------------------------------

/// The per-asset conversion `_execute_attempt` calls: Python's `runner(work, config, stage,
/// keys, on_heartbeat) -> int` (the process exit code; 0 success, 1 a content failure).
/// [`CliRunner`] is `_run_asset`; any other implementation is an in-process test seam.
pub trait AssetRunner {
    /// Convert one staged asset.
    fn run_asset(
        &self,
        work: &AssetWorkItem,
        config: &BatchConfig,
        stage: &Path,
        keys: &KeyMaterial,
        on_heartbeat: &dyn Fn(),
    ) -> Result<i32, AttemptError>;
}

impl<F> AssetRunner for F
where
    F: Fn(
        &AssetWorkItem,
        &BatchConfig,
        &Path,
        &KeyMaterial,
        &dyn Fn(),
    ) -> Result<i32, AttemptError>,
{
    fn run_asset(
        &self,
        work: &AssetWorkItem,
        config: &BatchConfig,
        stage: &Path,
        keys: &KeyMaterial,
        on_heartbeat: &dyn Fn(),
    ) -> Result<i32, AttemptError> {
        self(work, config, stage, keys, on_heartbeat)
    }
}

/// The clear-HLS source functions `batch` calls in-process, one method per function
/// ([`crate::batch_support::DefaultHlsSource`] in production).
pub trait HlsSource: Sync {
    /// `source::read_analysis(path).asset_id`: the frozen snapshot verified (its digest and
    /// every recorded file). [`AttemptError::Other`] is an exception `_commands` does not catch
    /// (outside `OSError` / `ValueError`): it escapes as unclassified; any other error is
    /// `hls-config-invalid` there.
    fn read_analysis(&self, path: &Path) -> Result<String, AttemptError>;
    /// `source::_Fetcher(hls_cli.fetch_policy(policy), directory)`.
    fn open_fetcher(
        &self,
        policy: &Value,
        directory: &Path,
    ) -> Result<Box<dyn PlaylistFetcher>, AttemptError>;
    /// `source::_lines(body)`: stripped, non-empty lines of a playlist with known tags.
    fn lines(&self, body: &[u8]) -> Result<Vec<String>, AttemptError>;
    /// `source::_child_url(base, reference)`.
    fn child_url(&self, base: &str, reference: &str) -> Result<String, AttemptError>;
}

/// `source::_Fetcher` as the preflight uses it.
pub trait PlaylistFetcher {
    /// `read(url, playlist=True)`: the body and the effective URL.
    fn read_playlist(&mut self, url: &str) -> Result<(Vec<u8>, String), AttemptError>;
    /// `close_all`.
    fn close_all(&mut self);
}

/// `_preflight_source_graphs` as a replaceable function.
pub type PreflightFn = dyn Fn(&[AssetWorkItem], &BatchConfig) -> Result<(), RunError> + Sync;
/// `_stage_inputs` as a replaceable function.
pub type StageFn = dyn Fn(&AssetWorkItem, &BatchConfig) -> Result<PathBuf, AttemptError> + Sync;

/// Injectable preflight, staging, worker-command, and source operations.
/// These overrides apply only in this process; worker processes use production adapters.
pub struct Seams<'a> {
    /// Replaces `_preflight_source_graphs`.
    pub preflight: Option<&'a PreflightFn>,
    /// Replaces `_stage_inputs` (in-process attempts only).
    pub stage_inputs: Option<&'a StageFn>,
    /// Replaces `_worker_command`.
    pub worker_command: Option<Vec<OsString>>,
    /// The clear-HLS source functions.
    pub source: &'a dyn HlsSource,
}

impl Default for Seams<'static> {
    fn default() -> Self {
        Seams {
            preflight: None,
            stage_inputs: None,
            worker_command: None,
            source: &DefaultHlsSource,
        }
    }
}

impl fmt::Debug for Seams<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Seams")
            .field("preflight", &self.preflight.is_some())
            .field("stage_inputs", &self.stage_inputs.is_some())
            .field("worker_command", &self.worker_command)
            .finish_non_exhaustive()
    }
}

// ------------------------------------------------------------------------------------------
// One attempt.
// ------------------------------------------------------------------------------------------

/// `_classify`: the reason token and whether it is retryable. (An `Interrupted` attempt is
/// never classified; it is reimplemented here as `unclassified` for completeness.)
pub fn classify(error: &AttemptError) -> (String, bool) {
    match error {
        AttemptError::KeyNotAvailable => ("missing-key".to_string(), false),
        AttemptError::Retry(reason) => (reason.clone(), true),
        AttemptError::Storage(error) if error.is_retryable() => {
            ("storage-transient".to_string(), true)
        }
        AttemptError::Batch(error) if FATAL.contains(&error.token()) => {
            (error.token().to_string(), false)
        }
        AttemptError::Batch(_) | AttemptError::Value(_) => {
            ("malformed-manifest".to_string(), false)
        }
        AttemptError::Storage(_) | AttemptError::Other | AttemptError::Interrupted(_) => {
            ("unclassified".to_string(), false)
        }
    }
}

/// `_execute_attempt`: validate and stage the whole source graph, clear the owned output
/// prefix (and any orphan marker), resolve the keys, convert, upload `cbcs/`, `manifests/` and
/// `reports/`, then publish and verify the `_complete.json` marker last.
pub fn execute_attempt(
    work: &AssetWorkItem,
    config: &BatchConfig,
    runner: &dyn AssetRunner,
    heartbeat: &dyn Fn(),
    seams: &Seams<'_>,
) -> Result<(), AttemptError> {
    if work.kind != "dash-cenc" && work.kind != "hls-clear" {
        return Err(BatchError::new("unsupported-kind").into());
    }
    let prefix = ObjectRef::new(format!("{}/{}", config.output_prefix, work.asset_id))?;
    let stage = match seams.stage_inputs {
        Some(stage_inputs) => stage_inputs(work, config)?,
        None => stage_inputs(work, config, seams.source)?,
    };
    config.storage.delete_prefix(&prefix)?;
    let keys = config.keys.key_material(&work.asset_id)?;
    let code = runner.run_asset(work, config, &stage, &keys, heartbeat)?;
    if code != 0 {
        let token = if code == 1 {
            "content-fail"
        } else {
            "malformed-manifest"
        };
        return Err(BatchError::new(token).into());
    }
    let asset_dir = config.output_root.join(&work.asset_id);
    let items = output_items(&asset_dir, &prefix)?;
    if items.is_empty() {
        return Err(BatchError::new("content-fail").into());
    }
    config.storage.put_many(&items)?;
    let marker = asset_dir.join("reports").join("_complete.json");
    let mut body = Map::new();
    body.insert(
        "schema_version".into(),
        Value::String(SCHEMA_VERSION.into()),
    );
    body.insert("asset_id".into(), Value::String(work.asset_id.clone()));
    body.insert("outcome".into(), Value::String("done-pass".into()));
    body.insert("object_count".into(), Value::from(items.len()));
    write_private(
        &marker,
        Dumps::DEFAULT.dumps(&Value::Object(body)).as_bytes(),
    )
    .map_err(|_| AttemptError::Other)?;
    let marker_ref = ObjectRef::new(format!("{}/_complete.json", prefix.key()))?;
    config.storage.put_many(&[(marker, marker_ref.clone())])?;
    if !config.storage.exists(&marker_ref)? {
        return Err(AttemptError::Retry("storage-transient".to_string()));
    }
    Ok(())
}

/// Every file under `cbcs/`, `manifests/` and `reports/` with its key under `prefix`; a
/// symbolic link is `unclassified`.
fn output_items(
    asset_dir: &Path,
    prefix: &ObjectRef,
) -> Result<Vec<(PathBuf, ObjectRef)>, AttemptError> {
    let mut items = Vec::new();
    for name in ["cbcs", "manifests", "reports"] {
        let directory = asset_dir.join(name);
        if directory.is_dir() {
            collect_files(&directory, asset_dir, prefix, &mut items)?;
        }
    }
    Ok(items)
}

fn collect_files(
    directory: &Path,
    asset_dir: &Path,
    prefix: &ObjectRef,
    items: &mut Vec<(PathBuf, ObjectRef)>,
) -> Result<(), AttemptError> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Ok(());
    };
    let mut paths: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
    paths.sort();
    let mut directories = Vec::new();
    for path in paths {
        if path.is_symlink() {
            return Err(BatchError::new("unclassified").into());
        }
        if path.is_file() {
            let relative = path
                .strip_prefix(asset_dir)
                .map_err(|_| AttemptError::Other)?;
            let relative = relative
                .components()
                .map(|component| component.as_os_str().to_str().ok_or(AttemptError::Other))
                .collect::<Result<Vec<_>, _>>()?
                .join("/");
            let key = ObjectRef::new(format!("{}/{relative}", prefix.key()))?;
            items.push((path, key));
        } else if path.is_dir() {
            directories.push(path);
        }
    }
    for directory in directories {
        collect_files(&directory, asset_dir, prefix, items)?;
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// run_batch.
// ------------------------------------------------------------------------------------------

/// `run_batch(work_list, config, on_log=…, runner=…)` with the real preflight, staging and
/// worker command.
pub fn run_batch(
    work_list: &[AssetWorkItem],
    config: &BatchConfig,
    on_log: Option<&dyn Sink>,
    runner: Option<&dyn AssetRunner>,
) -> Result<BatchReport, RunError> {
    run_batch_with(work_list, config, on_log, runner, &Seams::default())
}

/// The progress records of one run (`emit` in `run_batch`).
struct Progress<'a> {
    start: Instant,
    total: usize,
    processed: std::cell::Cell<usize>,
    on_log: Option<&'a dyn Sink>,
}

impl Progress<'_> {
    fn emit(&self, message: &str, work: &AssetWorkItem, index: usize) {
        let Some(sink) = self.on_log else {
            return;
        };
        let elapsed = self.start.elapsed().as_secs_f64();
        let processed = exact(self.processed.get());
        let throughput = if elapsed == 0.0 {
            0.0
        } else {
            processed * 3600.0 / elapsed
        };
        let mut record = Record::new();
        record.insert(
            "schema_version".into(),
            Value::String(SCHEMA_VERSION.into()),
        );
        record.insert("ts".into(), Value::String(now_iso()));
        record.insert("stage".into(), Value::String("batch".into()));
        record.insert("level".into(), Value::String("info".into()));
        record.insert("message".into(), Value::String(message.into()));
        record.insert("asset_id".into(), Value::String(work.asset_id.clone()));
        record.insert("index".into(), Value::from(index));
        record.insert("total".into(), Value::from(self.total));
        record.insert("elapsed_s".into(), Value::from(elapsed));
        record.insert("throughput_assets_per_h".into(), rate(elapsed, throughput));
        let eta = if throughput == 0.0 {
            Value::Null
        } else {
            Value::from(exact(self.total - index) * 3600.0 / throughput)
        };
        record.insert("eta_s".into(), eta);
        sink.emit(&record);
    }
}

/// A count as a float (exact below 2^53, as Python's int-to-float division is).
fn exact(count: usize) -> f64 {
    u32::try_from(count).map_or_else(|_| f64::from(u32::MAX), f64::from)
}

/// Removes the asset's local directory when the asset ends, however it ends (Python's
/// `finally: shutil.rmtree(asset_dir, ignore_errors=True)`).
struct ScratchGuard(PathBuf);

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// [`run_batch`] with explicit seams (injected dependencies).
pub fn run_batch_with(
    work_list: &[AssetWorkItem],
    config: &BatchConfig,
    on_log: Option<&dyn Sink>,
    runner: Option<&dyn AssetRunner>,
    seams: &Seams<'_>,
) -> Result<BatchReport, RunError> {
    config.validate()?;
    check_work_list(work_list, config)?;
    let terminal = load_resume_log(&config.resume_log)?;
    match seams.preflight {
        Some(preflight) => preflight(work_list, config)?,
        None => preflight_source_graphs(work_list, config, seams.source)?,
    }
    let progress = Progress {
        start: Instant::now(),
        total: work_list.len(),
        processed: std::cell::Cell::new(0),
        on_log,
    };
    let (mut skipped, mut retried) = (0, 0);
    let mut outcomes = Vec::with_capacity(work_list.len());
    for (offset, work) in work_list.iter().enumerate() {
        let index = offset + 1;
        if let Some(previous) = terminal.get(&work.asset_id)
            && !(config.retry_failed && previous.outcome == AssetOutcome::DoneFail)
        {
            skipped += 1;
            outcomes.push(previous.clone());
            continue;
        }
        progress.emit("asset-start", work, index);
        let guard = ScratchGuard(config.output_root.join(&work.asset_id));
        let (reason, attempts) = process_asset(work, index, config, runner, seams, &progress)?;
        let outcome = if reason == "ok" {
            AssetOutcome::DonePass
        } else {
            AssetOutcome::DoneFail
        };
        let entry = ResumeEntry::new(&work.asset_id, outcome, &reason, attempts, &now_iso())?;
        append_resume_entry(&config.resume_log, &entry)?;
        outcomes.push(entry);
        retried += usize::from(attempts > 1);
        progress.processed.set(progress.processed.get() + 1);
        progress.emit("asset-end", work, index);
        drop(guard);
    }
    let elapsed = progress.start.elapsed().as_secs_f64();
    let processed = exact(progress.processed.get());
    let report = BatchReport {
        schema_version: SCHEMA_VERSION.to_string(),
        total: work_list.len(),
        passed: outcomes
            .iter()
            .filter(|e| e.outcome == AssetOutcome::DonePass)
            .count(),
        failed: outcomes
            .iter()
            .filter(|e| e.outcome == AssetOutcome::DoneFail)
            .count(),
        skipped,
        retried,
        reasons: count_reasons(&outcomes),
        outcomes,
        cpu_count: crate::batch_support::cpu_count(),
        max_workers: config.max_workers,
        wall_clock_s: elapsed,
        throughput_assets_per_h: if elapsed == 0.0 {
            0.0
        } else {
            processed * 3600.0 / elapsed
        },
    };
    let destination = config.output_root.join("_batch");
    write_private(&destination.join("batch-report.json"), &report.json_bytes())?;
    write_private(
        &destination.join("batch-report.md"),
        report.markdown().as_bytes(),
    )?;
    Ok(report)
}

/// `Counter(e.reason for e in outcomes)` in order of first appearance.
fn count_reasons(outcomes: &[ResumeEntry]) -> Vec<(String, usize)> {
    let mut reasons: Vec<(String, usize)> = Vec::new();
    for entry in outcomes {
        match reasons
            .iter_mut()
            .find(|(reason, _)| *reason == entry.reason)
        {
            Some((_, count)) => *count += 1,
            None => reasons.push((entry.reason.clone(), 1)),
        }
    }
    reasons
}

/// The work-list checks before anything is read: unique ids, no symbolic-link output
/// directory, a resume log outside every asset directory, no declared source under the
/// output prefix.
fn check_work_list(work_list: &[AssetWorkItem], config: &BatchConfig) -> Result<(), RunError> {
    let mut ids = HashSet::new();
    for work in work_list {
        validate_asset_id(&work.asset_id)?;
        #[cfg(unix)]
        let id = work.asset_id.clone();
        #[cfg(windows)]
        let id = work.asset_id.to_ascii_lowercase();
        if !ids.insert(id) {
            return Err(BatchError::new("duplicate-asset-id").into());
        }
    }
    let resume = resolve(&config.resume_log)?;
    for work in work_list {
        let asset_dir = config.output_root.join(&work.asset_id);
        if asset_dir.is_symlink() {
            return Err(BatchError::new("unsafe-output-path").into());
        }
        if is_relative_to(&resume, &resolve(&asset_dir)?) {
            return Err(BatchError::new("resume-output-overlap").into());
        }
        for object in [&work.dash_manifest, &work.hls_master]
            .into_iter()
            .flatten()
        {
            if is_output_key(object.key(), &config.output_prefix) {
                return Err(BatchError::new("source-output-overlap").into());
            }
        }
    }
    Ok(())
}

/// `key == prefix or key.startswith(prefix + "/")`.
pub(crate) fn is_output_key(key: &str, prefix: &str) -> bool {
    key == prefix
        || key
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// One asset's attempts: the terminal reason and the attempts used. `Interrupted` (and an
/// `OSError` while clearing the scratch directory) ends the run.
fn process_asset(
    work: &AssetWorkItem,
    index: usize,
    config: &BatchConfig,
    runner: Option<&dyn AssetRunner>,
    seams: &Seams<'_>,
    progress: &Progress<'_>,
) -> Result<(String, u32), RunError> {
    let asset_dir = config.output_root.join(&work.asset_id);
    let max_attempts = u32::try_from(config.retry.max_attempts).expect("validated 1..=10");
    let heartbeat = || progress.emit("heartbeat", work, index);
    let mut reason = "unclassified".to_string();
    let mut attempts = 0;
    for attempt in 1..=max_attempts {
        attempts = attempt;
        clear_scratch(work, &asset_dir)?;
        let result = match runner {
            None => supervised(work, config, &heartbeat, seams),
            Some(runner) => execute_attempt(work, config, runner, &heartbeat, seams),
        };
        let error = match result {
            Ok(()) => {
                reason = "ok".to_string();
                break;
            }
            Err(AttemptError::Interrupted(message)) => return Err(RunError::Interrupted(message)),
            Err(error) => error,
        };
        let (token, retryable) = match &error {
            AttemptError::Retry(token) => (token.clone(), true),
            AttemptError::Storage(storage) if storage.is_retryable() => {
                ("storage-transient".to_string(), true)
            }
            _ => (classify(&error).0, false),
        };
        if !retryable {
            reason = token;
            break;
        }
        if attempt == max_attempts {
            reason = format!("retries-exhausted:{token}");
            break;
        }
        let backoff = config.retry.backoff_max_s.min(
            config.retry.backoff_base_s * 2f64.powi(i32::try_from(attempt - 1).expect("small")),
        );
        let pause = crate::batch_support::uniform(backoff);
        std::thread::sleep(std::time::Duration::try_from_secs_f64(pause).unwrap_or_default());
    }
    Ok((reason, attempts))
}

/// A supervised attempt mapped back to the exceptions `run_batch` catches: `ok`, a retryable
/// token (`_RetryFailure`) or a fatal one (`BatchError`).
fn supervised(
    work: &AssetWorkItem,
    config: &BatchConfig,
    heartbeat: &dyn Fn(),
    seams: &Seams<'_>,
) -> Result<(), AttemptError> {
    let command = seams.worker_command.clone().unwrap_or_else(worker_command);
    let (token, retryable) = supervise_attempt(work, config, heartbeat, &command)?;
    if token == "ok" {
        return Ok(());
    }
    if retryable {
        return Err(AttemptError::Retry(token));
    }
    Err(BatchError::new(&token).into())
}

/// Before each attempt: the asset's scratch is removed — for a clear-HLS asset with a frozen
/// source everything except `clear/source/`, which the next attempt verifies again.
fn clear_scratch(work: &AssetWorkItem, asset_dir: &Path) -> Result<(), RunError> {
    if !asset_dir.exists() {
        return Ok(());
    }
    if work.kind != "hls-clear" || !asset_dir.join("clear/source").is_dir() {
        std::fs::remove_dir_all(asset_dir)?;
        return Ok(());
    }
    for child in std::fs::read_dir(asset_dir)? {
        let child = child?.path();
        let name = child.file_name().unwrap_or_default();
        if name == "clear" && !child.is_symlink() {
            for clear_child in std::fs::read_dir(&child)? {
                let clear_child = clear_child?.path();
                if clear_child.file_name().unwrap_or_default() != "source" {
                    remove_path(&clear_child)?;
                }
            }
        } else {
            remove_path(&child)?;
        }
    }
    Ok(())
}

/// `shutil.rmtree` for a real directory, `unlink` for anything else (a link included).
fn remove_path(path: &Path) -> io::Result<()> {
    if path.is_dir() && !path.is_symlink() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Read up to `limit` bytes from the end of a file (the tail of a child's output).
pub(crate) fn read_tail(file: &mut std::fs::File, limit: u64) -> io::Result<Vec<u8>> {
    let end = file.seek(io::SeekFrom::End(0))?;
    file.seek(io::SeekFrom::Start(end.saturating_sub(limit)))?;
    let mut data = Vec::new();
    file.read_to_end(&mut data)?;
    Ok(data)
}

/// Read up to `limit` bytes from the start of a file.
pub(crate) fn read_head(file: &mut std::fs::File, limit: u64) -> io::Result<Vec<u8>> {
    file.seek(io::SeekFrom::Start(0))?;
    let mut data = Vec::new();
    file.take(limit).read_to_end(&mut data)?;
    Ok(data)
}
