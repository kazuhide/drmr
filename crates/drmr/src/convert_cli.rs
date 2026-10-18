//! Generic media commands with protected operator key files and secret-free summaries.

use clap::Args;
use drmr_core::conversion::{
    ConversionError, ConversionRequest, Converter, VerificationRequest, VerificationSource,
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
    /// Local source media. The source is never overwritten.
    pub input: PathBuf,
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
            eprintln!("Processing up to {workers} tracks at a time (CPU and memory limits).");
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
            "input-key-required"
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

pub fn convert(args: &ConvertArgs) -> ExitCode {
    let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
        return error("convert", ConversionError("signal-monitor-unavailable"));
    };
    let keys = match load(args.keys_file.as_ref()) {
        Ok(keys) => keys,
        Err(code) => return code,
    };
    let format = match args.format {
        Some(format) => format,
        None => match inspection::inspect(&args.input) {
            Ok(report) => report.format.into(),
            Err(_) => return error("convert", ConversionError("invalid-input-media")),
        },
    };
    if !args.json {
        eprintln!("Converting and checking media...");
    }
    let converter = converter(
        &args.packager_bin,
        args.max_workers,
        !args.json,
        cancellation.stop.clone(),
    );
    let execute = |command: &Converter| {
        command.convert(ConversionRequest {
            input: &args.input,
            output: &args.output,
            format,
            target: args.to,
            input_keys: &keys.input,
            hls_input_keys: &keys.hls_input,
            output_key: keys.output.as_ref(),
            hls_key_uri: keys.output.as_ref().and_then(|_| keys.hls_output_uri()),
            hls_output_key: keys.hls_output.as_ref(),
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
        Err(cause) => return error("convert", cause),
    };
    if cancellation.stop.is_set() {
        return error("convert", ConversionError("cancelled"));
    }
    let result = if args.json {
        let mut value = serde_json::to_value(&report).expect("conversion report serializes");
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
