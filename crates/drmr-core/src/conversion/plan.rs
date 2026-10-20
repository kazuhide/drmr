//! Executable local intent, with byte identities and independently supplied runtime secrets.
pub mod bundle;
pub(super) mod destination;
#[cfg(test)]
mod tests;
use super::{
    ConversionError, ConversionReport, ConversionRequest, Converter, FrozenSource,
    SourceCompatibilityNote, SourceInventory, SourceProtection, SourceQualification,
    SourceValidationPurpose, VerificationChecks, VerificationReport, VerificationRequest,
    VerificationSource, files, frozen, key_file::OperatorKeys,
};
use crate::inspection::{Inspection, OutputFormat, TargetProtection};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
    sync::Arc,
};

const KIND: &str = "execution-plan";
const INVALID: ConversionError = ConversionError("execution-plan-invalid");
const UNSAFE: ConversionError = ConversionError("execution-plan-destination-unsafe");
const UNSUPPORTED: ConversionError = ConversionError("execution-plan-not-supported");
const CHANGED: ConversionError = ConversionError("execution-plan-source-changed");
const EXISTS: ConversionError = ConversionError("execution-plan-exists");
const LIMIT: u64 = 32 * 1024 * 1024;

/// Both supported saved schemas retain their own strict fields and source identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SavedPlan {
    Single(ExecutionPlan),
    Bundle(bundle::BundlePlan),
}

/// Dispatch from a duplicate-safe header, then parse the original bytes into a strict schema.
pub fn read_saved(path: &Path) -> Result<SavedPlan, ConversionError> {
    #[derive(Deserialize)]
    struct Header {
        kind: String,
        schema_version: u32,
    }
    let file = crate::native::open_private_read(path).map_err(|_| INVALID)?;
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.len() as u64 > LIMIT {
        return Err(INVALID);
    }
    let header: Header = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    if header.kind != KIND {
        return Err(INVALID);
    }
    match header.schema_version {
        1 => {
            let plan = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
            validate(&plan)?;
            Ok(SavedPlan::Single(plan))
        }
        2 => {
            let plan = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
            bundle::validate(&plan)?;
            Ok(SavedPlan::Bundle(plan))
        }
        _ => Err(INVALID),
    }
}

/// Save a new private version-two plan without replacing another entry.
pub fn write_bundle(path: &Path, plan: &bundle::BundlePlan) -> Result<(), ConversionError> {
    bundle::write_new(path, plan)
}

/// The output location and media intent are fixed; keys and tool configuration are not saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputIntent {
    pub format: OutputFormat,
    pub target: TargetProtection,
    pub output: PathBuf,
}

/// A digest detects accidental edits, not malicious authorship. Revalidate before every use.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPlan {
    pub kind: String,
    pub schema_version: u32,
    pub plan_id: String,
    pub source: SourceInventory,
    pub source_format: OutputFormat,
    pub intent: OutputIntent,
}
impl std::fmt::Debug for ExecutionPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecutionPlan")
            .field("schema_version", &self.schema_version)
            .field("plan_id", &self.plan_id)
            .field("format", &self.intent.format)
            .field("target", &self.intent.target)
            .finish_non_exhaustive()
    }
}

/// Save one qualified output. Output encryption keys are needed only at execution time.
#[derive(Clone, Copy)]
pub struct ExecutionPlanRequest<'a> {
    pub input: VerificationSource<'a>,
    pub output: &'a Path,
    pub format: Option<OutputFormat>,
    pub target: TargetProtection,
}

/// Full input admission is evidence about the source, not a converted result or DRM playback.
#[derive(Clone, Debug, Serialize)]
pub struct InputAnalysis {
    pub kind: &'static str,
    pub schema_version: u32,
    pub input_admitted: bool,
    pub input_checks: VerificationChecks,
    pub input: Inspection,
    pub actual_format: OutputFormat,
    pub track_protection: Vec<SourceProtection>,
    pub compatibility_notes: Vec<SourceCompatibilityNote>,
    pub eligible_formats: Vec<OutputFormat>,
    pub source: SourceInventory,
}

/// Source identity, output intent, and independent media comparison are separate outcomes.
#[derive(Clone, Debug, Serialize)]
pub struct PlanVerificationReport {
    pub kind: &'static str,
    pub schema_version: u32,
    pub plan_id: String,
    pub source_unchanged: bool,
    pub output_matches_plan: bool,
    pub verification: VerificationReport,
}

impl Converter {
    /// Check all source media from a private byte-bound copy without publishing a conversion.
    pub fn analyze_source(
        &self,
        source: VerificationSource<'_>,
    ) -> Result<InputAnalysis, ConversionError> {
        let frozen = FrozenSource::capture(source.input, &self.backend.stop)?;
        let command = self.using_frozen_source(&frozen);
        let qualified = command.validate_source(
            rebound(source, &frozen),
            SourceValidationPurpose::Conversion,
        )?;
        frozen.check_original()?;
        Ok(InputAnalysis {
            kind: "input-analysis",
            schema_version: 1,
            input_admitted: true,
            input_checks: qualified.verification.checks,
            input: qualified.input,
            actual_format: qualified.actual_format,
            track_protection: qualified.track_protection,
            compatibility_notes: qualified.compatibility_notes,
            eligible_formats: qualified.eligible_formats,
            source: frozen.inventory().clone(),
        })
    }

    pub fn create_execution_plan(
        &self,
        request: ExecutionPlanRequest<'_>,
    ) -> Result<ExecutionPlan, ConversionError> {
        self.backend.check_cancelled()?;
        let output = super::destination(request.output).map_err(|_| UNSAFE)?;
        let destination = destination::OutputGuard::new(&output)?;
        let frozen = FrozenSource::capture(request.input.input, &self.backend.stop)?;
        let command = self.using_frozen_source(&frozen);
        let qualified = command.validate_source(
            rebound(request.input, &frozen),
            SourceValidationPurpose::Conversion,
        )?;
        let format = request
            .format
            .unwrap_or_else(|| OutputFormat::from(qualified.input.format));
        if !qualified.eligible_formats.contains(&format) || !supported(format, request.target) {
            return Err(UNSUPPORTED);
        }
        let mut plan = ExecutionPlan {
            kind: KIND.into(),
            schema_version: 1,
            plan_id: String::new(),
            source: frozen.inventory().clone(),
            source_format: qualified.actual_format,
            intent: OutputIntent {
                format,
                target: request.target,
                output,
            },
        };
        check_output_location(&plan)?;
        plan.plan_id = identity(&plan)?;
        validate(&plan)?;
        frozen.check_original().map_err(source_error)?;
        destination.check()?;
        Ok(plan)
    }

    /// Reopen only the saved graph, then use the ordinary verified, atomic conversion route.
    pub fn execute_plan(
        &self,
        plan: &ExecutionPlan,
        keys: &OperatorKeys,
    ) -> Result<ConversionReport, ConversionError> {
        validate(plan)?;
        self.backend.check_cancelled()?;
        if super::destination(&plan.intent.output)? != plan.intent.output {
            return Err(UNSAFE);
        }
        let destination = destination::OutputGuard::new(&plan.intent.output)?;
        let frozen =
            FrozenSource::from_inventory(&plan.source, &self.backend.stop).map_err(source_error)?;
        let mut command = self
            .using_frozen_source(&frozen)
            .using_plan_destination(destination);
        if let Some(signaling) = &keys.signaling {
            if command.output_signaling.is_some() {
                return Err(ConversionError("output-signaling-profile-conflict"));
            }
            command.output_signaling = Some(Arc::new(signaling.clone()));
        }
        let source = VerificationSource {
            input: frozen.entrypoint(),
            input_keys: &keys.input,
            hls_keys: &keys.hls_input,
        };
        let qualified = command.validate_source(source, SourceValidationPurpose::Conversion)?;
        check_qualification(plan, &qualified)?;
        command
            .convert(ConversionRequest {
                input: frozen.entrypoint(),
                output: &plan.intent.output,
                format: plan.intent.format,
                target: plan.intent.target,
                input_keys: &keys.input,
                hls_input_keys: &keys.hls_input,
                output_key: keys.output.as_ref(),
                hls_key_uri: keys.output.as_ref().and_then(|_| keys.hls_output_uri()),
                hls_output_key: keys.hls_output.as_ref(),
            })
            .map_err(source_error)
    }

    /// Read current result bytes and compare them with the saved original; no conversion receipt is trusted.
    pub fn verify_plan(
        &self,
        plan: &ExecutionPlan,
        keys: &OperatorKeys,
    ) -> Result<PlanVerificationReport, ConversionError> {
        validate(plan)?;
        self.backend.check_cancelled()?;
        let original =
            FrozenSource::from_inventory(&plan.source, &self.backend.stop).map_err(source_error)?;
        let result = FrozenSource::capture(&result_entrypoint(plan), &self.backend.stop)?;
        let command = self.using_frozen_source(&original);
        let output_keys = keys.output_verification_keys()?;
        let output_hls_keys = keys.output_hls_verification_keys()?;
        let source = VerificationSource {
            input: original.entrypoint(),
            input_keys: &keys.input,
            hls_keys: &keys.hls_input,
        };
        let output = VerificationSource {
            input: result.entrypoint(),
            input_keys: &output_keys,
            hls_keys: &output_hls_keys,
        };
        let qualified = command.validate_source(output, SourceValidationPurpose::Verification)?;
        let output_matches_plan = qualified.actual_format == plan.intent.format
            && !qualified.track_protection.is_empty()
            && qualified
                .track_protection
                .iter()
                .all(|protection| protection.matches_target(plan.intent.target));
        let verification = command.verify(VerificationRequest { source, output })?;
        if keys.signaling.is_some() {
            command.verify_output_signaling(
                result.entrypoint(),
                plan.intent.format,
                plan.intent.target,
                keys,
            )?;
        }
        original.check_original().map_err(source_error)?;
        result.check_original().map_err(|error| {
            if error.0 == "cancelled" {
                error
            } else {
                ConversionError("verification-output-changed")
            }
        })?;
        Ok(PlanVerificationReport {
            kind: "plan-verification",
            schema_version: 1,
            plan_id: plan.plan_id.clone(),
            source_unchanged: true,
            output_matches_plan,
            verification,
        })
    }
}

fn rebound<'a>(source: VerificationSource<'a>, frozen: &'a FrozenSource) -> VerificationSource<'a> {
    VerificationSource {
        input: frozen.entrypoint(),
        ..source
    }
}
fn source_error(error: ConversionError) -> ConversionError {
    match error.0 {
        "planned-source-changed" => CHANGED,
        "source-inventory-invalid" => INVALID,
        _ => error,
    }
}
fn check_qualification(
    plan: &ExecutionPlan,
    qualified: &SourceQualification,
) -> Result<(), ConversionError> {
    if qualified.actual_format != plan.source_format
        || !qualified.eligible_formats.contains(&plan.intent.format)
    {
        return Err(UNSUPPORTED);
    }
    Ok(())
}
fn supported(format: OutputFormat, target: TargetProtection) -> bool {
    match format {
        OutputFormat::Mp4 | OutputFormat::Dash => matches!(
            target,
            TargetProtection::Clear | TargetProtection::Cenc | TargetProtection::Cbcs
        ),
        OutputFormat::Hls => matches!(
            target,
            TargetProtection::Clear | TargetProtection::Cbcs | TargetProtection::HlsAes128
        ),
        OutputFormat::HlsTs => matches!(
            target,
            TargetProtection::Clear | TargetProtection::HlsAes128 | TargetProtection::HlsSampleAes
        ),
    }
}
fn result_entrypoint(plan: &ExecutionPlan) -> PathBuf {
    match plan.intent.format {
        OutputFormat::Mp4 => plan.intent.output.clone(),
        OutputFormat::Dash => plan.intent.output.join("manifest.mpd"),
        OutputFormat::Hls | OutputFormat::HlsTs => plan.intent.output.join("master.m3u8"),
    }
}

#[derive(Serialize)]
struct Identity<'a> {
    kind: &'a str,
    schema_version: u32,
    source: &'a SourceInventory,
    source_format: OutputFormat,
    intent: &'a OutputIntent,
}
fn identity(plan: &ExecutionPlan) -> Result<String, ConversionError> {
    let bytes = serde_json::to_vec(&Identity {
        kind: &plan.kind,
        schema_version: plan.schema_version,
        source: &plan.source,
        source_format: plan.source_format,
        intent: &plan.intent,
    })
    .map_err(|_| INVALID)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
fn validate(plan: &ExecutionPlan) -> Result<(), ConversionError> {
    if plan.kind != KIND
        || plan.schema_version != 1
        || plan.plan_id.len() != 64
        || !plan
            .plan_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !supported(plan.intent.format, plan.intent.target)
        || !absolute(&plan.intent.output)
    {
        return Err(INVALID);
    }
    frozen::validate(&plan.source).map_err(|_| INVALID)?;
    if identity(plan)? != plan.plan_id {
        return Err(INVALID);
    }
    check_output_location(plan)
}
fn absolute(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= 16_384
        && path
            .to_str()
            .is_some_and(|text| !text.chars().any(char::is_control))
        && !path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        && path.file_name().is_some()
}
fn check_output_location(plan: &ExecutionPlan) -> Result<(), ConversionError> {
    let output = &plan.intent.output;
    if (plan.source_format != OutputFormat::Mp4 && output.starts_with(&plan.source.root))
        || plan.source.files.iter().any(|file| {
            let source = plan.source.root.join(&file.path);
            source.starts_with(output) || output.starts_with(source)
        })
    {
        return Err(UNSAFE);
    }
    Ok(())
}

/// Load only a bounded, owner-protected strict schema. Paths do not confer cleanup ownership.
pub fn read(path: &Path) -> Result<ExecutionPlan, ConversionError> {
    let file = crate::native::open_private_read(path).map_err(|_| INVALID)?;
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| INVALID)?;
    if bytes.len() as u64 > LIMIT {
        return Err(INVALID);
    }
    let plan: ExecutionPlan = serde_json::from_slice(&bytes).map_err(|_| INVALID)?;
    validate(&plan)?;
    Ok(plan)
}

/// Atomically publish a new private plan outside the source graph; never replace an existing file.
pub fn write_new(path: &Path, plan: &ExecutionPlan) -> Result<(), ConversionError> {
    validate(plan)?;
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(EXISTS);
    }
    let path = super::destination(path).map_err(|_| UNSAFE)?;
    if path == plan.intent.output
        || path.starts_with(&plan.intent.output)
        || (plan.source_format != OutputFormat::Mp4 && path.starts_with(&plan.source.root))
        || plan
            .source
            .files
            .iter()
            .any(|file| path == plan.source.root.join(&file.path))
    {
        return Err(UNSAFE);
    }
    let bytes = serde_json::to_vec_pretty(plan).map_err(|_| INVALID)?;
    if bytes.len() as u64 >= LIMIT {
        return Err(INVALID);
    }
    let destination = destination::OutputGuard::new(&path)?;
    let stage = files::Stage::new(path.parent())?;
    let staged = stage.path().join("execution-plan.json");
    let mut file = stage.create_file(&staged).map_err(|_| UNSAFE)?;
    file.write_all(&bytes)
        .and_then(|()| file.write_all(b"\n"))
        .and_then(|()| file.flush())
        .and_then(|()| file.sync_all())
        .map_err(|_| UNSAFE)?;
    let source = stage
        .created_file_source(&staged, &file)
        .map_err(|_| UNSAFE)?;
    drop(file);
    destination.commit(&source).map_err(|_| {
        if std::fs::symlink_metadata(&path).is_ok() {
            EXISTS
        } else {
            UNSAFE
        }
    })
}
