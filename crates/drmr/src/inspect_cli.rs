//! Read-only metadata commands. Legacy catalog planning remains available through --mpd.

use clap::Args;
use drmr_core::inspection::{self, Inspection, OutputFormat, TargetProtection};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Args)]
pub struct InspectArgs {
    /// Local MP4, DASH manifest, or HLS playlist. References are not fetched remotely.
    pub input: PathBuf,
    /// Print the versioned JSON metadata report.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct PlanArgs {
    /// Local media or manifest whose conversion intent should be described.
    #[arg(conflicts_with = "mpd")]
    pub input: Option<PathBuf>,
    /// Keep the original read-only catalog rendition listing.
    #[arg(long, required_unless_present = "input")]
    pub mpd: Option<PathBuf>,
    /// Output protection: clear, cenc, cbcs, or hls-aes128. Required with INPUT.
    #[arg(long, requires = "input")]
    pub to: Option<TargetProtection>,
    /// Output packaging: mp4, dash, hls, or a comma-separated list.
    #[arg(long, value_delimiter = ',', requires = "input")]
    pub format: Vec<OutputFormat>,
    /// Print the versioned JSON plan without executing any conversion.
    #[arg(long, requires = "input")]
    pub json: bool,
    /// Override the asset name for the original --mpd catalog listing.
    #[arg(long, requires = "mpd")]
    pub asset_id: Option<String>,
}

fn write_json(value: &serde_json::Value) -> bool {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value).is_ok() && writeln!(out).is_ok()
}

fn inspect_input(path: &Path, command: &str) -> Result<Inspection, ExitCode> {
    inspection::inspect(path).map_err(|error| {
        eprintln!("{command}: {error}");
        ExitCode::from(2)
    })
}

fn summary(report: &Inspection, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(out, "Format: {}", report.format.as_str())?;
    writeln!(out, "Protection: {}", report.protection.as_str())?;
    writeln!(
        out,
        "Evidence: {:?}; media samples have not been verified",
        report.evidence
    )?;
    for track in &report.tracks {
        // Debug quoting prevents metadata from injecting terminal control sequences.
        writeln!(
            out,
            "  {:?}: {:?}, codec {:?}, {}",
            track.id,
            track.kind,
            track.codec,
            track.protection.as_str()
        )?;
    }
    for finding in &report.findings {
        writeln!(out, "Note: {}", finding.code)?;
    }
    Ok(())
}

pub fn inspect(args: &InspectArgs) -> ExitCode {
    let report = match inspect_input(&args.input, "inspect") {
        Ok(value) => value,
        Err(code) => return code,
    };
    let success = if args.json {
        write_json(&serde_json::to_value(&report).expect("metadata report serializes"))
    } else {
        summary(&report, &mut std::io::stdout().lock()).is_ok()
    };
    ExitCode::from(u8::from(!success))
}

pub fn plan(args: &PlanArgs) -> ExitCode {
    let (Some(input), Some(target)) = (&args.input, args.to) else {
        eprintln!("plan: --to is required with INPUT");
        return ExitCode::from(2);
    };
    let report = match inspect_input(input, "plan") {
        Ok(value) => value,
        Err(code) => return code,
    };
    let plan = inspection::plan(report, target, args.format.clone());
    let success = if args.json {
        write_json(&serde_json::to_value(&plan).expect("conversion plan serializes"))
    } else {
        text_plan(&plan).is_ok()
    };
    ExitCode::from(u8::from(!success || !plan.compatible))
}

fn text_plan(plan: &inspection::ConversionPlan) -> std::io::Result<()> {
    let mut out = std::io::stdout().lock();
    summary(&plan.input, &mut out)?;
    let formats: Vec<_> = plan.formats.iter().map(|value| value.as_str()).collect();
    writeln!(
        out,
        "Target: {} ({})",
        plan.target.as_str(),
        formats.join(", ")
    )?;
    writeln!(out, "Operations: {}", plan.operations.join(" -> "))?;
    for keys in &plan.keys {
        writeln!(out, "Keys required: {}", keys.role)?;
    }
    writeln!(out, "Compatible intent: {}", plan.compatible)?;
    writeln!(out, "Execution available: {}", plan.execution_available)?;
    for finding in &plan.findings {
        writeln!(out, "Note: {}", finding.code)?;
    }
    Ok(())
}
