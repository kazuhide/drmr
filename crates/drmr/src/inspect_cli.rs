//! Read-only metadata inspection and conversion planning.

use crate::convert_cli::{self, Progress};
use clap::Args;
use drmr_core::conversion::{ConversionError, ProcessingMode, dash_addressing::DashAddressing};
use drmr_core::inspection::{self, Inspection, OutputFormat, TargetProtection};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Clone, Args)]
#[command(mut_arg("decode", |arg| arg.requires("save").help("Also decode all media while saving the plan; later runs retain this minimum.")))]
pub struct PlanArgs {
    /// Local media or manifest whose conversion intent should be described.
    #[arg(help_heading = "Input", value_name = "INPUT")]
    pub input: PathBuf,
    /// Output protection for one result; use --output-config for several results.
    #[arg(
        long = "encryption",
        required_unless_present = "outputs",
        conflicts_with = "outputs"
    )]
    #[arg(help_heading = "Output", value_name = "MODE")]
    pub to: Option<TargetProtection>,
    /// Output packaging: mp4, dash, hls, hls-ts, or a comma-separated list.
    #[arg(long = "output-format", value_delimiter = ',', value_parser = crate::output_cli::parse_format)]
    #[arg(help_heading = "Output", value_name = "FORMAT")]
    pub format: Vec<OutputFormat>,
    /// Processing mode: segments keeps source boundaries; stitch joins media before packaging.
    /// HLS/DASH output from stitch is segmented again. Default: auto.
    #[arg(long, value_enum, requires = "save")]
    #[arg(help_heading = "Output", value_name = "MODE")]
    pub processing: Option<ProcessingMode>,
    /// DASH segment references for new output; default: template. Source directories are retained.
    #[arg(long, value_enum, requires = "save")]
    #[arg(help_heading = "Output", value_name = "MODE")]
    pub dash_addressing: Option<DashAddressing>,
    /// Print the versioned JSON plan without executing any conversion.
    #[arg(long)]
    #[arg(help_heading = "Reporting")]
    pub json: bool,
    /// New private execution-plan file; requires --output-dir and one intent or --output-config.
    #[arg(long)]
    #[arg(help_heading = "Plan", value_name = "FILE")]
    pub save: Option<PathBuf>,
    /// JSON output list, with each result's format, protection, DRM systems and manifest checks.
    #[arg(long = "output-config", requires = "save", conflicts_with_all = ["to", "format", "output_file"])]
    #[arg(help_heading = "Output", value_name = "FILE")]
    pub outputs: Option<PathBuf>,
    /// New single MP4 result file fixed in the saved execution plan.
    #[arg(long, requires = "save", conflicts_with_all = ["output_dir", "outputs"])]
    #[arg(help_heading = "Output", value_name = "FILE")]
    pub output_file: Option<PathBuf>,
    /// New streaming or named-output result directory fixed in the saved plan.
    #[arg(long, requires = "save", conflicts_with = "output_file")]
    #[arg(help_heading = "Output", value_name = "DIR")]
    pub output_dir: Option<PathBuf>,
    /// Retain an original URL source for a saved execution plan.
    #[arg(long, requires = "save")]
    #[arg(help_heading = "Input", value_name = "DIR")]
    pub download_dir: Option<PathBuf>,
    /// Private HTTP settings for URL metadata or retained acquisition.
    #[arg(long = "fetch-config")]
    #[arg(help_heading = "Input", value_name = "FILE")]
    pub fetch_policy: Option<PathBuf>,
    /// Required native final-manifest rules recorded in the execution plan.
    #[arg(long, value_enum, requires = "save")]
    #[arg(help_heading = "Verification", value_name = "PROFILE")]
    pub manifest_profile: Option<drmr_core::conversion::manifest_lint::ManifestProfile>,
    /// Require external final-manifest checks when the saved plan is executed.
    #[arg(long, requires = "save")]
    #[arg(help_heading = "Verification")]
    pub external_manifest_check: bool,
    /// Protected input/output key configuration used for complete source checks.
    #[arg(long, requires = "save")]
    #[arg(help_heading = "Input", value_name = "FILE")]
    pub keys_file: Option<PathBuf>,
    /// Shaka Packager executable used for complete source checks.
    #[arg(long, requires = "save")]
    #[arg(help_heading = "Tools", value_name = "PROGRAM")]
    pub packager_bin: Option<String>,
    /// Upper bound for simultaneous media work (1-64); default adapts to CPU and memory.
    #[arg(long, requires = "save", value_parser = clap::value_parser!(u8).range(1..=64))]
    #[arg(help_heading = "Execution", value_name = "N")]
    pub max_workers: Option<u8>,
    #[command(flatten)]
    pub decoding: convert_cli::DecodeOptions,
    /// Include detailed observed statistics; only available when saving a plan.
    #[arg(long = "stats", requires = "save")]
    #[arg(help_heading = "Reporting")]
    pub measure: bool,
    /// Write the same structured result to a new private JSON file.
    #[arg(long)]
    #[arg(help_heading = "Reporting", value_name = "FILE")]
    pub report_file: Option<PathBuf>,
}

impl PlanArgs {
    pub(crate) fn destination(&self) -> Option<&Path> {
        self.output_file.as_deref().or(self.output_dir.as_deref())
    }
}

fn inspect_input(
    args: &PlanArgs,
    progress: &Arc<Progress>,
    stop: drmr_core::concurrency::Stop,
) -> Result<Inspection, convert_cli::ConfigurationError> {
    match crate::download_cli::Input::parse(args.input.as_os_str())? {
        crate::download_cli::Input::Local(path) => {
            if args.fetch_policy.is_some() {
                return Err(ConversionError("download-options-require-url").into());
            }
            inspection::inspect(&path).map_err(|error| ConversionError(error.0).into())
        }
        crate::download_cli::Input::Url(url) => {
            let policy =
                drmr_core::conversion::download_policy::read(&url, args.fetch_policy.as_deref())?;
            progress.mark("acquisition");
            convert_cli::converter("packager", None, false, progress.clone(), stop)
                .inspect_url_metadata(&url, &policy)
                .map_err(Into::into)
        }
    }
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
    let progress = Arc::new(Progress::new(!args.json));
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
    let Ok(cancellation) = crate::cancellation::Cancellation::install() else {
        return convert_cli::error(
            "plan",
            ConversionError("signal-monitor-unavailable"),
            args.json,
            &progress,
            None,
        );
    };
    progress.mark("inspection");
    let report = match inspect_input(args, &progress, cancellation.stop.clone()) {
        Ok(value) => value,
        Err(cause) => return convert_cli::error("plan", cause, args.json, &progress, Some(2)),
    };
    if cancellation.stop.is_set() {
        return convert_cli::error(
            "plan",
            ConversionError("cancelled"),
            args.json,
            &progress,
            None,
        );
    }
    let plan = inspection::plan(report, target, args.format.clone());
    progress.mark("publication");
    let success = progress
        .write_result(|| {
            crate::reporting::render(
                &serde_json::to_value(&plan).expect("conversion plan serializes"),
                args.json,
                || text_plan(&plan),
            )
        })
        .is_ok();
    if success {
        progress.finish();
    } else {
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
    let formats: Vec<_> = plan
        .formats
        .iter()
        .map(|value| crate::output_cli::profile_name(*value))
        .collect();
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
                crate::output_cli::profile_name(candidate.output_format)
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
