//! Isolated asset workers and their supervisors.
//!
//! Each attempt runs staging, conversion, upload, and marker verification in its own
//! process group. Configuration travels over stdin, heartbeats report progress, and
//! the supervisor terminates the whole group at the asset deadline or attempt end.

use super::commands::{after, exit_code};
use super::{
    AssetWorkItem, AttemptError, BatchConfig, BatchError, CliRunner, EnvKeyProvider, FATAL,
    FileKeyProvider, KeyNotAvailable, KeyProvider, KeyProviderKind, RETRYABLE, RetryPolicy, Seams,
    classify, execute_attempt,
};
use crate::batch_support::DefaultHlsSource;
use crate::canonical::Dumps;
use crate::config::KeyMaterial;
use crate::storage::{
    LocalStorageBackend, ObjectRef, S3StorageBackend, StorageBackend, StorageLocation,
    TransferPolicy,
};
use serde_json::{Value, json};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The argument that makes the tool's executable an asset worker: the `drmr` binary runs
/// [`worker_main`] (and exits with its code) when its arguments are exactly `[WORKER_ARG]`,
/// before any other argument parsing.
pub const WORKER_ARG: &str = "--_batch-worker";
/// The largest private configuration a worker accepts.
const MAX_PAYLOAD: usize = 65_536;
/// A worker answer longer than this is a crash.
const MAX_ANSWER: usize = 256;

/// `_worker_command`: this executable with [`WORKER_ARG`].
pub fn worker_command() -> Vec<OsString> {
    let executable = std::env::current_exe().map_or_else(|_| OsString::from("drmr"), Into::into);
    vec![executable, WORKER_ARG.into()]
}

fn path_text(path: &Path) -> Result<Value, AttemptError> {
    // The envelope carries paths as JSON strings; a non-UTF-8 path cannot travel.
    path.to_str()
        .map(|text| Value::String(text.to_string()))
        .ok_or_else(|| BatchError::new("worker-config-invalid").into())
}

fn transfer_value(policy: &TransferPolicy) -> Value {
    json!({
        "batch_size": policy.batch_size,
        "max_concurrency": policy.max_concurrency,
        "per_object_timeout_s": policy.per_object_timeout_s,
        "max_attempts": policy.max_attempts,
        "backoff_base_s": policy.backoff_base_s,
        "backoff_max_s": policy.backoff_max_s,
        "max_object_bytes": policy.max_object_bytes,
        "max_objects": policy.max_objects,
        "max_asset_bytes": policy.max_asset_bytes,
    })
}

fn retry_value(policy: &RetryPolicy) -> Value {
    json!({
        "max_attempts": policy.max_attempts,
        "backoff_base_s": policy.backoff_base_s,
        "backoff_max_s": policy.backoff_max_s,
        "per_asset_timeout_s": policy.per_asset_timeout_s,
        "heartbeat_s": policy.heartbeat_s,
    })
}

/// `_worker_payload`: the private configuration of one attempt. A custom key provider's result
/// is resolved here and travels in the payload (the pipe is private); the built-in providers are
/// rebuilt in the worker. A store the worker cannot rebuild is `storage-config-invalid`.
pub fn worker_payload(work: &AssetWorkItem, config: &BatchConfig) -> Result<Vec<u8>, AttemptError> {
    let storage = match config.storage.location() {
        Some(StorageLocation::Local { root }) => {
            json!({"kind": "local", "root": path_text(&root)?})
        }
        Some(StorageLocation::S3 {
            bucket,
            region,
            endpoint,
        }) => {
            let mut s3 = json!({"kind": "s3", "bucket": bucket, "region": region});
            if let Some(endpoint) = endpoint {
                s3["endpoint"] = Value::String(endpoint);
            }
            s3
        }
        None => return Err(BatchError::new("storage-config-invalid").into()),
    };
    let provider = match config.keys.kind() {
        KeyProviderKind::File(path) => json!({"kind": "file", "path": path_text(path)?}),
        KeyProviderKind::Env => json!({"kind": "env"}),
        KeyProviderKind::Provided => {
            let key = config.keys.key_material(&work.asset_id)?;
            json!({"kind": "provided", "kid": key.key_id_hex(), "key": key.key_hex(), "iv": key.iv_hex()})
        }
    };
    let storage_policy = config
        .storage
        .policy()
        .ok_or_else(|| BatchError::new("storage-config-invalid"))?;
    let optional_path = |path: Option<&PathBuf>| -> Result<Value, AttemptError> {
        path.map_or(Ok(Value::Null), |path| path_text(path))
    };
    let data = json!({
        "work": {
            "asset_id": work.asset_id,
            "kind": work.kind,
            "dash_manifest": work.dash_manifest.as_ref().map(ObjectRef::key),
            "hls_master": work.hls_master.as_ref().map(ObjectRef::key),
        },
        "storage": storage,
        "provider": provider,
        "transfer": transfer_value(&config.transfer),
        "storage_policy": transfer_value(&storage_policy),
        "retry": retry_value(&config.retry),
        "output_root": path_text(&config.output_root)?,
        "resume_log": path_text(&config.resume_log)?,
        "output_prefix": config.output_prefix,
        "max_workers": config.max_workers,
        "hls_config_dir": optional_path(config.hls_config_dir.as_ref())?,
        "packager_bin": optional_path(config.packager_bin.as_ref())?,
    });
    let payload = Dumps::DEFAULT.dumps(&data).into_bytes();
    if payload.len() > MAX_PAYLOAD {
        return Err(BatchError::new("worker-config-invalid").into());
    }
    Ok(payload)
}

/// A provider holding one asset's key material resolved by the supervisor. Python sets the
/// three texts in the worker's environment and reads them back with an `EnvKeyProvider`, so
/// texts that are not valid key material fail only when the attempt asks for its keys — after
/// staging and the owned-prefix delete — as `missing-key` (`None` here).
struct ProvidedKeys(Option<KeyMaterial>);

impl KeyProvider for ProvidedKeys {
    fn key_material(&self, _asset_id: &str) -> Result<KeyMaterial, KeyNotAvailable> {
        self.0.clone().ok_or(KeyNotAvailable)
    }
}

/// `_worker_main`: read the private configuration from stdin, run one attempt with the real
/// staging and per-asset commands, print `[reason, retryable]` and exit 0. A crash (a panic,
/// an interruption) exits non-zero without an answer.
pub fn worker_main() -> i32 {
    crate::native::mark_asset_worker();
    let mut input = Vec::new();
    let read = std::io::stdin()
        .lock()
        .take(u64::try_from(MAX_PAYLOAD + 1).expect("small"))
        .read_to_end(&mut input);
    let result = match read {
        Ok(_) => worker_attempt(&input),
        Err(_) => Err(AttemptError::Other),
    };
    let (reason, retryable) = match result {
        Ok(()) => ("ok".to_string(), false),
        Err(AttemptError::Interrupted(_)) => return 1,
        Err(error) => classify(&error),
    };
    let line = Dumps::DEFAULT.dumps(&json!([reason, retryable]));
    let mut stdout = std::io::stdout().lock();
    if writeln!(stdout, "{line}")
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return 1;
    }
    0
}

fn field<'a>(data: &'a Value, name: &str) -> Result<&'a Value, AttemptError> {
    data.get(name).ok_or(AttemptError::Other)
}

fn text<'a>(data: &'a Value, name: &str) -> Result<&'a str, AttemptError> {
    field(data, name)?.as_str().ok_or(AttemptError::Other)
}

fn optional_text<'a>(data: &'a Value, name: &str) -> Result<Option<&'a str>, AttemptError> {
    match field(data, name)? {
        Value::Null => Ok(None),
        Value::String(text) if text.is_empty() => Ok(None),
        Value::String(text) => Ok(Some(text)),
        _ => Err(AttemptError::Other),
    }
}

fn transfer_from(data: &Value) -> Result<TransferPolicy, AttemptError> {
    let int = |name: &str| field(data, name)?.as_i64().ok_or(AttemptError::Other);
    let float = |name: &str| field(data, name)?.as_f64().ok_or(AttemptError::Other);
    let map = data.as_object().ok_or(AttemptError::Other)?;
    if map.len() != 9 {
        return Err(AttemptError::Other);
    }
    Ok(TransferPolicy {
        batch_size: int("batch_size")?,
        max_concurrency: int("max_concurrency")?,
        per_object_timeout_s: float("per_object_timeout_s")?,
        max_attempts: int("max_attempts")?,
        backoff_base_s: float("backoff_base_s")?,
        backoff_max_s: float("backoff_max_s")?,
        max_object_bytes: int("max_object_bytes")?,
        max_objects: int("max_objects")?,
        max_asset_bytes: int("max_asset_bytes")?,
    }
    .validate()?)
}

fn retry_from(data: &Value) -> Result<RetryPolicy, AttemptError> {
    let float = |name: &str| field(data, name)?.as_f64().ok_or(AttemptError::Other);
    let map = data.as_object().ok_or(AttemptError::Other)?;
    if map.len() != 5 {
        return Err(AttemptError::Other);
    }
    Ok(RetryPolicy {
        max_attempts: field(data, "max_attempts")?
            .as_i64()
            .ok_or(AttemptError::Other)?,
        backoff_base_s: float("backoff_base_s")?,
        backoff_max_s: float("backoff_max_s")?,
        per_asset_timeout_s: float("per_asset_timeout_s")?,
        heartbeat_s: float("heartbeat_s")?,
    }
    .validate()?)
}

fn storage_from(data: &Value) -> Result<Arc<dyn StorageBackend>, AttemptError> {
    let policy = transfer_from(field(data, "storage_policy")?)?;
    let prefix = text(data, "output_prefix")?;
    let location = field(data, "storage")?;
    if text(location, "kind")? == "local" {
        let root = Path::new(text(location, "root")?);
        return Ok(Arc::new(LocalStorageBackend::with(root, policy, prefix)?));
    }
    let endpoint = location.get("endpoint").and_then(Value::as_str);
    Ok(Arc::new(S3StorageBackend::with(
        text(location, "bucket")?,
        text(location, "region")?,
        policy,
        prefix,
        endpoint,
    )?))
}

fn keys_from(data: &Value) -> Result<Arc<dyn KeyProvider>, AttemptError> {
    let provider = field(data, "provider")?;
    match text(provider, "kind")? {
        "file" => {
            let output_root = Path::new(text(data, "output_root")?);
            let path = Path::new(text(provider, "path")?);
            Ok(Arc::new(FileKeyProvider::new(path, output_root)?))
        }
        "provided" => {
            // `os.environ.update(DRM_KID=…)` refuses a value that is not text (a `TypeError`:
            // unclassified); text is only validated when the keys are asked for.
            let material = KeyMaterial::new(
                text(provider, "kid")?,
                text(provider, "key")?,
                text(provider, "iv")?,
            )
            .ok();
            Ok(Arc::new(ProvidedKeys(material)))
        }
        _ => Ok(Arc::new(EnvKeyProvider)),
    }
}

fn worker_attempt(input: &[u8]) -> Result<(), AttemptError> {
    let data: Value = serde_json::from_slice(input)
        .map_err(|_| AttemptError::Value("json-decode-error".into()))?;
    let transfer = transfer_from(field(&data, "transfer")?)?;
    let storage = storage_from(&data)?;
    let keys = keys_from(&data)?;
    let row = field(&data, "work")?;
    let reference = |name: &str| -> Result<Option<ObjectRef>, AttemptError> {
        Ok(optional_text(row, name)?.map(ObjectRef::new).transpose()?)
    };
    let work = AssetWorkItem::new(
        text(row, "asset_id")?,
        text(row, "kind")?,
        reference("dash_manifest")?,
        reference("hls_master")?,
    )?;
    let max_workers = match field(&data, "max_workers")? {
        Value::Null => None,
        value => Some(value.as_i64().ok_or(AttemptError::Other)?),
    };
    let config = BatchConfig {
        storage,
        keys,
        output_root: PathBuf::from(text(&data, "output_root")?),
        resume_log: PathBuf::from(text(&data, "resume_log")?),
        retry: retry_from(field(&data, "retry")?)?,
        transfer,
        max_workers,
        retry_failed: false,
        hls_config_dir: optional_text(&data, "hls_config_dir")?.map(PathBuf::from),
        output_prefix: text(&data, "output_prefix")?.to_string(),
        packager_bin: optional_text(&data, "packager_bin")?.map(PathBuf::from),
    };
    config.validate().map_err(|error| match error {
        super::RunError::Batch(error) => AttemptError::Batch(error),
        super::RunError::Storage(error) => AttemptError::Storage(error),
        _ => AttemptError::Other,
    })?;
    let seams = Seams::default();
    let runner = CliRunner {
        source: &DefaultHlsSource,
    };
    execute_attempt(&work, &config, &runner, &|| {}, &seams)
}

/// `_supervise_attempt`: run one attempt in a worker process group under the per-asset
/// deadline, emitting `heartbeat` while waiting. Returns the worker's `(reason, retryable)`, or
/// `("asset-timeout", true)` at the deadline; the group is killed either way. A worker that
/// exits non-zero or answers garbage is a crash (`batch-worker-interrupted`).
pub fn supervise_attempt(
    work: &AssetWorkItem,
    config: &BatchConfig,
    heartbeat: &dyn Fn(),
    command: &[OsString],
) -> Result<(String, bool), AttemptError> {
    let payload = worker_payload(work, config)?;
    let (program, arguments) = command.split_first().ok_or(AttemptError::Other)?;
    let mut child_command = Command::new(program);
    child_command
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child =
        crate::native::ManagedChild::spawn(child_command, true).map_err(|_| AttemptError::Other)?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let mut stdout = child.stdout.take().expect("stdout is piped");
    // The payload is written and the pipe closed on a thread: a worker that never reads its
    // input cannot block the supervisor past the deadline.
    std::thread::spawn(move || {
        let _ = stdin.write_all(&payload);
    });
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut raw = Vec::new();
        let _ = stdout.read_to_end(&mut raw);
        let _ = sender.send(raw);
    });
    let waited = wait(&mut child, &receiver, config, heartbeat);
    let _ = child.kill();
    let _ = child.wait();
    let Some((status, raw)) = waited? else {
        return Ok(("asset-timeout".to_string(), true));
    };
    if exit_code(status) != 0 || raw.len() > MAX_ANSWER {
        return Err(AttemptError::Interrupted(
            "batch-worker-interrupted".to_string(),
        ));
    }
    parse_answer(&raw)
}

/// Wait for the worker to exit and close its stdout (`communicate`), with heartbeats; `None`
/// at the deadline.
fn wait(
    child: &mut crate::native::ManagedChild,
    receiver: &mpsc::Receiver<Vec<u8>>,
    config: &BatchConfig,
    heartbeat: &dyn Fn(),
) -> Result<Option<(std::process::ExitStatus, Vec<u8>)>, AttemptError> {
    let start = Instant::now();
    let deadline = after(start, config.retry.per_asset_timeout_s);
    let mut next_heartbeat = after(start, config.retry.heartbeat_s);
    let mut raw: Option<Vec<u8>> = None;
    let mut status = None;
    loop {
        if raw.is_none() {
            raw = receiver.try_recv().ok();
        }
        if status.is_none() {
            status = child.try_wait().map_err(|_| AttemptError::Other)?;
        }
        if let (Some(status), Some(raw)) = (status, raw.as_ref()) {
            return Ok(Some((status, raw.clone())));
        }
        let now = Instant::now();
        if now >= deadline {
            return Ok(None);
        }
        if now >= next_heartbeat {
            heartbeat();
            next_heartbeat = after(Instant::now(), config.retry.heartbeat_s);
        }
        let pause = deadline
            .saturating_duration_since(Instant::now())
            .min(next_heartbeat.saturating_duration_since(Instant::now()))
            .clamp(Duration::from_millis(1), Duration::from_millis(5));
        std::thread::sleep(pause);
    }
}

/// The worker's answer: `reason, retryable = json.loads(raw)` — two items (an array, a
/// two-character string or a two-key object unpack), the reason a known token and the flag a
/// boolean.
fn parse_answer(raw: &[u8]) -> Result<(String, bool), AttemptError> {
    let crashed = || AttemptError::Interrupted("batch-worker-interrupted".to_string());
    let value: Value = serde_json::from_slice(raw).map_err(|_| crashed())?;
    let items: Vec<Value> = match value {
        Value::Array(items) => items,
        Value::String(text) => text.chars().map(|c| Value::String(c.to_string())).collect(),
        Value::Object(map) => map.keys().map(|key| Value::String(key.clone())).collect(),
        _ => return Err(crashed()),
    };
    let [reason, retryable]: [Value; 2] = items.try_into().map_err(|_| crashed())?;
    if matches!(reason, Value::Array(_) | Value::Object(_)) {
        // An unhashable reason is a `TypeError` in Python: unclassified.
        return Err(AttemptError::Other);
    }
    let known = reason
        .as_str()
        .filter(|token| *token == "ok" || FATAL.contains(token) || RETRYABLE.contains(token));
    match (known, retryable.as_bool()) {
        (Some(token), Some(flag)) => Ok((token.to_string(), flag)),
        _ => Err(BatchError::new("worker-result-invalid").into()),
    }
}
