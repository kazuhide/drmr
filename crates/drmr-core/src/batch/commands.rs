//! Per-asset CLI subprocesses for batch processing.
//!
//! Runs the executable's own conversion commands and checks their exit codes. Child
//! output is not echoed because it may contain operator paths or source addresses.

use super::{
    AssetRunner, AssetWorkItem, AttemptError, BatchConfig, BatchError, HlsSource, TRANSIENT_SOURCE,
    read_head, read_tail,
};
use crate::batch_support::{is_relative_to, py_bytes_splitlines, resolve};
use crate::config::KeyMaterial;
use crate::hls_cli::private_json;
use serde_json::{Map, Value};
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// One command `_commands` builds: its argv and the subcommand it runs (Python reads it at
/// following the executable and subcommand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliCommand {
    /// The full argv, the executable first.
    pub argv: Vec<OsString>,
    /// `run`, `analyze-hls` or `package-hls`.
    pub subcommand: String,
}

impl CliCommand {
    /// A command with its subcommand.
    pub fn new<I, S>(argv: I, subcommand: &str) -> CliCommand
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        CliCommand {
            argv: argv.into_iter().map(Into::into).collect(),
            subcommand: subcommand.to_string(),
        }
    }
}

/// `_private_config`: an operator-owned mode-0600 JSON object outside the output root; every
/// failure is `hls-config-invalid`.
pub fn private_config(path: &Path, output_root: &Path) -> Result<Map<String, Value>, AttemptError> {
    use std::os::unix::fs::PermissionsExt;
    let invalid = || AttemptError::Batch(BatchError::new("hls-config-invalid"));
    let path = resolve(path);
    if is_relative_to(&path, &resolve(output_root)) {
        return Err(invalid());
    }
    let metadata = std::fs::metadata(&path).map_err(|_| invalid())?;
    if metadata.permissions().mode() & 0o777 != 0o600 {
        return Err(invalid());
    }
    private_json(&path).map_err(|_| invalid())
}

/// Locate the current executable for per-asset commands.
/// Subprocesses use the same CLI flags and exit statuses as direct invocations.
fn cli_base() -> OsString {
    std::env::current_exe().map_or_else(|_| OsString::from("drmr"), Into::into)
}

/// `_commands`: the per-asset commands. `dash-cenc` is one `run`; `hls-clear` is
/// `analyze-hls` then `package-hls` from the asset's private configuration — or `package-hls`
/// alone when a verified snapshot of this asset already exists. An unverified or partial
/// snapshot directory is removed (it never published its integrity record).
pub fn commands(
    work: &AssetWorkItem,
    config: &BatchConfig,
    stage: &Path,
    source: &dyn HlsSource,
) -> Result<Vec<CliCommand>, AttemptError> {
    let base = cli_base();
    if work.kind == "dash-cenc" {
        let manifest = work.dash_manifest.as_ref().ok_or(AttemptError::Other)?;
        let mut argv: Vec<OsString> = vec![base, "run".into(), "--mpd".into()];
        argv.push(stage.join(manifest.key()).into());
        argv.extend(["--asset-id".into(), OsString::from(&work.asset_id)]);
        argv.extend(["--output".into(), config.output_root.clone().into()]);
        argv.extend(["--log-format".into(), "jsonl".into()]);
        match &work.hls_master {
            Some(master) => argv.extend(["--hls-master".into(), stage.join(master.key()).into()]),
            None => argv.push("--no-manifests".into()),
        }
        push_tuning(&mut argv, config);
        return Ok(vec![CliCommand {
            argv,
            subcommand: "run".to_string(),
        }]);
    }
    if work.kind != "hls-clear" {
        return Err(BatchError::new("unsupported-kind").into());
    }
    clear_commands(work, config, base, source)
}

/// `--max-workers` and `--packager-bin` when configured.
fn push_tuning(argv: &mut Vec<OsString>, config: &BatchConfig) {
    if let Some(bound) = config.max_workers {
        argv.extend(["--max-workers".into(), bound.to_string().into()]);
    }
    if let Some(packager) = &config.packager_bin {
        argv.extend(["--packager-bin".into(), packager.clone().into()]);
    }
}

fn clear_commands(
    work: &AssetWorkItem,
    config: &BatchConfig,
    base: OsString,
    source: &dyn HlsSource,
) -> Result<Vec<CliCommand>, AttemptError> {
    let invalid = || AttemptError::Batch(BatchError::new("hls-config-invalid"));
    let directory = resolve(config.hls_config_dir.as_deref().ok_or_else(invalid)?);
    let settings = private_config(
        &directory.join(format!("{}.json", work.asset_id)),
        &config.output_root,
    )?;
    let allowed = ["source_config", "drm_config", "selection"];
    if settings
        .keys()
        .any(|name| !allowed.contains(&name.as_str()))
        || !settings.contains_key("source_config")
        || !settings.contains_key("drm_config")
    {
        return Err(invalid());
    }
    let mut files: Vec<(String, OsString)> = Vec::new();
    for (name, value) in &settings {
        let value = value.as_str().ok_or_else(invalid)?;
        let file = resolve(&directory.join(value));
        private_config(&file, &config.output_root)?;
        files.push((name.clone(), file.into_os_string()));
    }
    let file = |name: &str| -> Option<OsString> {
        files
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, path)| path.clone())
    };
    let mut analyze: Vec<OsString> =
        vec![base.clone(), "analyze-hls".into(), "--source-config".into()];
    analyze.push(file("source_config").ok_or_else(invalid)?);
    analyze.extend(["--asset-id".into(), OsString::from(&work.asset_id)]);
    analyze.extend(["--output".into(), config.output_root.clone().into()]);
    let source_dir = config.output_root.join(&work.asset_id).join("clear/source");
    let analysis_path = source_dir.join("analysis.json");
    let mut package: Vec<OsString> = vec![base, "package-hls".into(), "--analysis".into()];
    package.push(analysis_path.clone().into());
    package.extend([
        "--drm-config".into(),
        file("drm_config").ok_or_else(invalid)?,
    ]);
    if let Some(selection) = file("selection") {
        package.extend(["--selection".into(), selection]);
    }
    push_tuning(&mut package, config);
    let analyze = CliCommand {
        argv: analyze,
        subcommand: "analyze-hls".to_string(),
    };
    let package = CliCommand {
        argv: package,
        subcommand: "package-hls".to_string(),
    };
    let parent_link = source_dir.parent().is_some_and(Path::is_symlink);
    if source_dir.is_symlink() || parent_link || analysis_path.is_symlink() {
        return Err(invalid());
    }
    if analysis_path.exists() {
        match source.read_analysis(&analysis_path) {
            Ok(asset_id) if asset_id == work.asset_id => return Ok(vec![package]),
            // `except (OSError, ValueError)`: any other exception escapes `_commands`.
            Err(AttemptError::Other) => return Err(AttemptError::Other),
            Err(AttemptError::Interrupted(message)) => {
                return Err(AttemptError::Interrupted(message));
            }
            _ => return Err(invalid()),
        }
    }
    if source_dir.exists() {
        // An interrupted analysis never published its final integrity record. Its private
        // owned scratch may be refetched; admitted snapshots are immutable.
        if !source_dir.is_dir() {
            return Err(invalid());
        }
        std::fs::remove_dir_all(&source_dir).map_err(|_| AttemptError::Other)?;
    }
    Ok(vec![analyze, package])
}

/// Whether this process is an asset worker (`_IN_ASSET_WORKER`): its children stay in its
/// process group, which the supervisor kills as a whole, and the supervisor owns the deadline.
pub(crate) static IN_ASSET_WORKER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn in_worker() -> bool {
    IN_ASSET_WORKER.load(std::sync::atomic::Ordering::SeqCst)
}

/// `_run_commands`: run each command with the key material in its environment (`DRM_KID`,
/// `DRM_KEY`, `DRM_IV`; never on its argv), stdout and stderr to private temporary files, and
/// stop at the first non-zero exit. Outside a worker each child gets its own process group and
/// the per-asset deadline (`asset-timeout`, the whole group killed). A failed `analyze-hls`
/// whose closing `analysis` record names a transient source failure is `storage-transient`;
/// a child that printed `timed out` on stderr is `packager-timeout`.
pub fn run_commands(
    commands: &[CliCommand],
    config: &BatchConfig,
    keys: Option<&KeyMaterial>,
    on_heartbeat: &dyn Fn(),
) -> Result<i32, AttemptError> {
    let start = Instant::now();
    let deadline = after(start, config.retry.per_asset_timeout_s);
    let mut next_heartbeat = after(start, config.retry.heartbeat_s);
    for command in commands {
        let mut records = tempfile::tempfile().map_err(|_| AttemptError::Other)?;
        let mut errors = tempfile::tempfile().map_err(|_| AttemptError::Other)?;
        let (program, arguments) = command.argv.split_first().ok_or(AttemptError::Other)?;
        let mut child_command = Command::new(program);
        child_command
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(records.try_clone().map_err(|_| AttemptError::Other)?)
            .stderr(errors.try_clone().map_err(|_| AttemptError::Other)?);
        if let Some(keys) = keys {
            child_command
                .env("DRM_KID", keys.key_id_hex())
                .env("DRM_KEY", keys.key_hex())
                .env("DRM_IV", keys.iv_hex());
        }
        if !in_worker() {
            std::os::unix::process::CommandExt::process_group(&mut child_command, 0);
        }
        let mut child = ChildGuard(Some(
            child_command.spawn().map_err(|_| AttemptError::Other)?,
        ));
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            let now = Instant::now();
            if !in_worker() && now >= deadline {
                child.kill();
                return Err(AttemptError::Retry("asset-timeout".to_string()));
            }
            if now >= next_heartbeat {
                on_heartbeat();
                next_heartbeat = after(Instant::now(), config.retry.heartbeat_s);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(
                remaining.clamp(Duration::from_millis(1), Duration::from_millis(50)),
            );
        };
        let code = exit_code(status);
        if code != 0 {
            if command.subcommand == "analyze-hls" && transient_analysis(&mut records) {
                return Err(AttemptError::Retry("storage-transient".to_string()));
            }
            let head = read_head(&mut errors, 64 * 1024).unwrap_or_default();
            if contains(&head.to_ascii_lowercase(), b"timed out") {
                return Err(AttemptError::Retry("packager-timeout".to_string()));
            }
            return Ok(code);
        }
    }
    Ok(0)
}

/// `start` plus `seconds` (a validated, positive, finite policy value), saturating far in the
/// future: Python's float arithmetic accepts any finite timeout, `Instant + Duration` would
/// overflow on an absurd one.
pub(crate) fn after(start: Instant, seconds: f64) -> Instant {
    Duration::try_from_secs_f64(seconds)
        .ok()
        .and_then(|duration| start.checked_add(duration))
        .unwrap_or_else(|| start + Duration::from_secs(u64::from(u32::MAX)))
}

/// The closing `analysis` record is the child's last line, behind every tool record :
/// the last 64 KiB of its output are read, not the first.
fn transient_analysis(records: &mut std::fs::File) -> bool {
    let tail = read_tail(records, 64 * 1024).unwrap_or_default();
    py_bytes_splitlines(&tail).into_iter().any(|line| {
        let Ok(Value::Object(record)) = serde_json::from_slice::<Value>(line) else {
            return false;
        };
        record.get("stage").and_then(Value::as_str) == Some("analysis")
            && record
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|message| TRANSIENT_SOURCE.contains(&message))
    })
}

/// A running child that is killed (its whole process group outside a worker) and reaped if it
/// is still running when dropped (Python's `finally` around the poll loop).
struct ChildGuard(Option<std::process::Child>);

impl ChildGuard {
    fn try_wait(&mut self) -> Result<Option<ExitStatus>, AttemptError> {
        let Some(child) = self.0.as_mut() else {
            return Err(AttemptError::Other);
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                self.0 = None;
                Ok(Some(status))
            }
            Ok(None) => Ok(None),
            Err(_) => Err(AttemptError::Other),
        }
    }

    fn kill(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        if in_worker() {
            let _ = child.kill();
        } else {
            let _ = rustix::process::kill_process_group(
                rustix::process::Pid::from_child(&child),
                rustix::process::Signal::KILL,
            );
        }
        let _ = child.wait();
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        self.kill();
    }
}

/// The exit code as `Popen.returncode` reports it: the code, or minus the signal number.
pub(crate) fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|number| -number))
        .unwrap_or(-1)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// `_run_asset`: the production per-asset conversion, `_run_commands(_commands(…))`.
#[derive(Clone, Copy)]
pub struct CliRunner<'a> {
    /// The clear-HLS source functions `_commands` uses to verify a reused snapshot.
    pub source: &'a dyn HlsSource,
}

impl std::fmt::Debug for CliRunner<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CliRunner").finish_non_exhaustive()
    }
}

impl AssetRunner for CliRunner<'_> {
    fn run_asset(
        &self,
        work: &AssetWorkItem,
        config: &BatchConfig,
        stage: &Path,
        keys: &KeyMaterial,
        on_heartbeat: &dyn Fn(),
    ) -> Result<i32, AttemptError> {
        let planned = commands(work, config, stage, self.source)?;
        run_commands(&planned, config, Some(keys), on_heartbeat)
    }
}
