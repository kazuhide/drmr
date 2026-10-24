//! Read-only metadata inspection and conversion planning.

use crate::convert_cli::{self, Progress};
use clap::Args;
use drmr_core::conversion::{ConversionError, ProcessingMode};
use drmr_core::inspection::{self, Inspection, OutputFormat, TargetProtection};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Args)]
pub struct PlanArgs {
    /// Local media or manifest whose conversion intent should be described.
    pub input: PathBuf,
    /// Output protection for one result; use --outputs for several results.
    #[arg(long, required_unless_present = "outputs", conflicts_with = "outputs")]
    pub to: Option<TargetProtection>,
    /// Output packaging: mp4, dash, hls, hls-ts, or a comma-separated list.
    #[arg(long, value_delimiter = ',')]
    pub format: Vec<OutputFormat>,
    /// Processing mode: segments keeps source boundaries; stitch joins media before packaging.
    /// HLS/DASH output from stitch is segmented again. Default: auto.
    #[arg(long, value_enum, requires = "save")]
    pub processing: Option<ProcessingMode>,
    /// Print the versioned JSON plan without executing any conversion.
    #[arg(long)]
    pub json: bool,
    /// New private execution-plan file; requires --output and one intent or --outputs.
    #[arg(long, requires = "output")]
    pub save: Option<PathBuf>,
    /// JSON output list, with each result's format, protection, DRM systems and manifest checks.
    #[arg(long, requires = "save", conflicts_with_all = ["to", "format"])]
    pub outputs: Option<PathBuf>,
    /// New result file or directory recorded in the saved execution plan.
    #[arg(long, requires = "save")]
    pub output: Option<PathBuf>,
    /// Protected input/output key configuration used for complete source checks.
    #[arg(long, requires = "save")]
    pub keys_file: Option<PathBuf>,
    /// Shaka Packager executable used for complete source checks.
    #[arg(long, requires = "save")]
    pub packager_bin: Option<String>,
    /// Upper bound for simultaneous media work (1-64); default adapts to CPU and memory.
    #[arg(long, requires = "save", value_parser = clap::value_parser!(u8).range(1..=64))]
    pub max_workers: Option<u8>,
    /// Also decode all media while saving the plan; later runs retain this minimum.
    #[arg(long, requires = "save")]
    pub decode: bool,
    /// Include numeric measurements in JSON; only available when saving a plan.
    #[arg(long, requires_all = ["json", "save"])]
    pub measure: bool,
}

fn write_json(value: &serde_json::Value) -> bool {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut out, value).is_ok() && writeln!(out).is_ok()
}

fn inspect_input(path: &Path) -> Result<Inspection, ConversionError> {
    inspection::inspect(path).map_err(|error| ConversionError(error.0))
}

pub(crate) fn summary(report: &Inspection, out: &mut impl Write) -> std::io::Result<()> {
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

pub fn plan(args: &PlanArgs) -> ExitCode {
    if args.save.is_some() {
        return crate::plan_cli::save(args);
    }
    let progress = Progress::new(!args.json);
    progress.mark("configuration");
    let Some(target) = args.to else {
        return convert_cli::error(
            "plan",
            ConversionError("target-required"),
            args.json,
            &progress,
            Some(2),
        );
    };
    progress.mark("inspection");
    let report = match inspect_input(&args.input) {
        Ok(value) => value,
        Err(cause) => return convert_cli::error("plan", cause, args.json, &progress, Some(2)),
    };
    let plan = inspection::plan(report, target, args.format.clone());
    progress.mark("publication");
    let success = if args.json {
        write_json(&serde_json::to_value(&plan).expect("conversion plan serializes"))
    } else {
        text_plan(&plan).is_ok()
    };
    progress.finish();
    if !success {
        progress.diagnostic("plan", "result-output-unavailable");
    }
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
                "  Native minimum tools: {}; conditions: {}",
                if tools.is_empty() {
                    "none".into()
                } else {
                    tools.join(", ")
                },
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
