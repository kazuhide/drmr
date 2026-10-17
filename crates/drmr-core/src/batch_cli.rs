//! Batch command arguments, configuration validation, and exit codes.
//!
//! Configuration errors emit a fixed token rather than private paths or parser details.
//! Exit status is 0 for complete success or planning, 1 for failed assets, 2 for invalid
//! usage or configuration, and the interruption status when interrupted.

use crate::batch::{
    BatchConfig, BatchError, EnvKeyProvider, FileKeyProvider, KeyProvider, RetryPolicy, RunError,
    plan_batch, read_work_list, run_batch,
};
use crate::batch_support::{py_float, py_int, py_path};
use crate::events::{Record, Sink};
use crate::storage::{LocalStorageBackend, S3StorageBackend, StorageBackend, TransferPolicy};
use clap::builder::{OsStringValueParser, TypedValueParser as _};
use clap::{Args, ValueEnum};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

/// The subcommand's help (`help=` of the `batch` subparser).
pub const BATCH_HELP: &str = "sequential restartable fleet repackaging";
/// The exit code of an interrupted run (a crashed worker or a `KeyboardInterrupt` seam): the
/// shell's code for Python dying of SIGINT.
pub const EXIT_INTERRUPTED: i32 = 130;

/// `--storage`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum StorageKind {
    /// `LocalStorageBackend(--storage-root)`.
    Local,
    /// `S3StorageBackend(--bucket, --region)`.
    S3,
}

/// `--key-provider`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum KeyProviderChoice {
    /// `DRM_KID` / `DRM_KEY` / `DRM_IV`.
    Env,
    /// `--key-file`.
    File,
}

fn path_arg() -> impl clap::builder::TypedValueParser<Value = PathBuf> {
    // argparse's `type=Path`: pathlib's spelling of the argument.
    OsStringValueParser::new().map(|text| py_path(&text))
}

/// The arguments of `drmr batch` (`register_batch_commands`), with the same flag
/// names and defaults.
#[derive(Clone, Debug, Args)]
#[command(about = BATCH_HELP, args_override_self = true, infer_long_args = true)]
pub struct BatchArgs {
    /// `--work-list`.
    #[arg(long = "work-list", required = true, value_parser = path_arg())]
    pub work_list: PathBuf,
    /// `--output`.
    #[arg(long = "output", required = true, value_parser = path_arg())]
    pub output: PathBuf,
    /// `--resume-log`.
    #[arg(long = "resume-log", required = true, value_parser = path_arg())]
    pub resume_log: PathBuf,
    /// `--storage {local,s3}`.
    #[arg(long = "storage", value_enum, default_value = "local")]
    pub storage: StorageKind,
    /// `--storage-root`.
    #[arg(long = "storage-root", value_parser = path_arg())]
    pub storage_root: Option<PathBuf>,
    /// `--bucket`.
    #[arg(long = "bucket")]
    pub bucket: Option<String>,
    /// `--region`.
    #[arg(long = "region", default_value = "ap-northeast-1")]
    pub region: String,
    /// `--output-prefix`.
    #[arg(long = "output-prefix", default_value = "output")]
    pub output_prefix: String,
    /// `--key-provider {env,file}`.
    #[arg(long = "key-provider", value_enum, default_value = "env")]
    pub key_provider: KeyProviderChoice,
    /// `--key-file`.
    #[arg(long = "key-file", value_parser = path_arg())]
    pub key_file: Option<PathBuf>,
    /// `--hls-config-dir`.
    #[arg(long = "hls-config-dir", value_parser = path_arg())]
    pub hls_config_dir: Option<PathBuf>,
    /// `--packager-bin`.
    #[arg(long = "packager-bin", value_parser = path_arg())]
    pub packager_bin: Option<PathBuf>,
    /// `--max-workers` (`type=int`).
    #[arg(long = "max-workers", value_parser = py_int)]
    pub max_workers: Option<i64>,
    /// `--retry-failed`.
    #[arg(long = "retry-failed")]
    pub retry_failed: bool,
    /// `--plan`.
    #[arg(long = "plan")]
    pub plan: bool,
    /// `--max-attempts` (`type=int`, default 3).
    #[arg(long = "max-attempts", default_value = "3", value_parser = py_int)]
    pub max_attempts: i64,
    /// `--per-asset-timeout-s` (`type=float`, default 3600).
    #[arg(long = "per-asset-timeout-s", default_value = "3600", value_parser = py_float)]
    pub per_asset_timeout_s: f64,
    /// `--heartbeat-s` (`type=float`, default 60).
    #[arg(long = "heartbeat-s", default_value = "60", value_parser = py_float)]
    pub heartbeat_s: f64,
}

/// The `batch` subcommand, for a binary that builds its commands by hand.
pub fn command() -> clap::Command {
    BatchArgs::augment_args(clap::Command::new("batch"))
}

/// `cmd_batch`: plan or run the batch; records go to `sink` (`print(json.dumps(record))` in
/// Python): the plan rows, the run's batch records, then the `batch-report` summary — or one
/// `batch-config-invalid` record.
pub fn cmd_batch(args: &BatchArgs, sink: &dyn Sink) -> i32 {
    match run(args, sink) {
        Ok(code) => code,
        Err(RunError::Interrupted(_)) => EXIT_INTERRUPTED,
        Err(_) => {
            sink.emit(&config_invalid());
            2
        }
    }
}

fn run(args: &BatchArgs, sink: &dyn Sink) -> Result<i32, RunError> {
    let work = read_work_list(&args.work_list)?;
    if args.plan {
        for row in plan_batch(&work, &args.resume_log, args.retry_failed)? {
            sink.emit(&row);
        }
        return Ok(0);
    }
    let keys: Arc<dyn KeyProvider> = match (args.key_provider, &args.key_file) {
        (KeyProviderChoice::File, Some(path)) => {
            Arc::new(FileKeyProvider::new(path, &args.output)?)
        }
        (KeyProviderChoice::Env, None) => Arc::new(EnvKeyProvider),
        _ => return Err(BatchError::new("key-provider-invalid").into()),
    };
    let transfer = TransferPolicy::default();
    let storage: Arc<dyn StorageBackend> = match args.storage {
        StorageKind::Local => {
            let root = args
                .storage_root
                .as_deref()
                .ok_or_else(|| BatchError::new("storage-config-invalid"))?;
            Arc::new(LocalStorageBackend::with(
                root,
                transfer,
                &args.output_prefix,
            )?)
        }
        StorageKind::S3 => {
            let bucket = args
                .bucket
                .as_deref()
                .filter(|bucket| !bucket.is_empty())
                .ok_or_else(|| BatchError::new("storage-config-invalid"))?;
            Arc::new(S3StorageBackend::with(
                bucket,
                &args.region,
                transfer,
                &args.output_prefix,
                None,
            )?)
        }
    };
    let retry = RetryPolicy {
        max_attempts: args.max_attempts,
        per_asset_timeout_s: args.per_asset_timeout_s,
        heartbeat_s: args.heartbeat_s,
        ..RetryPolicy::default()
    }
    .validate()?;
    let config = BatchConfig {
        retry,
        transfer,
        max_workers: args.max_workers,
        retry_failed: args.retry_failed,
        hls_config_dir: args.hls_config_dir.clone(),
        output_prefix: args.output_prefix.clone(),
        packager_bin: args.packager_bin.clone(),
        ..BatchConfig::new(storage, keys, &args.output, &args.resume_log)
    };
    config.validate()?;
    let report = run_batch(&work, &config, Some(sink), None)?;
    let mut summary = Record::new();
    summary.insert("schema_version".into(), Value::String("1".into()));
    summary.insert("stage".into(), Value::String("batch-report".into()));
    summary.insert("total".into(), Value::from(report.total));
    summary.insert("passed".into(), Value::from(report.passed));
    summary.insert("failed".into(), Value::from(report.failed));
    summary.insert("skipped".into(), Value::from(report.skipped));
    sink.emit(&summary);
    Ok(i32::from(report.failed > 0))
}

/// `{"schema_version": "1", "stage": "batch", "level": "error", "message": "batch-config-invalid"}`.
fn config_invalid() -> Record {
    let mut record = Record::new();
    record.insert("schema_version".into(), Value::String("1".into()));
    record.insert("stage".into(), Value::String("batch".into()));
    record.insert("level".into(), Value::String("error".into()));
    record.insert(
        "message".into(),
        Value::String("batch-config-invalid".into()),
    );
    record
}
