//! Server configuration, environment variables and child process settings.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub type LogSink = Arc<dyn Fn(&str) -> std::io::Result<()> + Send + Sync>;
/// A clock in seconds (`time.monotonic` or `time.time`).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

/// How the conversion queue runs its head (`ConversionQueue._runner` / `_thread_factory`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueRunner {
    /// A runner thread launches the queue head whenever the lease is free (the default).
    Thread,
    /// No runner: the head is launched only by an explicit `tick()` (`_runner = False`).
    Disabled,
    /// A runner slot is taken and released as with `Thread`, but nothing runs on its own: the
    /// caller drives `run_once()` (the tests' inert thread).
    Inert,
    /// Starting a runner fails (the tests' broken thread factory).
    FailToStart,
}

/// Hooks for embedding and tests: where the diagnostic lines go, the job monitor, the queue
/// runner, the clocks and the log-tail poll interval. The defaults are the production
/// behaviour.
#[derive(Clone)]
pub struct Hooks {
    /// The diagnostic sink (stderr by default).
    pub log: LogSink,
    /// Whether launched jobs are watched by a monitor thread (`HlsService.monitor`).
    pub monitor: bool,
    /// The queue runner mode.
    pub queue_runner: QueueRunner,
    /// `HlsService.clock` (`time.monotonic` by default).
    pub hls_clock: Option<Clock>,
    /// `ConversionQueue.clock` (`time.time` by default).
    pub queue_clock: Option<Clock>,
    /// `tail_log`'s poll interval (0.1 s).
    pub tail_poll: Duration,
    /// The environment the jobs inherit (`os.environ`; this process's by default).
    pub base_env: crate::jobs::EnvSource,
}

impl Default for Hooks {
    fn default() -> Self {
        Hooks {
            log: Arc::new(stderr_line),
            monitor: true,
            queue_runner: QueueRunner::Thread,
            hls_clock: None,
            queue_clock: None,
            tail_poll: Duration::from_millis(100),
            base_env: crate::jobs::process_env(),
        }
    }
}

/// The default [`LogSink`]: the line and a newline on stderr, flushed; a write error (EPIPE
/// included) is returned, never a panic.
fn stderr_line(line: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut stderr = std::io::stderr().lock();
    stderr.write_all(line.as_bytes())?;
    stderr.write_all(b"\n")?;
    stderr.flush()
}

impl fmt::Debug for Hooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hooks")
            .field("monitor", &self.monitor)
            .field("queue_runner", &self.queue_runner)
            .field("tail_poll", &self.tail_poll)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// `DRMR_CATALOG_ROOT`.
    pub catalog_root: PathBuf,
    /// `DRMR_OUTPUT_ROOT`.
    pub output_root: PathBuf,
    /// `DRMR_BIND_HOST` (loopback by default).
    pub bind_host: String,
    /// `DRMR_BIND_PORT`.
    pub bind_port: u16,
    /// Optional private keys/fetch-policy folder, isolated from all HTTP roots.
    pub common_config_root: Option<PathBuf>,
    /// `DRMR_HLS_PROFILES`.
    pub hls_profiles: Option<PathBuf>,
    /// `DRMR_MAX_WORKERS` (`package-hls --max-workers`; `None` = the CPU default).
    pub max_workers: Option<u32>,
    /// The CLI the jobs run, before the subcommand (default: `drmr` next to this executable).
    pub cli_argv: Vec<OsString>,
    /// Serve the UI from this directory instead of the copy embedded at build time.
    pub static_dir: Option<PathBuf>,
    /// Whether the installed job CLI supports batch processing.
    pub batch_available: bool,
    /// Embedding and test hooks.
    pub hooks: Hooks,
}

impl ServerConfig {
    pub fn new(catalog_root: impl Into<PathBuf>, output_root: impl Into<PathBuf>) -> Self {
        ServerConfig {
            catalog_root: catalog_root.into(),
            output_root: output_root.into(),
            bind_host: "127.0.0.1".to_string(),
            bind_port: 8080,
            hls_profiles: None,
            common_config_root: None,
            max_workers: None,
            cli_argv: default_cli_argv(),
            static_dir: None,
            batch_available: false,
            hooks: Hooks::default(),
        }
    }
}

/// The `drmr` binary next to the running executable (`drmr-server` and `drmr` are built into
/// the same directory).
pub fn default_cli_argv() -> Vec<OsString> {
    let program = std::env::current_exe().map_or_else(
        |_| PathBuf::from("drmr"),
        |path| path.with_file_name(format!("drmr{}", std::env::consts::EXE_SUFFIX)),
    );
    vec![program.into_os_string()]
}

#[derive(Clone, Debug, Default, clap::Args)]
pub struct ServeArgs {
    /// Serve the UI from this directory instead of the copy embedded at build time.
    #[arg(long)]
    pub static_dir: Option<PathBuf>,
    /// The CLI the jobs run (default: the `drmr` executable — `drmr serve` itself, or the one
    /// next to `drmr-server`).
    #[arg(long)]
    pub cli: Option<PathBuf>,
}

/// The CLI the jobs run when `--cli` is not given, as the executable hosting the server knows
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobCli {
    /// The argv before the subcommand (`drmr run …`, `drmr analyze-hls …`).
    pub argv: Vec<OsString>,
    /// Whether that CLI carries `batch` (`GET /api/platform`'s `batch_available`).
    pub has_batch: bool,
}

impl JobCli {
    /// The `drmr` next to the running executable ([`default_cli_argv`], `drmr-server`'s
    /// default): a separate program this one cannot vouch for, so `has_batch` is false.
    pub fn sibling() -> JobCli {
        JobCli {
            argv: default_cli_argv(),
            has_batch: false,
        }
    }

    /// This very executable (`drmr serve`), which knows whether it carries `batch`. When its
    /// path cannot be read, `drmr` from the `PATH`, unvouched.
    pub fn this_executable(has_batch: bool) -> JobCli {
        match std::env::current_exe() {
            Ok(path) => JobCli {
                argv: vec![path.into_os_string()],
                has_batch,
            },
            Err(_) => JobCli {
                argv: vec![OsString::from("drmr")],
                has_batch: false,
            },
        }
    }
}

/// `_config_from_env()` with the shared options applied: `--static-dir`, and the CLI the jobs
/// run — `--cli` when given (it carries `batch` only when it is the default CLI's own file),
/// otherwise `default`.
pub fn configure<F>(env: F, args: &ServeArgs, default: JobCli) -> Result<ServerConfig, ConfigError>
where
    F: Fn(&str) -> Option<OsString>,
{
    let mut config = config_from_env(env)?;
    config.static_dir.clone_from(&args.static_dir);
    let jobs = match &args.cli {
        None => default,
        Some(program) => JobCli {
            has_batch: default.has_batch && same_file(program, &default.argv),
            argv: vec![program.clone().into_os_string()],
        },
    };
    config.cli_argv = jobs.argv;
    config.batch_available = jobs.has_batch;
    Ok(config)
}

/// Whether `program` is the one-element `argv`'s program, file for file.
fn same_file(program: &Path, argv: &[OsString]) -> bool {
    let [default] = argv else {
        return false;
    };
    match (
        std::fs::canonicalize(program),
        std::fs::canonicalize(default),
    ) {
        (Ok(given), Ok(default)) => given == default,
        _ => false,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    /// The exit code.
    pub exit_code: u8,
    /// The message for stderr.
    pub message: String,
}

/// `_config_from_env()`: the roots (both required), `DRMR_MAX_WORKERS` (ASCII digits,
/// 1–64), `DRMR_BIND_HOST`, `DRMR_BIND_PORT`, `DRMR_HLS_PROFILES`
/// (unset or empty: none).
pub fn config_from_env<F>(env: F) -> Result<ServerConfig, ConfigError>
where
    F: Fn(&str) -> Option<OsString>,
{
    let non_empty = |name: &str| env(name).filter(|value| !value.is_empty());
    let (Some(catalog_root), Some(output_root)) = (
        non_empty("DRMR_CATALOG_ROOT"),
        non_empty("DRMR_OUTPUT_ROOT"),
    ) else {
        return Err(ConfigError {
            exit_code: 2,
            message: "error: DRMR_CATALOG_ROOT and DRMR_OUTPUT_ROOT must both be set".to_string(),
        });
    };
    let mut config = ServerConfig::new(PathBuf::from(catalog_root), PathBuf::from(output_root));
    if let Some(raw) = non_empty("DRMR_MAX_WORKERS") {
        let text = raw.to_string_lossy();
        let digits = text.bytes().all(|byte| byte.is_ascii_digit()) && raw.to_str().is_some();
        // `int(workers_raw)` of a digit string: leading zeros are fine, the value decides.
        let value = digits
            .then(|| text.trim_start_matches('0'))
            .and_then(|trimmed| {
                if trimmed.len() > 2 {
                    None
                } else {
                    trimmed.parse::<u32>().ok().or(Some(0))
                }
            });
        match value {
            Some(workers @ 1..=64) => config.max_workers = Some(workers),
            _ => {
                return Err(ConfigError {
                    exit_code: 2,
                    message: "error: DRMR_MAX_WORKERS must be an integer from 1 to 64".to_string(),
                });
            }
        }
    }
    if let Some(host) = env("DRMR_BIND_HOST") {
        config.bind_host = host.to_string_lossy().into_owned();
    }
    if let Some(port) = env("DRMR_BIND_PORT") {
        let text = port.to_string_lossy();
        let parsed = crate::pyutil::python_int_text(&text).ok_or_else(|| ConfigError {
            exit_code: 1,
            message: format!(
                "ValueError: invalid literal for int() with base 10: {}",
                crate::pyutil::repr_str(&text)
            ),
        })?;
        config.bind_port = u16::try_from(parsed).map_err(|_| ConfigError {
            exit_code: 1,
            message: "OverflowError: bind(): port must be 0-65535.".to_string(),
        })?;
    }
    if let Some(root) = non_empty("DRMR_COMMON_CONFIG_ROOT") {
        config.common_config_root = Some(PathBuf::from(root));
    }
    if let Some(profiles) = non_empty("DRMR_HLS_PROFILES") {
        config.hls_profiles = Some(PathBuf::from(profiles));
    }
    Ok(config)
}

pub fn shown(path: &Path) -> String {
    crate::pyutil::path_text(path)
}
