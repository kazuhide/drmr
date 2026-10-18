//! Catalog conversion commands and read-only rendition planning.
//!
//! Keys and IVs come from environment variables or an operator configuration file.
//! Exit codes: 0 success, 1 failed checks, 2 invalid usage or configuration.
//! JSON Lines output redacts key material from every tool and pipeline record.

use crate::checks::CheckId;
use crate::config::{Asset, KeyMaterial};
use crate::events::{JsonLinesWriter, NoSink, Sink};
use crate::pipeline::{self, AssetOptions, AssetResult, PipelineDeps, py_str};
use crate::pipeline_deps::{Config, Manifests, MediaInspect, Packager, PlayReady, PyError};
use crate::toolrun::{Secrets, ToolRunner};
use clap::{Args, Parser, Subcommand, ValueEnum};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Every check passed (renditions and manifests).
pub const EXIT_ALL_PASS: u8 = 0;
/// At least one check failed.
pub const EXIT_RENDITION_FAIL: u8 = 1;
/// A usage or configuration error (argparse's own exit code).
pub const EXIT_USAGE_ERROR: u8 = 2;

/// The `run` subcommand's help line.
pub const RUN_ABOUT: &str = "repackage every in-scope rendition of one asset";
/// The `plan` subcommand's help line.
pub const PLAN_ABOUT: &str = "enumerate renditions only; writes nothing (read-only)";

/// The process environment as `load_key_material` reads it.
pub type Env = HashMap<String, String>;

/// `--log-format` (`choices=("text", "jsonl")`).
///
/// Here and in [`RunArgs`] / [`PlanArgs`] the members carry plain comments: clap prints a doc
/// comment as help text, and the help texts are written for CLI users.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum LogFormat {
    // The human summary.
    Text,
    // One JSON-Lines record per line on stdout.
    Jsonl,
}

/// Arguments for repackaging a catalog asset.
#[derive(Args, Clone, Debug)]
pub struct RunArgs {
    // `--mpd`.
    #[arg(long, help = "path to the source DASH manifest")]
    pub mpd: PathBuf,
    // `--output`.
    #[arg(long, help = "output root (output/<asset_id>/ is created under it)")]
    pub output: PathBuf,
    // `--asset-id`.
    #[arg(
        long = "asset-id",
        help = "Name the output asset (default: the source title directory name)"
    )]
    pub asset_id: Option<String>,
    // `--packager-bin`.
    #[arg(
        long = "packager-bin",
        help = "Shaka Packager binary/path override (default: `packager` on PATH)"
    )]
    pub packager_bin: Option<PathBuf>,
    // `--max-workers`.
    #[arg(
        long = "max-workers",
        allow_negative_numbers = true,
        help = "maximum rendition workers for this asset (default: CPU bound)"
    )]
    pub max_workers: Option<i64>,
    // `--no-round-trip`.
    #[arg(
        long = "no-round-trip",
        help = "skip the 6-4 framemd5 round-trip check"
    )]
    pub no_round_trip: bool,
    // `--limit-renditions`.
    #[arg(
        long = "limit-renditions",
        value_name = "N",
        allow_negative_numbers = true,
        help = "Process at most N renditions for debugging; the result is incomplete"
    )]
    pub limit_renditions: Option<i64>,
    // `--hls-master`.
    #[arg(
        long = "hls-master",
        help = "Source HLS master playlist (required unless --no-manifests); \
                HLS means HTTP Live Streaming"
    )]
    pub hls_master: Option<PathBuf>,
    // `--no-manifests`.
    #[arg(
        long = "no-manifests",
        help = "Skip playlist and manifest generation for debugging; the result is incomplete"
    )]
    pub no_manifests: bool,
    // `--manifest-local-url-base`.
    #[arg(
        long = "manifest-local-url-base",
        help = "Also write manifests for a local HTTP server using this base URL"
    )]
    pub manifest_local_url_base: Option<String>,
    // `--log-format`.
    #[arg(
        long = "log-format",
        value_enum,
        default_value = "text",
        help = "Output a readable summary (text) or one JSON object per line (jsonl)"
    )]
    pub log_format: LogFormat,
}

/// `drmr plan`'s arguments.
#[derive(Args, Clone, Debug)]
pub struct PlanArgs {
    // `--mpd`.
    #[arg(
        long,
        help = "Source DASH manifest (.mpd), which lists media renditions"
    )]
    pub mpd: PathBuf,
    // `--asset-id`.
    #[arg(long = "asset-id", help = "Override the asset name shown in the plan")]
    pub asset_id: Option<String>,
}

/// The `run` and `plan` subcommands; the `drmr` binary flattens them into its own. argparse
/// takes an option's last occurrence and unique prefixes of option names, so these do too.
#[derive(Subcommand, Clone, Debug)]
pub enum Commands {
    // `run`.
    #[command(about = RUN_ABOUT, args_override_self = true, infer_long_args = true)]
    Run(RunArgs),
    // `plan`.
    #[command(about = PLAN_ABOUT, args_override_self = true, infer_long_args = true)]
    Plan(PlanArgs),
}

/// `drmr` with the subcommands this module ports (for [`main`]).
#[derive(Parser, Debug)]
#[command(name = "drmr", disable_help_subcommand = true)]
struct DrmRepackage {
    #[command(subcommand)]
    command: Commands,
}

/// The modules `run` and `plan` call into.
#[derive(Clone, Copy)]
pub struct Modules<'a> {
    /// `config`.
    pub config: &'a dyn Config,
    /// `packager`.
    pub packager: &'a dyn Packager,
    /// `media_inspect`.
    pub media_inspect: &'a dyn MediaInspect,
    /// `manifest_dash` / `manifest_hls`.
    pub manifests: &'a dyn Manifests,
    /// `playready`.
    pub playready: &'a dyn PlayReady,
}

/// Why a command did not return an exit code of its own.
#[derive(Debug)]
pub enum CliError {
    /// `_UsageError`: exit 2.
    Usage(String),
    /// An exception from the modules: `ValueError` and `PackagerError` exit 2, the rest 1.
    Python(PyError),
    /// stdout could not be written (Python's `print` raises `BrokenPipeError`): exit 1.
    Output,
}

impl From<PyError> for CliError {
    fn from(error: PyError) -> Self {
        CliError::Python(error)
    }
}

impl From<io::Error> for CliError {
    fn from(_: io::Error) -> Self {
        CliError::Output
    }
}

fn usage(message: impl Into<String>) -> CliError {
    CliError::Usage(message.into())
}

/// `main(argv)` for `run` and `plan`: parse like argparse (usage errors exit 2, `--help` 0),
/// then [`dispatch`].
pub fn main<O: Write + Send + 'static>(
    argv: &[String],
    env: &Env,
    modules: &Modules<'_>,
    mut stdout: O,
    stderr: &mut dyn Write,
) -> u8 {
    let arguments = std::iter::once("drmr".to_string()).chain(argv.iter().cloned());
    match DrmRepackage::try_parse_from(arguments) {
        Ok(parsed) => dispatch(&parsed.command, env, modules, stdout, stderr),
        Err(error) => {
            let text = error.render().to_string();
            // A help or version request prints on stdout; an error on stderr.
            let written = if error.use_stderr() {
                stderr.write_all(text.as_bytes())
            } else {
                stdout.write_all(text.as_bytes())
            };
            if written.is_err() {
                return EXIT_RENDITION_FAIL;
            }
            u8::try_from(error.exit_code()).unwrap_or(EXIT_USAGE_ERROR)
        }
    }
}

/// Run a parsed `run` / `plan` and map its outcome to the exit code (`main`'s `except`
/// clauses): `_UsageError`, `ValueError` and `PackagerError` print `error: <message>` and exit
/// 2; anything else is uncaught in Python (exit 1).
pub fn dispatch<O: Write + Send + 'static>(
    command: &Commands,
    env: &Env,
    modules: &Modules<'_>,
    mut stdout: O,
    stderr: &mut dyn Write,
) -> u8 {
    let outcome = match command {
        Commands::Run(args) => cmd_run(args, env, modules, stdout, stderr),
        Commands::Plan(args) => cmd_plan(args, modules, &mut stdout),
    };
    let (code, line) = match outcome {
        Ok(code) => return code,
        Err(CliError::Usage(message)) => (EXIT_USAGE_ERROR, format!("error: {message}")),
        Err(CliError::Python(error @ (PyError::Value(_) | PyError::Packager(_)))) => {
            (EXIT_USAGE_ERROR, format!("error: {error}"))
        }
        Err(CliError::Python(error)) => (EXIT_RENDITION_FAIL, format!("{}: {error}", error.kind())),
        Err(CliError::Output) => (
            EXIT_RENDITION_FAIL,
            "BrokenPipeError: stdout could not be written".to_string(),
        ),
    };
    // stderr itself failing leaves nothing else to report to.
    let _ = writeln!(stderr, "{line}");
    code
}

/// `_widevine_pssh_by_rendition`: each rendition's copied Widevine box, when it has one.
fn widevine_pssh_by_rendition(asset: &Asset) -> BTreeMap<String, Vec<String>> {
    asset
        .renditions
        .iter()
        .filter_map(|rendition| {
            rendition
                .widevine_pssh_hex
                .as_ref()
                .filter(|pssh| !pssh.is_empty())
                .map(|pssh| (rendition.rep_id.clone(), vec![pssh.clone()]))
        })
        .collect()
}

/// `_resolve_packager_bin_on_path`: the `PATH` of the job's tools. Python prepends the
/// directory (the path itself, or a file's parent) to `os.environ["PATH"]`, so the bare
/// `packager` the pipeline runs resolves there first — and so does every other tool the job
/// runs (`ffmpeg`, `ffprobe`, `mp4dump`), a Python behaviour kept as it is. A file must be
/// named `packager`. A Rust process does not change its own environment: the returned `PATH`
/// is the job's tool runner's ([`ToolRunner::with_path`]), so the records show the bare name
/// Python's show.
fn resolve_packager_bin_on_path(packager_bin: &Path, env: &Env) -> Result<String, CliError> {
    #[cfg(unix)]
    let expected_name = "packager";
    #[cfg(windows)]
    let expected_name = "packager.exe";
    let correct_name = packager_bin.file_name().is_some_and(|name| {
        #[cfg(unix)]
        {
            name == expected_name
        }
        #[cfg(windows)]
        {
            name.to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(expected_name))
        }
    });
    if packager_bin.is_file() && !correct_name {
        return Err(usage(format!(
            "--packager-bin must be a directory containing '{expected_name}', or a file named \
             '{expected_name}' (got {})",
            py_str(packager_bin)
        )));
    }
    let target_dir = if packager_bin.is_dir() {
        packager_bin
    } else {
        packager_bin.parent().unwrap_or(packager_bin)
    };
    #[cfg(unix)]
    {
        Ok(format!(
            "{}:{}",
            py_str(target_dir),
            env.get("PATH").map_or("", String::as_str)
        ))
    }
    #[cfg(windows)]
    {
        let entries = std::iter::once(target_dir.to_path_buf())
            .chain(env.get("PATH").into_iter().flat_map(std::env::split_paths));
        std::env::join_paths(entries)
            .map_err(|_| usage("--packager-bin contains an invalid PATH entry"))?
            .into_string()
            .map_err(|_| usage("--packager-bin requires a Unicode path"))
    }
}

/// `cmd_run`: validate the arguments (in Python's order), load the key and the asset, build the
/// `PlayReady` objects before encryption, run the asset with the job's tool
/// runner (the key and the IV registered for scrubbing), then print the summary in text mode.
pub fn cmd_run<O: Write + Send + 'static>(
    args: &RunArgs,
    env: &Env,
    modules: &Modules<'_>,
    stdout: O,
    stderr: &mut dyn Write,
) -> Result<u8, CliError> {
    if !args.mpd.is_file() {
        return Err(usage(format!("--mpd not found: {}", py_str(&args.mpd))));
    }
    if args.limit_renditions.is_some_and(|limit| limit < 1) {
        return Err(usage("--limit-renditions must be >= 1"));
    }
    if args.max_workers.is_some_and(|workers| workers < 1) {
        return Err(usage("--max-workers must be >= 1"));
    }
    let tool_path = match &args.packager_bin {
        Some(packager_bin) => Some(resolve_packager_bin_on_path(packager_bin, env)?),
        None => None,
    };
    let emit_manifests = !args.no_manifests;
    if emit_manifests && args.hls_master.is_none() {
        return Err(usage(
            "--hls-master is required unless --no-manifests is given",
        ));
    }
    if let Some(hls_master) = &args.hls_master
        && !hls_master.is_file()
    {
        return Err(usage(format!(
            "--hls-master not found: {}",
            py_str(hls_master)
        )));
    }

    let key = modules.config.load_key_material(env)?;
    let mut asset =
        modules
            .config
            .load_asset(&args.mpd, key, &args.output, args.asset_id.as_deref())?;
    let limited = args.limit_renditions.is_some();
    if let Some(limit) = args.limit_renditions {
        asset
            .renditions
            .truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    }
    // The single PlayReady source, built before encryption: its pssh joins every rendition's
    // --pssh set as well as the manifest patch.
    let playready_objects = if emit_manifests {
        let parsed_pro = modules.manifests.parse_source_playready(&args.mpd)?;
        if parsed_pro.has_custom_attributes {
            return Err(usage("unsupported PlayReady custom attributes or DS_ID"));
        }
        Some(
            modules
                .playready
                .build_playready_v43(asset.key.key_id_hex(), parsed_pro.la_url.as_deref())?,
        )
    } else {
        None
    };

    let pssh = widevine_pssh_by_rendition(&asset);
    let options = AssetOptions {
        pssh_hex_by_rendition: Some(&pssh),
        verify_round_trip: !args.no_round_trip,
        full_enumeration: !limited && emit_manifests,
        hls_master_path: args.hls_master.as_deref().filter(|_| emit_manifests),
        playready_objects: playready_objects.as_ref(),
        manifest_local_url_base: args.manifest_local_url_base.as_deref(),
        on_log: None,
        max_workers: args.max_workers,
    };
    // The job's key and IV are scrubbed from every record in every encoding.
    let secrets = Arc::new(Secrets::with([asset.key.key_hex(), asset.key.iv_hex()]));
    let job_runner = |sink: Arc<dyn Sink>| {
        let runner = ToolRunner::new(secrets.clone(), sink);
        Arc::new(match &tool_path {
            Some(path) => runner.with_path(path),
            None => runner,
        })
    };
    match args.log_format {
        LogFormat::Jsonl => {
            // One writer for the pipeline records and the tool records: one lock, one line.
            let writer = Arc::new(JsonLinesWriter::new(stdout, secrets.clone()));
            let runner = job_runner(writer.clone());
            let deps = pipeline_deps(modules, &runner);
            let options = AssetOptions {
                on_log: Some(writer.as_ref() as &dyn Sink),
                ..options
            };
            let result = pipeline::repackage_asset(&asset, &options, &deps);
            if writer.failed() {
                return Err(CliError::Output);
            }
            let result = result?;
            complete_receipt(env, &asset, &result)?;
            Ok(exit_code(&result))
        }
        LogFormat::Text => {
            let runner = job_runner(Arc::new(NoSink));
            let deps = pipeline_deps(modules, &runner);
            let result = pipeline::repackage_asset(&asset, &options, &deps)?;
            print_summary(&result, stdout, stderr)?;
            complete_receipt(env, &asset, &result)?;
            Ok(exit_code(&result))
        }
    }
}

fn complete_receipt(env: &Env, asset: &Asset, result: &AssetResult) -> Result<(), CliError> {
    crate::catalog_receipt::complete(
        env.get(crate::catalog_receipt::REQUEST_ENV)
            .map(String::as_str),
        result,
        &asset.output_dir,
    )
    .map_err(|_| PyError::Runtime("catalog-receipt-unavailable".into()))?;
    Ok(())
}

/// The pipeline's modules and the job's runner. The pipeline runs the bare `packager`, as
/// Python's does: `--packager-bin` reaches it through the runner's `PATH`.
fn pipeline_deps<'a>(modules: &Modules<'a>, runner: &'a Arc<ToolRunner>) -> PipelineDeps<'a> {
    PipelineDeps {
        packager: modules.packager,
        media_inspect: modules.media_inspect,
        manifests: modules.manifests,
        runner,
        packager_bin: "packager",
    }
}

fn exit_code(result: &AssetResult) -> u8 {
    if result.all_pass {
        EXIT_ALL_PASS
    } else {
        EXIT_RENDITION_FAIL
    }
}

/// `cmd_plan`: enumerate the renditions and print them; read-only, no key material (an
/// all-zero placeholder that no subprocess ever sees).
pub fn cmd_plan(
    args: &PlanArgs,
    modules: &Modules<'_>,
    stdout: &mut dyn Write,
) -> Result<u8, CliError> {
    if !args.mpd.is_file() {
        return Err(usage(format!("--mpd not found: {}", py_str(&args.mpd))));
    }
    let zeros = "0".repeat(32);
    let placeholder_key = KeyMaterial::new(zeros.as_str(), zeros.as_str(), zeros.as_str())
        .expect("32 zeros are 16 bytes of hex");
    let asset = modules.config.load_asset(
        &args.mpd,
        placeholder_key,
        Path::new("."),
        args.asset_id.as_deref(),
    )?;
    writeln!(stdout, "asset_id: {}", asset.asset_id)?;
    writeln!(stdout, "renditions: {}", asset.renditions.len())?;
    for rendition in &asset.renditions {
        writeln!(
            stdout,
            "  {}  {:<5}  {:<20}  {} segments",
            rendition.rep_id,
            rendition.content_type,
            rendition.codec,
            rendition.segment_paths.len()
        )?;
    }
    stdout.flush()?;
    Ok(EXIT_ALL_PASS)
}

/// `_print_summary`: the asset verdict, the partial-run warning on stderr, each rendition's
/// failed check ids (sorted), and the manifests' verdict.
pub fn print_summary(
    result: &AssetResult,
    mut stdout: impl Write,
    stderr: &mut dyn Write,
) -> io::Result<()> {
    let status = if result.all_pass { "PASS" } else { "FAIL" };
    writeln!(
        stdout,
        "asset {}: {status} ({} renditions)",
        result.asset_id,
        result.renditions.len()
    )?;
    if !result.full_enumeration {
        writeln!(
            stderr,
            "WARNING: partial run (--limit-renditions or --no-manifests) -- not a valid \
             completion-gate result"
        )?;
    }
    let marker = |checks: &[(CheckId, String)]| {
        let mut failed: Vec<&str> = checks
            .iter()
            .filter(|(_, status)| status.starts_with("FAIL"))
            .map(|(id, _)| id.as_str())
            .collect();
        failed.sort_unstable();
        if failed.is_empty() {
            "PASS".to_string()
        } else {
            format!("FAIL ({})", failed.join(", "))
        }
    };
    for rendition in &result.renditions {
        writeln!(
            stdout,
            "  {}: {}",
            rendition.rendition_id,
            marker(&rendition.checks)
        )?;
    }
    if let Some(manifests) = &result.manifests {
        writeln!(stdout, "  manifests: {}", marker(&manifests.checks))?;
    }
    stdout.flush()
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::{Env, resolve_packager_bin_on_path};

    #[test]
    fn packager_override_keeps_native_path_entries_and_accepts_exe() {
        let temp = tempfile::tempdir().unwrap();
        let directory = temp.path().join("tools 日本語");
        std::fs::create_dir(&directory).unwrap();
        let executable = directory.join("packager.exe");
        std::fs::write(&executable, b"fixture never executed").unwrap();
        let env = Env::from([("PATH".into(), r"C:\first;D:\second path".into())]);
        let actual = resolve_packager_bin_on_path(&executable, &env).unwrap();
        let entries = std::env::split_paths(&actual).collect::<Vec<_>>();
        assert_eq!(
            entries,
            [directory, r"C:\first".into(), r"D:\second path".into()]
        );
    }

    #[test]
    fn packager_override_refuses_a_different_executable_name() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("other.exe");
        std::fs::write(&path, b"fixture never executed").unwrap();
        assert!(resolve_packager_bin_on_path(&path, &Env::new()).is_err());
    }
}
