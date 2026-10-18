//! Generic media commands with protected operator key files and secret-free summaries.

use clap::Args;
use drmr_core::conversion::{
    ConversionError, ConversionReport, ConversionRequest, Converter, VerificationRequest,
    VerificationSource,
    key_file::{self, OperatorKeys},
};
use drmr_core::events::{Record, Sink};
use drmr_core::inspection::{self, OutputFormat, TargetProtection};
use drmr_core::toolrun::{Secrets, ToolRunner};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

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

fn load(path: Option<&PathBuf>) -> Result<OperatorKeys, ExitCode> {
    path.map_or_else(
        || Ok(OperatorKeys::default()),
        |path| {
            key_file::read(path).map_err(|error| {
                eprintln!("keys: {error}. Use a schema-version-1, owner-only regular JSON file with valid keys.");
                ExitCode::from(2)
            })
        },
    )
}

struct Progress {
    enabled: bool,
    workers: Mutex<Option<u64>>,
}
impl Sink for Progress {
    fn emit(&self, record: &Record) {
        if !self.enabled
            || record.get("stage").and_then(serde_json::Value::as_str) != Some("resources")
        {
            return;
        }
        let Some(workers) = record.get("workers").and_then(serde_json::Value::as_u64) else {
            return;
        };
        let mut previous = self
            .workers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *previous != Some(workers) {
            eprintln!("Using up to {workers} workers (CPU and memory limits).");
            *previous = Some(workers);
        }
    }
}
fn converter(
    packager: &str,
    maximum: Option<u8>,
    progress: bool,
    stop: drmr_core::concurrency::Stop,
) -> Converter {
    let sink = Arc::new(Progress {
        enabled: progress,
        workers: Mutex::new(None),
    });
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), sink));
    Converter::new(runner, packager, "ffmpeg")
        .with_stop(stop)
        .with_max_workers(maximum.map(usize::from))
        .expect("clap validates the worker upper bound")
}

fn error(command: &str, error: ConversionError) -> ExitCode {
    eprintln!("{command}: {error}");
    if error.0 == "cancelled" {
        return ExitCode::from(130);
    }
    if let Some(hint) = match error.0 {
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
        "resource-budget" => Some(
            "Free memory or run fewer jobs; the worker maximum does not override memory admission.",
        ),
        "media-output-limit" => {
            Some("The tool output exceeded the capture limit. Partial evidence is never accepted.")
        }
        "input-key-required" => {
            Some("Provide every source key in input.keys; use input.hls_keys for HLS AES-128.")
        }
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
        "target-requires-hls-output" => Some("Use --format hls for HLS AES-128."),
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
    } {
        eprintln!("{hint}");
    }
    ExitCode::from(
        if matches!(
            error.0,
            "download-directory-required"
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
        },
    )
}

fn json(value: &serde_json::Value) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value)?;
    writeln!(out)
}

fn configured_download(
    args: &ConvertArgs,
    input: &crate::download_cli::Input,
) -> Result<Option<drmr_core::source::FetchPolicy>, ExitCode> {
    match input {
        crate::download_cli::Input::Local(_) => {
            if args.download_dir.is_some() || args.fetch_policy.is_some() {
                Err(error(
                    "convert",
                    ConversionError("download-options-require-url"),
                ))
            } else {
                Ok(None)
            }
        }
        crate::download_cli::Input::Url(url) => {
            let directory = args
                .download_dir
                .as_ref()
                .ok_or_else(|| error("convert", ConversionError("download-directory-required")))?;
            crate::download_cli::preflight(directory, &args.output).map_err(|cause| {
                error("convert", cause);
                ExitCode::from(2)
            })?;
            drmr_core::conversion::download_policy::read(url, args.fetch_policy.as_deref())
                .map(Some)
                .map_err(|cause| error("convert", cause))
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
) -> Result<(ConversionReport, Option<serde_json::Value>), ConversionError> {
    let downloaded = match input {
        crate::download_cli::Input::Url(url) => {
            if !args.json {
                eprintln!("Downloading source media...");
            }
            let source = command.download_hls(
                url,
                args.download_dir
                    .as_deref()
                    .expect("URL download directory was checked"),
                &args.output,
                fetch_policy.expect("URL fetch policy was checked"),
            )?;
            eprintln!(
                "Downloaded source retained at: {}",
                source.entrypoint().display()
            );
            Some(source)
        }
        crate::download_cli::Input::Local(_) => None,
    };
    let source = downloaded
        .as_ref()
        .map_or(args.input.as_path(), |source| source.entrypoint());
    let format = match local_format {
        Some(format) => format,
        None => inspection::inspect(source)
            .map_err(|_| ConversionError("invalid-input-media"))?
            .format
            .into(),
    };
    if !args.json {
        eprintln!("Converting and checking media...");
    }
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

pub fn convert(args: &ConvertArgs) -> ExitCode {
    let input = match crate::download_cli::Input::parse(args.input.as_os_str()) {
        Ok(input) => input,
        Err(cause) => return error("convert", cause),
    };
    let fetch_policy = match configured_download(args, &input) {
        Ok(policy) => policy,
        Err(code) => return code,
    };
    let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
        return error("convert", ConversionError("signal-monitor-unavailable"));
    };
    let keys = match load(args.keys_file.as_ref()) {
        Ok(keys) => keys,
        Err(code) => return code,
    };
    let local_format = match (&input, args.format) {
        (_, Some(format)) => Some(format),
        (crate::download_cli::Input::Local(path), None) => match inspection::inspect(path) {
            Ok(report) => Some(report.format.into()),
            Err(_) => return error("convert", ConversionError("invalid-input-media")),
        },
        (crate::download_cli::Input::Url(_), None) => None,
    };
    let converter = converter(
        &args.packager_bin,
        args.max_workers,
        !args.json,
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
        Err(cause) => return error("convert", cause),
    };
    if cancellation.stop.is_set() {
        return error("convert", ConversionError("cancelled"));
    }
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
    };
    ExitCode::from(u8::from(result.is_err()))
}

pub fn verify(args: &VerifyArgs) -> ExitCode {
    let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
        return error("verify", ConversionError("signal-monitor-unavailable"));
    };
    let keys = match load(args.keys_file.as_ref()) {
        Ok(keys) => keys,
        Err(code) => return code,
    };
    let output_keys = match keys.output_verification_keys() {
        Ok(keys) => keys,
        Err(cause) => return error("verify", cause),
    };
    let output_hls_keys = match keys.output_hls_verification_keys() {
        Ok(keys) => keys,
        Err(cause) => return error("verify", cause),
    };
    if !args.json {
        eprintln!("Comparing the source and result...");
    }
    let converter = converter(
        &args.packager_bin,
        args.max_workers,
        !args.json,
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
        Err(cause) => return error("verify", cause),
    };
    if cancellation.stop.is_set() {
        return error("verify", ConversionError("cancelled"));
    }
    let result = if args.json {
        let mut value = serde_json::to_value(&report).expect("verification report serializes");
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
    ExitCode::from(u8::from(result.is_err() || !report.passed))
}
