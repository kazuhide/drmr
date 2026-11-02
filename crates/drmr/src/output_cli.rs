//! New output defaults are distinct from immutable saved-plan execution.
use drmr_core::{
    conversion::{ConversionError, Converter, dash_addressing::DashAddressing},
    inspection::OutputFormat,
};
use std::path::{Path, PathBuf};

/// Public output profiles remain distinct from input metadata and saved-plan serialization.
pub(crate) fn parse_format(value: &str) -> Result<OutputFormat, String> {
    match value {
        "mp4" => Ok(OutputFormat::Mp4),
        "dash" => Ok(OutputFormat::Dash),
        "hls-fmp4" => Ok(OutputFormat::Hls),
        "hls-ts" => Ok(OutputFormat::HlsTs),
        _ => Err("expected mp4, dash, hls-fmp4, or hls-ts".into()),
    }
}

pub(crate) fn profile_name(format: OutputFormat) -> &'static str {
    match format {
        OutputFormat::Mp4 => "mp4",
        OutputFormat::Dash => "dash",
        OutputFormat::Hls => "hls-fmp4",
        OutputFormat::HlsTs => "hls-ts",
    }
}

pub(crate) fn error_hint(code: &str) -> Option<&'static str> {
    match code {
        "output-destination-required" => Some(
            "Choose --output-file for one MP4, --output-dir for streaming output, or output_dir in --output-config.",
        ),
        "output-destination-conflict" => Some("Choose one output destination kind."),
        "mp4-output-file-required" => Some(
            "Use --output-file for a single MP4 result; use --output-config for a named MP4 in a result directory.",
        ),
        "stream-output-dir-required" => Some("Use --output-dir for HLS or DASH results."),
        _ => None,
    }
}

pub(crate) fn bundle_destination(
    explicit: Option<&Path>,
    configured: Option<PathBuf>,
) -> Result<PathBuf, ConversionError> {
    let output = explicit
        .map(Path::to_owned)
        .or(configured)
        .ok_or(ConversionError("output-destination-required"))?;
    crate::reporting::protect(&output, true)?;
    Ok(output)
}

pub(crate) fn validate_destination(
    file: Option<&Path>,
    directory: Option<&Path>,
    format: OutputFormat,
) -> Result<(), ConversionError> {
    if file.is_some() && directory.is_some() {
        return Err(ConversionError("output-destination-conflict"));
    }
    if format == OutputFormat::Mp4 {
        if file.is_none() {
            return Err(ConversionError("mp4-output-file-required"));
        }
    } else if directory.is_none() {
        return Err(ConversionError("stream-output-dir-required"));
    }
    Ok(())
}

pub(crate) fn manifest_tools(
    hls: &str,
    php: &str,
) -> drmr_core::conversion::conformance::ManifestValidationTools {
    drmr_core::conversion::conformance::ManifestValidationTools {
        mediastreamvalidator: Some(hls.into()),
        dash_manifest_validator: Some(php.into()),
    }
}

pub(crate) fn validation(
    mut command: Converter,
    profile: Option<drmr_core::conversion::manifest_lint::ManifestProfile>,
    external: bool,
    tools: drmr_core::conversion::conformance::ManifestValidationTools,
) -> Converter {
    if let Some(profile) = profile {
        command = command.with_manifest_profile(profile);
    }
    if external {
        command = command.with_external_manifest_check(tools);
    }
    command
}

pub(crate) fn override_validation(
    outputs: &mut [drmr_core::conversion::plan::bundle::BundleOutputIntent],
    profile: Option<drmr_core::conversion::manifest_lint::ManifestProfile>,
    external: bool,
) {
    for output in outputs {
        if let Some(profile) = profile {
            output.manifest_profile = Some(profile);
        }
        if external {
            output.conformance = drmr_core::conversion::plan::bundle::ManifestValidation::Required;
        }
    }
}

pub fn configure(command: Converter, addressing: Option<DashAddressing>) -> Converter {
    command
        .with_new_output_defaults()
        .with_dash_addressing(addressing.unwrap_or(DashAddressing::TemplateDuration))
}

pub fn validate(
    addressing: Option<DashAddressing>,
    formats: impl IntoIterator<Item = OutputFormat>,
) -> Result<(), ConversionError> {
    if addressing.is_some()
        && !formats
            .into_iter()
            .any(|format| format == OutputFormat::Dash)
    {
        return Err(ConversionError("dash-addressing-requires-dash"));
    }
    Ok(())
}
