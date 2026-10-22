//! Server configuration, environment variables and child process settings.

use std::ffi::OsString;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

pub type LogSink = Arc<dyn Fn(&str) -> std::io::Result<()> + Send + Sync>;
/// Hooks for embedding and tests: the diagnostic sink and inherited environment.
#[derive(Clone)]
pub struct Hooks {
    pub log: LogSink,
    /// Observe common jobs even when no browser is polling.
    pub monitor: bool,
    pub base_env: crate::jobs::EnvSource,
}
impl Default for Hooks {
    fn default() -> Self {
        Self {
            log: Arc::new(stderr_line),
            monitor: true,
            base_env: crate::jobs::process_env(),
        }
    }
}
fn stderr_line(line: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut stderr = std::io::stderr().lock();
    stderr.write_all(line.as_bytes())?;
    stderr.write_all(b"\n")?;
    stderr.flush()
}
impl fmt::Debug for Hooks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hooks").finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// `DRMR_INPUT_ROOT`.
    pub input_root: PathBuf,
    /// `DRMR_OUTPUT_ROOT`.
    pub output_root: PathBuf,
    /// `DRMR_BIND_HOST` (loopback by default).
    pub bind_host: String,
    /// `DRMR_BIND_PORT`.
    pub bind_port: u16,
    /// Optional private keys/fetch-policy folder, isolated from all HTTP roots.
    pub common_config_root: Option<PathBuf>,
    /// `DRMR_MAX_WORKERS` (`None` selects automatic limits).
    pub max_workers: Option<u32>,
    /// The CLI the jobs run, before the subcommand (default: `drmr` next to this executable).
    pub cli_argv: Vec<OsString>,
    /// Serve the UI from this directory instead of the copy embedded at build time.
    pub static_dir: Option<PathBuf>,
    /// Embedding and test hooks.
    pub hooks: Hooks,
}

impl ServerConfig {
    pub fn new(input_root: impl Into<PathBuf>, output_root: impl Into<PathBuf>) -> Self {
        ServerConfig {
            input_root: input_root.into(),
            output_root: output_root.into(),
            bind_host: "127.0.0.1".to_string(),
            bind_port: 8080,
            common_config_root: None,
            max_workers: None,
            cli_argv: default_cli_argv(),
            static_dir: None,
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

/// The CLI selected by the executable hosting the console.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobCli {
    pub argv: Vec<OsString>,
}
impl JobCli {
    pub fn sibling() -> JobCli {
        JobCli {
            argv: default_cli_argv(),
        }
    }
    pub fn this_executable() -> JobCli {
        JobCli {
            argv: vec![
                std::env::current_exe()
                    .map_or_else(|_| OsString::from("drmr"), PathBuf::into_os_string),
            ],
        }
    }
}
/// Apply explicit static-asset and child-CLI overrides to the environment configuration.
pub fn configure<F>(env: F, args: &ServeArgs, default: JobCli) -> Result<ServerConfig, ConfigError>
where
    F: Fn(&str) -> Option<OsString>,
{
    let mut config = config_from_env(env)?;
    config.static_dir.clone_from(&args.static_dir);
    config.cli_argv = args.cli.as_ref().map_or(default.argv, |program| {
        vec![program.clone().into_os_string()]
    });
    Ok(config)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigError {
    /// The exit code.
    pub exit_code: u8,
    /// The message for stderr.
    pub message: String,
}

/// Read the required input/output roots and optional private configuration, worker and listener settings.
pub fn config_from_env<F>(env: F) -> Result<ServerConfig, ConfigError>
where
    F: Fn(&str) -> Option<OsString>,
{
    let non_empty = |name: &str| env(name).filter(|value| !value.is_empty());
    let (Some(input_root), Some(output_root)) =
        (non_empty("DRMR_INPUT_ROOT"), non_empty("DRMR_OUTPUT_ROOT"))
    else {
        return Err(ConfigError {
            exit_code: 2,
            message: "error: DRMR_INPUT_ROOT and DRMR_OUTPUT_ROOT must both be set".to_string(),
        });
    };
    let mut config = ServerConfig::new(PathBuf::from(input_root), PathBuf::from(output_root));
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
    Ok(config)
}
