//! Generic media commands with protected operator key files and secret-free summaries.

use clap::Args;
use drmr_core::conversion::{
    ConversionError, ConversionRequest, Converter, VerificationRequest, VerificationSource,
    key_file::{self, OperatorKeys},
};
use drmr_core::events::NoSink;
use drmr_core::inspection::{self, OutputFormat, TargetProtection};
use drmr_core::toolrun::{Secrets, ToolRunner};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Args)]
pub struct ConvertArgs {
    /// Local source media. The source is never overwritten.
    pub input: PathBuf,
    /// Target protection: clear, cenc, cbcs, or hls-aes128.
    #[arg(long)]
    pub to: TargetProtection,
    /// Output container or delivery format.
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
    /// Print the versioned result as one JSON object.
    #[arg(long)]
    pub json: bool,
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
    /// Print the versioned verification result as one JSON object.
    #[arg(long)]
    pub json: bool,
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

fn converter(packager: &str) -> Converter {
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)));
    Converter::new(runner, packager, "ffmpeg")
}

fn error(command: &str, error: ConversionError) -> ExitCode {
    eprintln!("{command}: {error}");
    if let Some(hint) = match error.0 {
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
    let report = match converter(&args.packager_bin).convert(ConversionRequest {
        input: &args.input,
        output: &args.output,
        format,
        target: args.to,
        input_keys: &keys.input,
        hls_input_keys: &keys.hls_input,
        output_key: keys.output.as_ref(),
        hls_key_uri: keys.hls_output_uri(),
        hls_output_key: keys.hls_output.as_ref(),
    }) {
        Ok(report) => report,
        Err(cause) => return error("convert", cause),
    };
    let result = if args.json {
        json(&serde_json::to_value(&report).expect("conversion report serializes"))
    } else {
        let mut out = std::io::stdout().lock();
        writeln!(
            out,
            "Converted: {} -> {}",
            report.input.protection.as_str(),
            report.output.protection.as_str()
        )
        .and_then(|()| {
            writeln!(
                out,
                "Verified packet payloads, decoded content, and packet timing."
            )
        })
    };
    ExitCode::from(u8::from(result.is_err()))
}

pub fn verify(args: &VerifyArgs) -> ExitCode {
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
    let report = match converter(&args.packager_bin).verify(VerificationRequest {
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
    }) {
        Ok(report) => report,
        Err(cause) => return error("verify", cause),
    };
    let result = if args.json {
        json(&serde_json::to_value(&report).expect("verification report serializes"))
    } else {
        writeln!(
            std::io::stdout().lock(),
            "Verification: {}",
            if report.passed { "passed" } else { "failed" }
        )
    };
    ExitCode::from(u8::from(result.is_err() || !report.passed))
}
