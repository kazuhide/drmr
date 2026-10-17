//! Command-line media inspection, packaging, batch processing, and a local web console.
//! Exit codes: 0 success, 1 the check failed, 2 usage or an input that is not what the
//! command expects.

mod convert_cli;
mod doctor;
mod inspect_cli;

use clap::{Parser, Subcommand};
use drmr_core::batch_cli::{self, BatchArgs};
use drmr_core::canonical::Dumps;
use drmr_core::cli;
use drmr_core::events::JsonLinesWriter;
use drmr_core::hls_cli;
use drmr_core::pipeline_deps::Ports;
use drmr_core::snapshot::{self, SnapshotError, SnapshotReport};
use drmr_core::toolrun::Secrets;
use drmr_server::{JobCli, ServeArgs};
use serde_json::{Map, Value};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

const EXIT_CHANGED: u8 = 1;
const EXIT_INVALID: u8 = 2;

#[derive(Parser)]
#[command(
    name = "drmr",
    version,
    about = "Inspect and repackage streaming media with digital rights management (DRM)",
    after_help = "Getting started:\n  drmr doctor                       Check external media tools\n  drmr inspect media.mp4             Read local media metadata without keys\n  drmr plan media.mp4 --to cbcs       Describe conversion intent\n  drmr plan --mpd manifest.mpd       List prepared DASH renditions\n  drmr run --help                    See existing DASH packaging options\n\nDASH and HLS are streaming formats that describe media using manifests or playlists.\nUse 'drmr help <COMMAND>' for detailed command help."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the version.
    Version,
    /// Check whether the external media tools are available.
    #[command(
        after_help = "Checks executable files on PATH without running them or reading configuration.\nThis checks availability only; it does not verify versions or media compatibility.\n\nExamples:\n  drmr doctor\n  drmr doctor --json\n\nExit codes: 0 all required tools found; 1 one or more tools missing."
    )]
    Doctor {
        /// Print a machine-readable JSON report instead of a readable checklist.
        #[arg(long)]
        json: bool,
    },
    /// Verify a saved HLS snapshot by comparing its recorded content digest.
    #[command(
        after_help = "A snapshot stores downloaded playlists, media segments, and analysis.json.\nA digest is a content fingerprint used to detect changes.\n\nExamples:\n  drmr verify-snapshot output/sample/clear/source\n  drmr verify-snapshot --files output/sample/clear/source\n\nExit codes: 0 verified; 1 changed; 2 invalid snapshot."
    )]
    VerifySnapshot {
        /// The snapshot directory (`output/<id>/clear/source/`) or its `analysis.json`.
        path: PathBuf,
        /// Also re-hash every recorded playlist and segment file.
        #[arg(long)]
        files: bool,
    },
    /// Print JSON with stable formatting and key order for content fingerprints.
    #[command(after_help = "Example:\n  drmr canonical analysis.json")]
    Canonical {
        /// Any JSON file.
        path: PathBuf,
    },
    /// Repackage the supported downloaded DASH catalog layout.
    #[command(about = cli::RUN_ABOUT, args_override_self = true, infer_long_args = true)]
    Run(cli::RunArgs),
    /// Inspect local MP4, DASH, or HLS metadata without keys or output files.
    Inspect(inspect_cli::InspectArgs),
    /// Convert media with independent input/output keys and round-trip checks.
    Convert(convert_cli::ConvertArgs),
    /// Compare result media with its original source, including every packet timestamp.
    Verify(convert_cli::VerifyArgs),
    /// Describe conversion intent, or list catalog renditions with --mpd.
    #[command(
        args_override_self = true,
        infer_long_args = true,
        after_help = "With INPUT: describe a requested conversion without executing it.\nWith --mpd: enumerate renditions only; writes nothing (read-only)."
    )]
    Plan(inspect_cli::PlanArgs),
    /// Repackage multiple assets sequentially, with support for restarting.
    Batch(BatchArgs),
    /// Start the local web console, configured using DRMR_* environment variables.
    Serve(ServeArgs),
    #[command(
        name = "analyze-hls",
        about = hls_cli::ANALYZE_ABOUT,
        args_override_self = true,
        infer_long_args = true
    )]
    AnalyzeHls(hls_cli::AnalyzeArgs),
    #[command(
        name = "package-hls",
        about = hls_cli::PACKAGE_ABOUT,
        args_override_self = true,
        infer_long_args = true
    )]
    PackageHls(hls_cli::PackageArgs),
}

fn main() -> ExitCode {
    // The asset worker of `batch`: this executable, invoked with exactly the worker
    // argument, reads its private configuration from stdin and never parses a command line.
    if invoked_as_worker() {
        return process_exit(drmr_core::batch::worker_main());
    }
    match Cli::parse().command {
        Command::Version => {
            println!("drmr {}", drmr_core::VERSION);
            ExitCode::SUCCESS
        }
        Command::Doctor { json } => doctor::run(json),
        Command::VerifySnapshot { path, files } => verify(&path, files),
        Command::Canonical { path } => canonical(&path),
        Command::Run(args) => repackage(&cli::Commands::Run(args)),
        Command::Inspect(args) => inspect_cli::inspect(&args),
        Command::Convert(args) => convert_cli::convert(&args),
        Command::Verify(args) => convert_cli::verify(&args),
        Command::Plan(args) => match &args.mpd {
            Some(mpd) => repackage(&cli::Commands::Plan(cli::PlanArgs {
                mpd: mpd.clone(),
                asset_id: args.asset_id.clone(),
            })),
            None => inspect_cli::plan(&args),
        },
        Command::Batch(args) => batch(&args),
        Command::Serve(args) => serve(&args),
        Command::AnalyzeHls(args) => {
            ExitCode::from(hls_cli::analyze_command(&args, std::io::stdout()))
        }
        Command::PackageHls(args) => package_hls(&args),
    }
}

/// `package-hls` with the process environment: the key material is read from `DRM_KID` /
/// `DRM_KEY` / `DRM_IV` there, never from a flag.
fn package_hls(args: &hls_cli::PackageArgs) -> ExitCode {
    let env = |name: &str| std::env::var_os(name).map(|value| value.to_string_lossy().into_owned());
    ExitCode::from(hls_cli::package_command(args, &env, std::io::stdout()))
}

/// `serve`: `drmr-server` inside this executable, with this executable as the jobs' CLI; that
/// CLI carries `batch`, which `GET /api/platform` reports as available.
fn serve(args: &ServeArgs) -> ExitCode {
    drmr_server::run_server(args, JobCli::this_executable(true), "drmr serve")
}

/// The worker protocol accepts exactly `[WORKER_ARG]`.
fn invoked_as_worker() -> bool {
    let mut args = std::env::args_os().skip(1);
    matches!(
        (args.next(), args.next()),
        (Some(first), None) if first == drmr_core::batch::WORKER_ARG
    )
}

/// A worker exit status as a process exit code (the codes the tool uses are 0..=130).
fn process_exit(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// `batch`: the records go to stdout as JSON Lines (one JSON object per line); the keys
/// of the fleet never reach this process's records (the workers hold them), so the writer
/// scrubs nothing.
fn batch(args: &BatchArgs) -> ExitCode {
    let writer = JsonLinesWriter::new(std::io::stdout(), Arc::new(Secrets::new()));
    let code = batch_cli::cmd_batch(args, &writer);
    if writer.failed() {
        return ExitCode::from(1);
    }
    process_exit(code)
}

/// `run` / `plan` with the process environment (keys come from `DRM_KID` / `DRM_KEY` /
/// `DRM_IV` there, never from a flag), over the real `config` / `packager` / `media_inspect` /
/// `manifest_dash` + `manifest_hls` / `playready` ports.
fn repackage(command: &cli::Commands) -> ExitCode {
    let modules = cli::Modules {
        config: &Ports,
        packager: &Ports,
        media_inspect: &Ports,
        manifests: &Ports,
        playready: &Ports,
    };
    let env: cli::Env = std::env::vars_os()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect();
    let code = cli::dispatch(
        command,
        &env,
        &modules,
        std::io::stdout(),
        &mut std::io::stderr(),
    );
    ExitCode::from(code)
}

fn verify(path: &Path, files: bool) -> ExitCode {
    let result = if files {
        snapshot::verify_files(path)
    } else {
        snapshot::verify_record(path)
    };
    match result {
        Ok(report) => {
            println!("{}", Dumps::DEFAULT.dumps(&report_value(path, &report)));
            ExitCode::SUCCESS
        }
        Err(error) => {
            let mut record = Map::new();
            record.insert("path".into(), Value::String(path.display().to_string()));
            record.insert("status".into(), Value::String(error.token().to_string()));
            println!("{}", Dumps::DEFAULT.dumps(&Value::Object(record)));
            eprintln!("verify-snapshot: {error}");
            ExitCode::from(match error {
                SnapshotError::Changed => EXIT_CHANGED,
                SnapshotError::Invalid => EXIT_INVALID,
            })
        }
    }
}

fn report_value(path: &Path, report: &SnapshotReport) -> Value {
    let mut record = Map::new();
    record.insert("path".into(), Value::String(path.display().to_string()));
    record.insert("status".into(), Value::String("ok".to_string()));
    if let Value::Object(fields) = serde_json::to_value(report).expect("report serializes") {
        record.extend(fields);
    }
    Value::Object(record)
}

fn canonical(path: &Path) -> ExitCode {
    let parsed = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let Some(value) = parsed else {
        eprintln!("canonical: not a JSON file: {}", path.display());
        return ExitCode::from(EXIT_INVALID);
    };
    let mut stdout = std::io::stdout().lock();
    if stdout
        .write_all(Dumps::CANONICAL.dumps(&value).as_bytes())
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return ExitCode::from(EXIT_INVALID);
    }
    ExitCode::SUCCESS
}
