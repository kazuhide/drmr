//! Key-free metadata analysis or complete source admission with retained remote input.
use crate::convert_cli::{self, Progress};
use clap::Args;
use drmr_core::conversion::{
    ConversionError, Converter, SourceCompatibilityNote, VerificationSource,
    key_file::OperatorKeys, plan::InputAnalysis,
};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Args)]
#[command(
    after_help = "Use --metadata-only to read local or HTTP(S) MP4, DASH, or HLS metadata without keys or media tools.\nMetadata-only URL input fetches bounded metadata, does not retain a source, and does not verify media. Use --fetch-policy for allowed origins, authentication, and limits; it applies only to URL input.\nDo not combine --metadata-only with --download-dir, --keys-file, --packager-bin, --max-workers, --decode, or --measure.\n\nComplete analysis checks local MP4, DASH, or HLS media before choosing a conversion.\nURL input requires a new --download-dir. Supported profiles: completed HLS; single-Period static DASH using SegmentTemplate with a finite SegmentTimeline; self-contained MP4.\nMP4 and DASH support clear, cenc, and cbcs within the ordinary input profile. Ciphertext and encryption declarations are retained; key and license URIs are never fetched.\nSupply decryption keys separately with --keys-file. Relative playlist key URIs resolve against the effective remote playlist URL.\nThe retained entrypoint is input.m3u8, input.mpd, or input.mp4; --json reports the actual local path.\nThe saved original is reused offline by plan, convert, and verify; keys and request headers stay separate.\nInput admission does not establish a converted result, DRM licensing, or device playback.\n\nExamples:\n  drmr analyze media.mp4 --json\n  drmr analyze https://example.com/vod/master.m3u8 --download-dir source --json\n  drmr analyze https://example.com/vod/manifest.mpd --download-dir dash-source --json\n  drmr analyze https://example.com/media.mp4 --download-dir mp4-source --json\n  drmr plan dash-source/input.mpd --to cbcs --format dash --output converted --save conversion.plan.json"
)]
pub struct AnalyzeArgs {
    /// Local MP4, DASH, or HLS source, or an HTTP(S) URL to analyze.
    pub input: PathBuf,
    /// Read metadata without keys, media tools, complete media checks, or retained URL input.
    #[arg(long, conflicts_with_all = ["download_dir", "keys_file", "packager_bin", "max_workers", "decode", "measure"])]
    pub metadata_only: bool,
    /// New private original directory; required for complete URL analysis. Source directories are retained.
    #[arg(long)]
    pub download_dir: Option<PathBuf>,
    /// Private settings for allowed origins, authentication, and bounded retrieval.
    #[arg(long)]
    pub fetch_policy: Option<PathBuf>,
    /// Protected schema-version-1 or 2 JSON input keys; output domains are not selected.
    #[arg(long)]
    pub keys_file: Option<PathBuf>,
    /// Shaka Packager executable, selected by the operator.
    #[arg(long, default_value = "packager")]
    pub packager_bin: String,
    /// Upper bound for simultaneous media work (1-64); default adapts to CPU and memory.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=64))]
    pub max_workers: Option<u8>,
    #[command(flatten)]
    pub decoding: convert_cli::DecodeOptions,
    /// Print one versioned metadata or complete input-analysis report.
    #[arg(long)]
    pub json: bool,
    /// Include numeric measurements in JSON; does not change input checks.
    #[arg(long, requires = "json")]
    pub measure: bool,
}

fn fetch_settings(
    args: &AnalyzeArgs,
    input: &crate::download_cli::Input,
) -> Result<Option<drmr_core::source::FetchPolicy>, ConversionError> {
    match input {
        crate::download_cli::Input::Local(_) => {
            if args.download_dir.is_some() || args.fetch_policy.is_some() {
                Err(ConversionError("download-options-require-url"))
            } else {
                Ok(None)
            }
        }
        crate::download_cli::Input::Url(url) => {
            let directory = args
                .download_dir
                .as_deref()
                .ok_or(ConversionError("download-directory-required"))?;
            crate::download_cli::preflight_source(directory)?;
            drmr_core::conversion::download_policy::read(url, args.fetch_policy.as_deref())
                .map(Some)
        }
    }
}

fn text_report(report: &InputAnalysis) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(out, "Source checks: passed.")?;
    writeln!(out, "Actual format: {}", report.actual_format.as_str())?;
    let mut protection: Vec<_> = report
        .track_protection
        .iter()
        .map(|value| value.as_str())
        .collect();
    protection.sort_unstable();
    protection.dedup();
    writeln!(out, "Actual protection: {}", protection.join(" / "))?;
    writeln!(out, "Checked tracks: {}", report.input.tracks.len())?;
    let names: Vec<_> = report
        .eligible_formats
        .iter()
        .map(|format| format.as_str())
        .collect();
    writeln!(out, "Candidate output formats: {}", names.join(", "))?;
    for &note in &report.compatibility_notes {
        writeln!(out, "Source compatibility: {}.", note.as_str())?;
        writeln!(out, "  {}", compatibility_explanation(note))?;
    }
    writeln!(
        out,
        "Checked complete encoded packets, codec configuration, playback windows, and timestamps."
    )?;
    writeln!(
        out,
        "{}",
        convert_cli::decode_summary(report.verification.decode_evidence)
    )?;
    writeln!(
        out,
        "Create a saved plan to validate the requested output scheme and format."
    )
}

fn compatibility_explanation(note: SourceCompatibilityNote) -> &'static str {
    match note {
        SourceCompatibilityNote::TsEmptyPayloadPadding => {
            "Verified empty TS padding is omitted from the private demux input; originals and media/timing checks are retained."
        }
        SourceCompatibilityNote::HlsEmptyCuePair => {
            "Adjacent zero-duration HLS cue markers have no active interval; the original playlist is retained."
        }
    }
}

fn execute_analysis(
    command: &Converter,
    args: &AnalyzeArgs,
    input: &crate::download_cli::Input,
    policy: Option<&drmr_core::source::FetchPolicy>,
    keys: &OperatorKeys,
    progress: &Progress,
) -> Result<(InputAnalysis, Option<serde_json::Value>), ConversionError> {
    let downloaded = match input {
        crate::download_cli::Input::Local(_) => None,
        crate::download_cli::Input::Url(url) => {
            progress.mark("acquisition");
            Some(
                command.acquire_source(
                    url,
                    args.download_dir
                        .as_deref()
                        .expect("URL destination was checked"),
                    policy.expect("URL fetch settings were checked"),
                )?,
            )
        }
    };
    let bound = downloaded
        .as_ref()
        .map(|source| command.using_downloaded_source(source))
        .transpose()?;
    let command = bound.as_ref().unwrap_or(command);
    let source = downloaded
        .as_ref()
        .map_or_else(|| args.input.as_path(), |source| source.entrypoint());
    progress.mark("input-preparation");
    let report = command.analyze_source(VerificationSource {
        input: source,
        input_keys: &keys.input,
        hls_keys: &keys.hls_input,
    })?;
    let download = downloaded.as_ref().map(|source| {
        serde_json::json!({
            "directory": source.directory().to_string_lossy(),
            "entrypoint": source.entrypoint().to_string_lossy(),
            "entrypoint_relative": source.entrypoint_relative().to_string_lossy(),
            "retained": true,
        })
    });
    Ok((report, download))
}

fn metadata_analysis(
    args: &AnalyzeArgs,
    input: &crate::download_cli::Input,
    progress: &Arc<Progress>,
) -> ExitCode {
    let policy = match input {
        crate::download_cli::Input::Local(_) if args.fetch_policy.is_some() => {
            return convert_cli::error(
                "analyze",
                ConversionError("download-options-require-url"),
                args.json,
                progress,
                Some(2),
            );
        }
        crate::download_cli::Input::Local(_) => None,
        crate::download_cli::Input::Url(url) => {
            match drmr_core::conversion::download_policy::read(url, args.fetch_policy.as_deref()) {
                Ok(policy) => Some(policy),
                Err(cause) => {
                    return convert_cli::error("analyze", cause, args.json, progress, None);
                }
            }
        }
    };
    let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
        return convert_cli::error(
            "analyze",
            ConversionError("signal-monitor-unavailable"),
            args.json,
            progress,
            None,
        );
    };
    let result = match input {
        crate::download_cli::Input::Local(path) => {
            progress.mark("inspection");
            drmr_core::inspection::inspect(path).map_err(|cause| ConversionError(cause.0))
        }
        crate::download_cli::Input::Url(url) => {
            progress.mark("acquisition");
            convert_cli::converter(
                "packager",
                None,
                false,
                Arc::clone(progress),
                cancellation.stop.clone(),
            )
            .inspect_url_metadata(url, policy.as_ref().expect("URL policy was checked"))
        }
    };
    if cancellation.stop.is_set() {
        return convert_cli::error(
            "analyze",
            ConversionError("cancelled"),
            args.json,
            progress,
            None,
        );
    }
    let report = match result {
        Ok(report) => report,
        Err(cause) => {
            let exit = matches!(input, crate::download_cli::Input::Local(_)).then_some(2);
            return convert_cli::error("analyze", cause, args.json, progress, exit);
        }
    };
    progress.mark("publication");
    progress.begin_result();
    let written = if args.json {
        convert_cli::json(&serde_json::to_value(&report).expect("metadata report serializes"))
    } else {
        crate::inspect_cli::summary(&report, &mut std::io::stdout().lock())
    };
    if written.is_err() {
        progress.diagnostic("analyze", "result-output-unavailable");
    } else {
        progress.finish();
    }
    ExitCode::from(u8::from(written.is_err()))
}

pub fn analyze(args: &AnalyzeArgs) -> ExitCode {
    let progress = Arc::new(Progress::new(!args.json));
    progress.mark("configuration");
    let input = match crate::download_cli::Input::parse(args.input.as_os_str()) {
        Ok(value) => value,
        Err(cause) => return convert_cli::error("analyze", cause, args.json, &progress, None),
    };
    if args.metadata_only {
        return metadata_analysis(args, &input, &progress);
    }
    let policy = match fetch_settings(args, &input) {
        Ok(value) => value,
        Err(cause) => return convert_cli::error("analyze", cause, args.json, &progress, None),
    };
    let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
        return convert_cli::error(
            "analyze",
            ConversionError("signal-monitor-unavailable"),
            args.json,
            &progress,
            None,
        );
    };
    let keys = match convert_cli::load_source(args.keys_file.as_ref()) {
        Ok(value) => value,
        Err(cause) => return convert_cli::error("analyze", cause, args.json, &progress, None),
    };
    let converter = convert_cli::converter(
        &args.packager_bin,
        args.max_workers,
        args.decoding.decode,
        progress.clone(),
        cancellation.stop.clone(),
    )
    .with_new_output_defaults();
    let execute = |command: &Converter| {
        execute_analysis(command, args, &input, policy.as_ref(), &keys, &progress)
    };
    let (result, measurement) = if args.measure {
        let (result, measurement) = converter.measure(execute);
        (result, Some(measurement))
    } else {
        (execute(&converter), None)
    };
    let (report, download) = match result {
        Ok(value) => value,
        Err(cause) => return convert_cli::error("analyze", cause, args.json, &progress, None),
    };
    if cancellation.stop.is_set() {
        return convert_cli::error(
            "analyze",
            ConversionError("cancelled"),
            args.json,
            &progress,
            None,
        );
    }
    progress.mark("publication");
    progress.begin_result();
    let result = if args.json {
        let mut value = serde_json::to_value(&report).expect("source analysis serializes");
        if let Some(download) = download {
            value["download"] = download;
        }
        if let Some(measurement) = measurement {
            value["measurements"] =
                serde_json::to_value(measurement).expect("measurement serializes");
        }
        convert_cli::json(&value)
    } else {
        text_report(&report)
    };
    if result.is_err() {
        progress.diagnostic("analyze", "result-output-unavailable");
    } else {
        progress.finish();
    }
    ExitCode::from(u8::from(result.is_err()))
}
