//! Command-line media inspection, conversion, verification, and a local web console.
//! Exit codes: 0 success, 1 the check failed, 2 usage or an input that is not what the
//! command expects.

mod analyze_cli;
mod bundle_cli;
mod cancellation;
mod common_job;
mod conversion_summary;
mod convert_cli;
mod doctor;
mod download_cli;
mod inspect_cli;
mod output_cli;
mod plan_cli;
mod progress;

use clap::{Parser, Subcommand};
use drmr_server::{JobCli, ServeArgs};
use std::path::Path;
use std::process::ExitCode;

const EXIT_INVALID: u8 = 2;

#[derive(Parser)]
#[command(
    name = "drmr",
    version,
    about = "Inspect and repackage streaming media with digital rights management (DRM)",
    after_help = "Getting started:\n  drmr doctor                       Check external media tools\n  drmr analyze media.mp4 --metadata-only  Read metadata without keys or media tools\n  drmr analyze media.mp4             Check the complete source media\n  drmr plan media.mp4 --to cbcs       Describe conversion intent\n  drmr convert input.mpd --to clear --output clear-media\n  drmr verify clear-media/manifest.mpd --source input.mpd\n\nSaved-plan workflow:\n  drmr analyze URL --download-dir SOURCE_DIR\n  drmr plan SOURCE_DIR/input.m3u8 --to cbcs --format dash --output RESULT_DIR --save PLAN.json\n  drmr convert --plan PLAN.json --keys-file KEYS.json\n  drmr verify --plan PLAN.json --keys-file KEYS.json\n\nRetained entrypoints are SOURCE_DIR/input.m3u8 (HLS), input.mpd (DASH), or input.mp4 (MP4).\nFor URL metadata without retaining a source, use 'drmr analyze URL --metadata-only'.\nSupported URL input also works directly with 'drmr convert URL --download-dir SOURCE_DIR'.\nDASH and HLS describe streaming media using manifests or playlists.\nUse 'drmr help <COMMAND>' for detailed command help."
)]
struct Cli {
    /// Progress on stderr: auto shows text interactively and keeps redirected JSON quiet.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    progress: progress::Mode,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the version.
    Version,
    /// Discover external tools or probe their versions and advertised options.
    #[command(
        after_help = "Discovery checks executable files on the current launch PATH without running them.\n--probe executes bounded version/help checks for required tools; it never proves media compatibility.\nThe default checks native conversion requirements. Add --decode to require FFmpeg and ffprobe for additional complete decoding. Native analysis, saved planning and verification need no external media tools.\n\nExamples:\n  drmr doctor\n  drmr doctor --probe --route progressive-mp4 --json\n  drmr doctor --probe --decode --route progressive-mp4 --json\n\nExit codes: 0 selected checks passed for required tools; 1 a required check failed; 2 invalid arguments."
    )]
    Doctor {
        /// Print a machine-readable JSON report instead of a readable checklist.
        #[arg(long)]
        json: bool,
        /// Execute bounded version and advertised-option checks (three seconds per call).
        #[arg(long)]
        probe: bool,
        /// Include dependencies for additional complete decoding.
        #[arg(long)]
        decode: bool,
        /// Check a route's native tools, adding decoder tools with --decode; other tools are optional.
        #[arg(long, default_value = "all", value_parser = ["all", "progressive-mp4", "fragmented-mp4", "dash-presentation", "fmp4-hls-presentation", "mpeg-ts-hls", "mpeg-ts-dash", "mpeg-ts-sample-aes-hls", "mpeg-ts-hls-ts", "fmp4-hls-ts", "fmp4-segments", "mpeg-ts-segments"])]
        route: String,
    },
    /// Read metadata or check complete MP4, DASH, or HLS input media.
    Analyze(analyze_cli::AnalyzeArgs),
    /// Convert media with independent input/output keys and round-trip checks.
    Convert(convert_cli::ConvertArgs),
    /// Compare result media with its original source, including every packet timestamp.
    Verify(convert_cli::VerifyArgs),
    /// Describe conversion intent or save an execution plan.
    #[command(
        after_help = "With INPUT: describe a requested conversion without executing it.\nWith --save FILE and --output PATH: validate the complete source and save its fingerprints and output intent. Use --outputs FILE for several results with separate DRM and manifest-check policies. Keys remain separate.\n\nExamples:\n  drmr plan source/input.m3u8 --outputs outputs.json --output converted --save conversion.plan.json\n  drmr convert --plan conversion.plan.json --keys-file keys.json\n  drmr verify --plan conversion.plan.json --keys-file keys.json"
    )]
    Plan(inspect_cli::PlanArgs),
    /// Start the local web console, configured using DRMR_* environment variables.
    Serve(ServeArgs),
}

fn main() -> ExitCode {
    if drmr_core::native::initialize_supervised_cli().is_err() {
        eprintln!("drmr: the supervised job context could not be initialized");
        return ExitCode::from(EXIT_INVALID);
    }
    let supervised = drmr_core::native::is_supervised_cli();
    if supervised
        && std::env::args_os()
            .nth(2)
            .is_none_or(|command| command != drmr_server::common_protocol::JOB_ARGUMENT)
    {
        eprintln!("drmr: this command is unavailable in a supervised console job");
        return ExitCode::from(EXIT_INVALID);
    }
    let job_arguments: Vec<_> = std::env::args_os()
        .skip(if supervised { 2 } else { 1 })
        .collect();
    if job_arguments
        .first()
        .is_some_and(|value| value == drmr_server::common_protocol::JOB_ARGUMENT)
    {
        return if let [_, path] = job_arguments.as_slice() {
            common_job::run(Path::new(path))
        } else {
            eprintln!("drmr: invalid common job request");
            ExitCode::from(EXIT_INVALID)
        };
    }
    let cli = Cli::parse();
    let command = cli.command;
    let command_name = match &command {
        Command::Version => "version",
        Command::Doctor { .. } => "doctor",
        Command::Analyze(_) => "analyze",
        Command::Convert(_) => "convert",
        Command::Verify(_) => "verify",
        Command::Plan(_) => "plan",
        Command::Serve(_) => "serve",
    };
    progress::configure(cli.progress, command_name);
    match command {
        Command::Version => {
            println!("drmr {}", drmr_core::VERSION);
            ExitCode::SUCCESS
        }
        Command::Doctor {
            json,
            probe,
            decode,
            route,
        } => doctor::run(json, probe, decode, &route),
        Command::Analyze(args) => analyze_cli::analyze(&args),
        Command::Convert(args) => convert_cli::convert(&args),
        Command::Verify(args) => convert_cli::verify(&args),
        Command::Plan(args) => inspect_cli::plan(&args),
        Command::Serve(args) => serve(&args),
    }
}

/// Start the local console with this executable for its shared CLI operations.
fn serve(args: &ServeArgs) -> ExitCode {
    let progress = progress::Progress::new(true);
    progress.mark("server-startup");
    if progress.server_handoff().is_err() {
        progress.diagnostic("serve", "result-output-unavailable");
        return ExitCode::from(1);
    }
    let code = drmr_server::run_server_with_ready(
        args,
        JobCli::this_executable(),
        "drmr serve",
        |address| {
            progress.mark("serving");
            progress.server_address(address);
        },
    );
    if code == ExitCode::SUCCESS {
        progress.finish();
    } else {
        progress.fail("server-stopped-with-error");
    }
    code
}
