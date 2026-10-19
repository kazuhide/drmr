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
    /// Output protection: clear, cenc, cbcs, hls-aes128, or hls-sample-aes. Required with INPUT.
    #[arg(long, requires = "input")]
    pub to: Option<TargetProtection>,
    /// Output packaging: mp4, dash, hls, hls-ts, or a comma-separated list.
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
        "Evidence: {}; media samples have not been verified",
        match report.evidence {
            inspection::Evidence::ContainerMetadata => "container metadata",
            inspection::Evidence::Manifest => "manifest declarations",
        }
    )?;
    for track in &report.tracks {
        // Debug quoting prevents metadata from injecting terminal control sequences.
        writeln!(
            out,
            "  {:?}: {}, codec {:?}, {}",
            track.id,
            track.kind.as_str(),
            track.codec.as_deref().unwrap_or("unknown"),
            track.protection.as_str()
        )?;
    }
    if let Some(details) = &report.details {
        use inspection::{FactStatus, SampleCoverage};
        for detail in &details.track_protection {
            writeln!(
                out,
                "  {:?}: cipher {}; encryption unit {} ({})",
                detail.track_id.as_deref().unwrap_or("unknown"),
                detail.cipher.description(),
                detail.encryption_unit.description(),
                detail.evidence.description()
            )?;
            for method in &detail.hls_key_methods {
                writeln!(out, "    HLS method: {}", method.as_str())?;
            }
            match &detail.pattern.value {
                Some(value) => writeln!(
                    out,
                    "    Default pattern: {} encrypted / {} clear blocks (16 bytes each)",
                    value.encrypted_blocks, value.clear_blocks
                )?,
                None => writeln!(out, "    Pattern: {}", detail.pattern.status.description())?,
            }
            writeln!(
                out,
                "    Full or partial sample encryption: {}",
                match detail.sample_coverage {
                    SampleCoverage::Unknown => "unknown; sample auxiliary data was not inspected",
                    SampleCoverage::NotApplicable =>
                        "not applicable to the declared protection unit",
                }
            )?;
        }
        match (details.periods.status, details.periods.count) {
            (FactStatus::Known, Some(count)) => writeln!(out, "Declared DASH periods: {count}")?,
            (FactStatus::NotApplicable, _) => writeln!(out, "DASH periods: not applicable")?,
            _ => writeln!(out, "Declared DASH periods: unknown")?,
        }
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
    writeln!(
        out,
        "Plan status: {}",
        if plan.compatible {
            "conditional; source media, keys, and tools require validation"
        } else {
            "unsupported from the inspected declarations"
        }
    )?;
    summary(&plan.input, &mut out)?;
    let formats: Vec<_> = plan.formats.iter().map(|value| value.as_str()).collect();
    writeln!(
        out,
        "Target: {} ({})",
        plan.target.as_str(),
        formats.join(", ")
    )?;
    writeln!(out, "Operations: {}", plan.operations.join(" -> "))?;
    if let Some(processing) = &plan.processing {
        writeln!(
            out,
            "Target cipher: {}; encryption unit {} (requested intent)",
            processing.target.cipher.description(),
            processing.target.encryption_unit.description()
        )?;
        for candidate in &processing.candidates {
            let tools = candidate
                .required_tools
                .iter()
                .map(|tool| tool.as_str())
                .collect::<Vec<_>>();
            writeln!(
                out,
                "Conditional route: {} -> {}",
                candidate.route.as_str(),
                candidate.output_format.as_str()
            )?;
            writeln!(out, "  {}", candidate.explanation)?;
            writeln!(
                out,
                "  Required tools: {}; conditions: {}",
                tools.join(", "),
                candidate.condition_codes.join(", ")
            )?;
        }
        if processing.candidates.is_empty() {
            writeln!(
                out,
                "No candidate route is available from the inspected declarations."
            )?;
        }
        writeln!(
            out,
            "Source revalidation is required; tools and media have not been verified by this plan."
        )?;
    }
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
