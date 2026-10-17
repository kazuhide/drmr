//! Generic media commands with protected operator key files and secret-free summaries.

use clap::Args;
use drmr_core::conversion::{
    ConversionError, Converter,
    key_file::{self, OperatorKeys},
};
use drmr_core::events::NoSink;
use drmr_core::inspection::{OutputFormat, TargetProtection};
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
                eprintln!("keys: {error}");
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
    ExitCode::from(
        if matches!(
            error.0,
            "input-key-required"
                | "output-key-required"
                | "output-key-not-used"
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
    if args
        .format
        .is_some_and(|format| format != OutputFormat::Mp4)
    {
        return error("convert", ConversionError("output-format-not-supported"));
    }
    if !args.json {
        eprintln!("Converting and checking media...");
    }
    let report = match converter(&args.packager_bin).convert_mp4(
        &args.input,
        &args.output,
        args.to,
        &keys.input,
        keys.output.as_ref(),
    ) {
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
    if !args.json {
        eprintln!("Comparing the source and result...");
    }
    let report = match converter(&args.packager_bin).verify_mp4(
        &args.source,
        &keys.input,
        &args.output,
        &output_keys,
    ) {
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
