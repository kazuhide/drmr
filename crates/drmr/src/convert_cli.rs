//! Generic media commands with protected operator key files and secret-free summaries.

use clap::Args;
use drmr_core::conversion::{
    ConversionError, ConversionReport, ConversionRequest, Converter, VerificationRequest,
    VerificationSource,
    key_file::{self, OperatorKeys},
};
mod feedback;
use drmr_core::inspection::{self, OutputFormat, TargetProtection};
use drmr_core::toolrun::{Secrets, ToolRunner};
use feedback::Progress;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Args)]
pub struct ConvertArgs {
    /// Local source media or a completed clear HLS URL. The source is never overwritten.
    pub input: PathBuf,
    /// New directory retaining the original input; required for URL input.
    #[arg(long)]
    pub download_dir: Option<PathBuf>,
    /// Private JSON settings for additional origins, authentication and fetch limits.
    #[arg(long)]
    pub fetch_policy: Option<PathBuf>,
    /// Target protection: clear, cenc, cbcs, hls-aes128, or hls-sample-aes.
    #[arg(long)]
    pub to: TargetProtection,
    /// Output format: mp4, dash, hls (fragmented MP4), or hls-ts.
    #[arg(long)]
    pub format: Option<OutputFormat>,
    /// A new output file or directory; existing destinations are refused.
    #[arg(long)]
    pub output: PathBuf,
    /// Protected JSON configuration with distinct input and output keys.
    #[arg(long)]
    pub keys_file: Option<PathBuf>,
    /// Shaka Packager executable, selected by the operator.
    #[arg(long, default_value = "packager")]
    pub packager_bin: String,
    /// Upper bound for simultaneous media work (1-64); default adapts to CPU and memory.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=64))]
    pub max_workers: Option<u8>,
    /// Print the versioned result as one JSON object.
    #[arg(long)]
    pub json: bool,
    /// Include numeric measurements in JSON; does not change media verification.
    #[arg(long, requires = "json")]
    pub measure: bool,
}

#[derive(Args)]
pub struct VerifyArgs {
    /// Result media to compare with the source.
    pub output: PathBuf,
    /// Original source media. Required to verify content correspondence.
    #[arg(long)]
    pub source: PathBuf,
    /// Protected JSON configuration: input keys for source, output keys for result.
    #[arg(long)]
    pub keys_file: Option<PathBuf>,
    /// Shaka Packager executable, selected by the operator.
    #[arg(long, default_value = "packager")]
    pub packager_bin: String,
    /// Upper bound for simultaneous media work (1-64); default adapts to CPU and memory.
    #[arg(long, value_parser = clap::value_parser!(u8).range(1..=64))]
    pub max_workers: Option<u8>,
    /// Print the versioned verification result as one JSON object.
    #[arg(long)]
    pub json: bool,
    /// Include numeric measurements in JSON; does not change media verification.
    #[arg(long, requires = "json")]
    pub measure: bool,
}

fn load(path: Option<&PathBuf>) -> Result<OperatorKeys, ConversionError> {
    path.map_or_else(|| Ok(OperatorKeys::default()), |path| key_file::read(path))
}

fn converter(
    packager: &str,
    maximum: Option<u8>,
    progress: Arc<Progress>,
    stop: drmr_core::concurrency::Stop,
) -> Converter {
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), progress.clone()));
    Converter::new(runner, packager, "ffmpeg")
        .with_activity_sink(progress)
        .with_stop(stop)
        .with_max_workers(maximum.map(usize::from))
        .expect("clap validates the worker upper bound")
}

fn error_hint(code: &str) -> Option<&'static str> {
    match code {
        "key-file-invalid" => {
            Some("Use a schema-version-1, owner-only regular JSON file with valid keys.")
        }
        "cancelled" => Some(
            "Check the destination and job status before retrying; cancellation is not a media verification result.",
        ),
        "verification-mismatch" => {
            Some("Review every source/result check. Do not use the result as verified media.")
        }
        "media-tool-unavailable-or-timeout" | "media-tool-timeout" => Some(
            "Check external tool discovery and source compatibility, and review the configured media timeout.",
        ),
        "media-cleanup-unconfirmed" => Some(
            "Cleanup is unconfirmed. Resolve the owned process failure before retrying; no verified result is established.",
        ),
        "download-directory-required" => Some(
            "URL input requires --download-dir pointing to a new directory under an existing parent.",
        ),
        "download-options-require-url" => {
            Some("Use --download-dir and --fetch-policy only with URL input.")
        }
        "download-output-overlap" => {
            Some("Choose separate download and output paths; neither may contain the other.")
        }
        "download-directory-exists" => {
            Some("Choose a new --download-dir, or convert its saved input.m3u8 as local input.")
        }
        "download-destination-unsafe" => Some(
            "Choose new download and output paths whose immediate parent directories already exist.",
        ),
        "fetch-policy-invalid" => {
            Some("Use an owner-only JSON object with supported, correctly typed fetch settings.")
        }
        "download-source-unavailable" => Some(
            "Check network access, allowed origins, authentication, and fetch limits in --fetch-policy.",
        ),
        "download-source-not-supported" => Some(
            "URL input requires completed clear HLS with supported TS or fragmented MP4 media. See docs/conversion-support.md.",
        ),
        "download-source-changed" => Some(
            "The retained source changed or became unavailable. Inspect it and choose a new download directory before retrying the URL.",
        ),
        "download-graph-limit" => Some(
            "The playlist references exceed the bounded URL acquisition profile. See docs/media-processing.md.",
        ),
        "download-cleanup-unconfirmed" => Some(
            "Partial source cleanup could not be confirmed. Inspect the chosen download directory before retrying.",
        ),
        "resource-budget" => Some(
            "Free memory or run fewer jobs; the worker maximum does not override memory admission.",
        ),
        "media-output-limit" => {
            Some("The tool output exceeded the capture limit. Partial evidence is never accepted.")
        }
        "input-key-required" => Some(
            "Provide every source key in input.keys; use input.hls_keys for HLS AES-128 or SAMPLE-AES.",
        ),
        "output-key-required" => Some("Provide output.kid and output.key in --keys-file."),
        "output-key-role-mismatch" => Some(
            "For cenc/cbcs, provide output.kid and output.key; for HLS AES-128, omit the KID and provide output.key plus output.hls_key_uri. For clear output, omit output.",
        ),
        "hls-key-uri-required" => {
            Some("Provide output.hls_key_uri for CBCS HLS playlist signaling.")
        }
        "cenc-hls-output-not-supported" => Some("Use --format dash or --format mp4 for cenc."),
        "output-key-not-used" => {
            Some("For clear output, omit the output section from --keys-file.")
        }
        "target-requires-hls-output" => {
            Some("Use --format hls or --format hls-ts for HLS AES-128.")
        }
        "single-track-mp4-required" | "mp4-output-requires-one-track" => {
            Some("Use DASH or HLS output for separate video and audio tracks.")
        }
        "finite-audio-tail-not-supported" => Some(
            "This source has a finite audio trim that the backend cannot preserve. See docs/media-processing.md.",
        ),
        "output-exists-or-unsafe" | "output-exists-or-publish-failed" => {
            Some("Choose a new output path under an existing directory.")
        }
        "packet-timing-unavailable" => Some(
            "Use an FFmpeg build that supplies complete packet timestamps and durations. See docs/media-processing.md.",
        ),
        _ => None,
    }
}

fn error_value(command: &str, error: ConversionError, progress: &Progress) -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1, "command": command,
        "status": if error.0 == "cancelled" { "cancelled" } else { "failed" },
        "error": {"code": error.0, "hint": error_hint(error.0).unwrap_or("Review the error code and source profile before retrying with a new output destination."), "last_activity": progress.last_activity()}
    })
}
fn error(
    command: &str,
    error: ConversionError,
    as_json: bool,
    progress: &Progress,
    exit_override: Option<u8>,
) -> ExitCode {
    progress.finish();
    eprintln!("{command}: {error}");
    if as_json {
        if json(&error_value(command, error, progress)).is_err() {
            eprintln!("{command}: result-output-unavailable");
            return ExitCode::from(1);
        }
    } else if let Some(hint) = error_hint(error.0) {
        eprintln!("{hint}");
    }
    if error.0 == "cancelled" {
        return ExitCode::from(130);
    }
    ExitCode::from(exit_override.unwrap_or({
        if matches!(
            error.0,
            "key-file-invalid"
                | "download-directory-required"
                | "download-options-require-url"
                | "download-output-overlap"
                | "download-directory-exists"
                | "download-destination-unsafe"
                | "download-url-invalid"
                | "download-url-scheme-not-supported"
                | "fetch-policy-invalid"
                | "input-key-required"
                | "output-key-required"
                | "output-key-not-used"
                | "output-key-role-mismatch"
                | "hls-key-uri-required"
                | "cenc-hls-output-not-supported"
                | "multi-track-mp4-output-not-supported"
                | "target-requires-hls-output"
                | "input-format-not-supported"
                | "single-track-mp4-required"
                | "input-protection-not-supported"
        ) {
            2
        } else {
            1
        }
    }))
}
struct ConfigurationError {
    cause: ConversionError,
    exit: Option<u8>,
}
impl From<ConversionError> for ConfigurationError {
    fn from(cause: ConversionError) -> Self {
        Self { cause, exit: None }
    }
}

fn json(value: &serde_json::Value) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)
}

fn configured_download(
    args: &ConvertArgs,
    input: &crate::download_cli::Input,
) -> Result<Option<drmr_core::source::FetchPolicy>, ConfigurationError> {
    match input {
        crate::download_cli::Input::Local(_) => {
            if args.download_dir.is_some() || args.fetch_policy.is_some() {
                Err(ConversionError("download-options-require-url").into())
            } else {
                Ok(None)
            }
        }
        crate::download_cli::Input::Url(url) => {
            let directory = args
                .download_dir
                .as_ref()
                .ok_or(ConversionError("download-directory-required"))?;
            crate::download_cli::preflight(directory, &args.output).map_err(|cause| {
                ConfigurationError {
                    cause,
                    exit: Some(2),
                }
            })?;
            drmr_core::conversion::download_policy::read(url, args.fetch_policy.as_deref())
                .map(Some)
                .map_err(Into::into)
        }
    }
}

fn execute_conversion(
    command: &Converter,
    args: &ConvertArgs,
    input: &crate::download_cli::Input,
    fetch_policy: Option<&drmr_core::source::FetchPolicy>,
    local_format: Option<OutputFormat>,
    keys: &OperatorKeys,
    progress: &Progress,
) -> Result<(ConversionReport, Option<serde_json::Value>), ConversionError> {
    let downloaded = match input {
        crate::download_cli::Input::Url(url) => {
            progress.mark("acquisition");
            let source = command.download_hls(
                url,
                args.download_dir
                    .as_deref()
                    .expect("URL download directory was checked"),
                &args.output,
                fetch_policy.expect("URL fetch policy was checked"),
            )?;
            eprintln!("Downloaded source retained in the requested download directory.");
            Some(source)
        }
        crate::download_cli::Input::Local(_) => None,
    };
    let source = downloaded
        .as_ref()
        .map_or(args.input.as_path(), |source| source.entrypoint());
    let format = if let Some(format) = local_format {
        format
    } else {
        progress.mark("inspection");
        inspection::inspect(source)
            .map_err(|_| ConversionError("invalid-input-media"))?
            .format
            .into()
    };
    progress.mark("input-preparation");
    let request = ConversionRequest {
        input: source,
        output: &args.output,
        format,
        target: args.to,
        input_keys: &keys.input,
        hls_input_keys: &keys.hls_input,
        output_key: keys.output.as_ref(),
        hls_key_uri: keys.output.as_ref().and_then(|_| keys.hls_output_uri()),
        hls_output_key: keys.hls_output.as_ref(),
    };
    let report = match &downloaded {
        Some(source) => command.convert_downloaded(source, request),
        None => command.convert(request),
    }?;
    let download = downloaded.as_ref().map(|source| serde_json::json!({
        "directory": source.entrypoint().parent().expect("downloaded entrypoint has a parent").to_string_lossy(),
        "entrypoint": source.entrypoint().to_string_lossy(),
        "entrypoint_relative": "input.m3u8",
        "retained": true
    }));
    Ok((report, download))
}

fn conversion_text(report: &ConversionReport) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "Converted: {} -> {}",
        report.input.protection.as_str(),
        report.output.protection.as_str()
    )
    .and_then(|()| {
        if let Some(processing) = &report.processing {
            writeln!(out, "Processing route: {}", processing.route.as_str())?;
            writeln!(
                out,
                "Input preparation: {}",
                processing.input_preparation.description()
            )?;
            writeln!(out, "Reason: {}", processing.explanation)?;
            let tools = processing
                .required_tools
                .iter()
                .map(|tool| tool.as_str())
                .collect::<Vec<_>>();
            writeln!(out, "Required tools: {}", tools.join(", "))?;
        }
        Ok(())
    })
    .and_then(|()| {
        writeln!(
            out,
            "Verified stream metadata, packet payloads, decoded content, and packet timing."
        )
    })
}

pub fn convert(args: &ConvertArgs) -> ExitCode {
    let progress = Arc::new(Progress::new(!args.json));
    progress.mark("configuration");
    let input = match crate::download_cli::Input::parse(args.input.as_os_str()) {
        Ok(input) => input,
        Err(cause) => return error("convert", cause, args.json, &progress, None),
    };
    let fetch_policy = match configured_download(args, &input) {
        Ok(policy) => policy,
        Err(cause) => return error("convert", cause.cause, args.json, &progress, cause.exit),
    };
    let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
        return error(
            "convert",
            ConversionError("signal-monitor-unavailable"),
            args.json,
            &progress,
            None,
        );
    };
    let keys = match load(args.keys_file.as_ref()) {
        Ok(keys) => keys,
        Err(cause) => return error("convert", cause, args.json, &progress, None),
    };
    let local_format = match (&input, args.format) {
        (_, Some(format)) => Some(format),
        (crate::download_cli::Input::Local(path), None) => {
            progress.mark("inspection");
            match inspection::inspect(path) {
                Ok(report) => Some(report.format.into()),
                Err(_) => {
                    return error(
                        "convert",
                        ConversionError("invalid-input-media"),
                        args.json,
                        &progress,
                        None,
                    );
                }
            }
        }
        (crate::download_cli::Input::Url(_), None) => None,
    };
    let converter = converter(
        &args.packager_bin,
        args.max_workers,
        progress.clone(),
        cancellation.stop.clone(),
    );
    let execute = |command: &Converter| {
        execute_conversion(
            command,
            args,
            &input,
            fetch_policy.as_ref(),
            local_format,
            &keys,
            &progress,
        )
    };
    let (result, measurement) = if args.measure {
        let (result, measurement) = converter.measure(execute);
        (result, Some(measurement))
    } else {
        (execute(&converter), None)
    };
    let (report, download) = match result {
        Ok(report) => report,
        Err(cause) => return error("convert", cause, args.json, &progress, None),
    };
    if cancellation.stop.is_set() {
        return error(
            "convert",
            ConversionError("cancelled"),
            args.json,
            &progress,
            None,
        );
    }
    progress.finish();
    let result = if args.json {
        let mut value = serde_json::to_value(&report).expect("conversion report serializes");
        if let Some(download) = download {
            value["download"] = download;
        }
        if let Some(measurement) = measurement {
            value["measurements"] =
                serde_json::to_value(measurement).expect("measurement serializes");
        }
        json(&value)
    } else {
        conversion_text(&report)
    };
    if result.is_err() {
        eprintln!("convert: result-output-unavailable");
    }
    ExitCode::from(u8::from(result.is_err()))
}

pub fn verify(args: &VerifyArgs) -> ExitCode {
    let progress = Arc::new(Progress::new(!args.json));
    progress.mark("configuration");
    let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
        return error(
            "verify",
            ConversionError("signal-monitor-unavailable"),
            args.json,
            &progress,
            None,
        );
    };
    let keys = match load(args.keys_file.as_ref()) {
        Ok(keys) => keys,
        Err(cause) => return error("verify", cause, args.json, &progress, None),
    };
    let output_keys = match keys.output_verification_keys() {
        Ok(keys) => keys,
        Err(cause) => return error("verify", cause, args.json, &progress, None),
    };
    let output_hls_keys = match keys.output_hls_verification_keys() {
        Ok(keys) => keys,
        Err(cause) => return error("verify", cause, args.json, &progress, None),
    };
    progress.mark("input-preparation");
    let converter = converter(
        &args.packager_bin,
        args.max_workers,
        progress.clone(),
        cancellation.stop.clone(),
    );
    let execute = |command: &Converter| {
        command.verify(VerificationRequest {
            source: VerificationSource {
                input: &args.source,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output: VerificationSource {
                input: &args.output,
                input_keys: &output_keys,
                hls_keys: &output_hls_keys,
            },
        })
    };
    let (result, measurement) = if args.measure {
        let (result, measurement) = converter.measure(execute);
        (result, Some(measurement))
    } else {
        (execute(&converter), None)
    };
    let report = match result {
        Ok(report) => report,
        Err(cause) => return error("verify", cause, args.json, &progress, None),
    };
    if cancellation.stop.is_set() {
        return error(
            "verify",
            ConversionError("cancelled"),
            args.json,
            &progress,
            None,
        );
    }
    progress.finish();
    if !report.passed {
        eprintln!("verify: verification-mismatch");
    }
    let result = if args.json {
        let mut value = serde_json::to_value(&report).expect("verification report serializes");
        if !report.passed {
            let failure = error_value(
                "verify",
                ConversionError("verification-mismatch"),
                &progress,
            );
            for key in ["command", "status", "error"] {
                value[key] = failure[key].clone();
            }
        }
        if let Some(measurement) = measurement {
            value["measurements"] =
                serde_json::to_value(measurement).expect("measurement serializes");
        }
        json(&value)
    } else {
        writeln!(
            std::io::stdout().lock(),
            "Verification: {}",
            if report.passed { "passed" } else { "failed" }
        )
    };
    if result.is_err() {
        eprintln!("verify: result-output-unavailable");
    }
    ExitCode::from(u8::from(result.is_err() || !report.passed))
}
