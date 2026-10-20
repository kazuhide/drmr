//! One byte-bound source and one atomic destination for several independently checked outputs.
use super::{EXISTS, INVALID, KIND, LIMIT, UNSAFE, UNSUPPORTED, absolute, destination};
use crate::{
    conversion::{
        ConversionError, ConversionReport, ConversionRequest, Converter, DrmSystem, FrozenSource,
        OutputSignaling, SourceInventory, SourceValidationPurpose, VerificationReport,
        VerificationRequest, VerificationSource,
        conformance::{ManifestValidationReport, ManifestValidationTools},
        files, frozen,
        key_file::{BundleOperatorKeys, OperatorKeys},
    },
    inspection::{OutputFormat, TargetProtection},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeSet,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};

const MAX_OUTPUTS: usize = 8;
const KEYS: ConversionError = ConversionError("execution-plan-output-keys-mismatch");
const DRM: ConversionError = ConversionError("execution-plan-drm-mismatch");

/// A required external check validates the final manifest, without a decrypted shadow copy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManifestValidation {
    #[default]
    None,
    Required,
}

/// A safe child name determines the complete output path; no arbitrary child paths are accepted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleOutputIntent {
    pub id: String,
    pub format: OutputFormat,
    pub target: TargetProtection,
    pub drms: Vec<DrmSystem>,
    #[serde(default)]
    pub conformance: ManifestValidation,
}

/// Version two fixes one new result directory and all requested media and DRM profiles.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundlePlan {
    pub kind: String,
    pub schema_version: u32,
    pub plan_id: String,
    pub source: SourceInventory,
    pub source_format: OutputFormat,
    pub output_root: PathBuf,
    pub outputs: Vec<BundleOutputIntent>,
}

impl std::fmt::Debug for BundlePlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BundlePlan")
            .field("schema_version", &self.schema_version)
            .field("plan_id", &self.plan_id)
            .field("outputs", &self.outputs)
            .finish_non_exhaustive()
    }
}

/// Source keys qualify the input; output keys and signaling stay outside the saved plan.
#[derive(Clone, Copy)]
pub struct BundlePlanRequest<'a> {
    pub input: VerificationSource<'a>,
    pub output_root: &'a Path,
    pub outputs: &'a [BundleOutputIntent],
}

#[derive(Clone, Debug, Serialize)]
pub struct DrmSignalingReport {
    pub systems: Vec<DrmSystem>,
    pub verified: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct BundleConversionOutput {
    pub id: String,
    pub conversion: ConversionReport,
    pub signaling: DrmSignalingReport,
    pub conformance: Option<ManifestValidationReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BundleConversionReport {
    pub kind: &'static str,
    pub schema_version: u32,
    pub plan_id: String,
    pub outputs: Vec<BundleConversionOutput>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BundleVerificationOutput {
    pub id: String,
    pub output_matches_plan: bool,
    pub verification: VerificationReport,
    pub signaling: DrmSignalingReport,
    pub conformance: Option<ManifestValidationReport>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BundleVerificationReport {
    pub kind: &'static str,
    pub schema_version: u32,
    pub plan_id: String,
    pub source_unchanged: bool,
    pub passed: bool,
    pub outputs: Vec<BundleVerificationOutput>,
}

impl Converter {
    pub fn create_bundle_plan(
        &self,
        request: BundlePlanRequest<'_>,
    ) -> Result<BundlePlan, ConversionError> {
        self.backend.check_cancelled()?;
        let outputs = normalized(request.outputs)?;
        let output_root = super::super::destination(request.output_root).map_err(|_| UNSAFE)?;
        let destination = destination::OutputGuard::new(&output_root)?;
        let frozen = FrozenSource::capture(request.input.input, &self.backend.stop)?;
        self.check_downloaded_inventory(frozen.inventory())?;
        let command = self.using_frozen_source(&frozen);
        let source = super::rebound(request.input, &frozen);
        let qualified = command.validate_source(source, SourceValidationPurpose::Conversion)?;
        for output in &outputs {
            if !qualified.eligible_formats.contains(&output.format) {
                return Err(UNSUPPORTED);
            }
            if !output.drms.is_empty() {
                command.validate_drm_source(source, &output.drms, output.format, output.target)?;
            }
        }
        let mut plan = BundlePlan {
            kind: KIND.into(),
            schema_version: 2,
            plan_id: String::new(),
            source: frozen.inventory().clone(),
            source_format: qualified.actual_format,
            output_root,
            outputs,
        };
        plan.plan_id = identity(&plan)?;
        validate(&plan)?;
        frozen.check_original().map_err(super::source_error)?;
        destination.check()?;
        Ok(plan)
    }

    /// Stage and check every child before publishing one new result directory.
    pub fn execute_bundle_plan(
        &self,
        plan: &BundlePlan,
        keys: &BundleOperatorKeys,
        tools: &ManifestValidationTools,
    ) -> Result<BundleConversionReport, ConversionError> {
        validate(plan)?;
        validate_keys(plan, keys)?;
        self.backend.check_cancelled()?;
        self.refuse_bundle_signaling()?;
        let destination = destination::OutputGuard::new(&plan.output_root)?;
        let frozen = FrozenSource::from_inventory(&plan.source, &self.backend.stop)
            .map_err(super::source_error)?;
        self.check_downloaded_inventory(frozen.inventory())?;
        let command = self.using_frozen_source(&frozen);
        let source = VerificationSource {
            input: frozen.entrypoint(),
            input_keys: &keys.input,
            hls_keys: &keys.hls_input,
        };
        let qualified = command.validate_source(source, SourceValidationPurpose::Conversion)?;
        if qualified.actual_format != plan.source_format
            || plan
                .outputs
                .iter()
                .any(|output| !qualified.eligible_formats.contains(&output.format))
        {
            return Err(UNSUPPORTED);
        }
        for output in &plan.outputs {
            if !output.drms.is_empty() {
                command.validate_drm_source(source, &output.drms, output.format, output.target)?;
            }
        }
        let stage = files::UnpublishedStage::new(plan.output_root.parent())?;
        let empty = OperatorKeys::default();
        let mut outputs = Vec::with_capacity(plan.outputs.len());
        let mut completed = Vec::with_capacity(plan.outputs.len());
        for output in &plan.outputs {
            self.backend.check_cancelled()?;
            destination.check()?;
            if output.format == OutputFormat::Mp4 {
                files::create_dir(&stage.path().join(&output.id))?;
            }
            let path = child_output(stage.path(), output);
            let output_keys = keys.outputs.get(&output.id).unwrap_or(&empty);
            let guard = destination::OutputGuard::for_bundle_child(&path, &destination)?;
            let child = command
                .using_plan_destination(guard)
                .with_output_signaling(output_keys.signaling.clone());
            let mut conversion = child
                .convert(ConversionRequest {
                    input: frozen.entrypoint(),
                    output: &path,
                    format: output.format,
                    target: output.target,
                    input_keys: &keys.input,
                    hls_input_keys: &keys.hls_input,
                    output_key: output_keys.output.as_ref(),
                    hls_key_uri: output_keys
                        .output
                        .as_ref()
                        .and_then(|_| output_keys.hls_output_uri()),
                    hls_output_key: output_keys.hls_output.as_ref(),
                })
                .map_err(super::source_error)?;
            self.backend.check_cancelled()?;
            destination.check()?;
            command.check_downloaded()?;
            frozen.check_original().map_err(super::source_error)?;
            let entrypoint = result_entrypoint(stage.path(), output);
            let bound = FrozenSource::capture(&entrypoint, &self.backend.stop)?;
            let (matches, verification) =
                command.bundle_media(source, bound.entrypoint(), output, output_keys)?;
            if !matches || !verification.passed {
                return Err(ConversionError("verification-mismatch"));
            }
            conversion.verification = verification;
            let signaling = command.bundle_signaling(bound.entrypoint(), output, output_keys)?;
            let conformance = command.bundle_conformance(bound.entrypoint(), output, tools)?;
            bound.check_original().map_err(output_error)?;
            completed.push(bound);
            outputs.push(BundleConversionOutput {
                id: output.id.clone(),
                conversion,
                signaling,
                conformance,
            });
        }
        self.backend.check_cancelled()?;
        frozen.check_original().map_err(super::source_error)?;
        command
            .using_plan_destination(destination)
            .publish_directory_checked(&stage, &plan.output_root, || {
                for result in &completed {
                    result.check_original().map_err(output_error)?;
                }
                Ok(())
            })?;
        stage.finish_publication();
        Ok(BundleConversionReport {
            kind: "bundle-conversion",
            schema_version: 1,
            plan_id: plan.plan_id.clone(),
            outputs,
        })
    }

    /// Reopen every current output and compare it with one freshly byte-bound original graph.
    pub fn verify_bundle_plan(
        &self,
        plan: &BundlePlan,
        keys: &BundleOperatorKeys,
        tools: &ManifestValidationTools,
    ) -> Result<BundleVerificationReport, ConversionError> {
        validate(plan)?;
        validate_keys(plan, keys)?;
        self.backend.check_cancelled()?;
        self.refuse_bundle_signaling()?;
        let original = FrozenSource::from_inventory(&plan.source, &self.backend.stop)
            .map_err(super::source_error)?;
        let command = self.using_frozen_source(&original);
        let source = VerificationSource {
            input: original.entrypoint(),
            input_keys: &keys.input,
            hls_keys: &keys.hls_input,
        };
        let empty = OperatorKeys::default();
        let frozen_results = plan
            .outputs
            .iter()
            .map(|intent| {
                FrozenSource::capture(
                    &result_entrypoint(&plan.output_root, intent),
                    &self.backend.stop,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut results = Vec::with_capacity(plan.outputs.len());
        for (intent, result) in plan.outputs.iter().zip(&frozen_results) {
            self.backend.check_cancelled()?;
            let output_keys = keys.outputs.get(&intent.id).unwrap_or(&empty);
            let (output_matches_plan, verification) =
                command.bundle_media(source, result.entrypoint(), intent, output_keys)?;
            let signaling = command.bundle_signaling(result.entrypoint(), intent, output_keys)?;
            let conformance = command.bundle_conformance(result.entrypoint(), intent, tools)?;
            original.check_original().map_err(super::source_error)?;
            result.check_original().map_err(output_error)?;
            results.push(BundleVerificationOutput {
                id: intent.id.clone(),
                output_matches_plan,
                verification,
                signaling,
                conformance,
            });
        }
        original.check_original().map_err(super::source_error)?;
        for result in &frozen_results {
            result.check_original().map_err(output_error)?;
        }
        let passed = results.iter().all(|result| {
            result.output_matches_plan
                && result.verification.passed
                && (result.signaling.systems.is_empty() || result.signaling.verified)
                && result
                    .conformance
                    .as_ref()
                    .is_none_or(|report| report.passed)
        });
        Ok(BundleVerificationReport {
            kind: "bundle-verification",
            schema_version: 1,
            plan_id: plan.plan_id.clone(),
            source_unchanged: true,
            passed,
            outputs: results,
        })
    }

    fn refuse_bundle_signaling(&self) -> Result<(), ConversionError> {
        if self.output_signaling.is_some() {
            Err(ConversionError("output-signaling-profile-conflict"))
        } else {
            Ok(())
        }
    }

    fn bundle_media(
        &self,
        source: VerificationSource<'_>,
        entrypoint: &Path,
        intent: &BundleOutputIntent,
        keys: &OperatorKeys,
    ) -> Result<(bool, VerificationReport), ConversionError> {
        let media_keys = keys.output_verification_keys()?;
        let hls_keys = keys.output_hls_verification_keys()?;
        let output = VerificationSource {
            input: entrypoint,
            input_keys: &media_keys,
            hls_keys: &hls_keys,
        };
        let qualified = self.validate_source(output, SourceValidationPurpose::Verification)?;
        let matches = qualified.actual_format == intent.format
            && !qualified.track_protection.is_empty()
            && qualified
                .track_protection
                .iter()
                .all(|value| value.matches_target(intent.target));
        let verification = self.verify(VerificationRequest { source, output })?;
        Ok((matches, verification))
    }

    fn bundle_signaling(
        &self,
        entrypoint: &Path,
        intent: &BundleOutputIntent,
        keys: &OperatorKeys,
    ) -> Result<DrmSignalingReport, ConversionError> {
        self.verify_output_signaling(entrypoint, intent.format, intent.target, keys)?;
        Ok(DrmSignalingReport {
            systems: intent.drms.clone(),
            verified: !intent.drms.is_empty(),
        })
    }

    fn bundle_conformance(
        &self,
        entrypoint: &Path,
        intent: &BundleOutputIntent,
        tools: &ManifestValidationTools,
    ) -> Result<Option<ManifestValidationReport>, ConversionError> {
        if intent.conformance == ManifestValidation::None {
            return Ok(None);
        }
        let report = self.validate_manifest(entrypoint, intent.format, tools)?;
        if !report.passed {
            return Err(ConversionError("manifest-conformance-failed"));
        }
        Ok(Some(report))
    }
}

/// Read a public, non-secret output recipe without accepting commands or arbitrary paths.
pub fn read_output_spec(path: &Path) -> Result<Vec<BundleOutputIntent>, ConversionError> {
    const ERROR: ConversionError = ConversionError("output-spec-invalid");
    const SPEC_LIMIT: u64 = 65_536;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Specification {
        schema_version: u32,
        outputs: Vec<BundleOutputIntent>,
    }
    let file = files::regular_input(path).map_err(|_| ERROR)?;
    let mut bytes = Vec::new();
    file.take(SPEC_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ERROR)?;
    if bytes.len() as u64 > SPEC_LIMIT {
        return Err(ERROR);
    }
    let spec: Specification = serde_json::from_slice(&bytes).map_err(|_| ERROR)?;
    if spec.schema_version != 1 {
        return Err(ERROR);
    }
    normalized(&spec.outputs).map_err(|_| ERROR)
}

fn output_error(error: ConversionError) -> ConversionError {
    if error.0 == "cancelled" {
        error
    } else {
        ConversionError("verification-output-changed")
    }
}

fn child_output(root: &Path, intent: &BundleOutputIntent) -> PathBuf {
    if intent.format == OutputFormat::Mp4 {
        root.join(&intent.id).join("media.mp4")
    } else {
        root.join(&intent.id)
    }
}

fn result_entrypoint(root: &Path, intent: &BundleOutputIntent) -> PathBuf {
    let output = child_output(root, intent);
    match intent.format {
        OutputFormat::Mp4 => output,
        OutputFormat::Dash => output.join("manifest.mpd"),
        OutputFormat::Hls | OutputFormat::HlsTs => output.join("master.m3u8"),
    }
}

fn normalized(outputs: &[BundleOutputIntent]) -> Result<Vec<BundleOutputIntent>, ConversionError> {
    if outputs.is_empty()
        || outputs.len() > MAX_OUTPUTS
        || outputs
            .iter()
            .any(|output| output.id.len() > 32 || output.drms.len() > 3)
    {
        return Err(INVALID);
    }
    let mut outputs = outputs.to_vec();
    for output in &mut outputs {
        output.drms.sort();
    }
    validate_outputs(&outputs)?;
    Ok(outputs)
}

fn validate_outputs(outputs: &[BundleOutputIntent]) -> Result<(), ConversionError> {
    if outputs.is_empty() || outputs.len() > MAX_OUTPUTS {
        return Err(INVALID);
    }
    let mut ids = BTreeSet::new();
    for output in outputs {
        if !valid_id(&output.id)
            || !ids.insert(&output.id)
            || output.drms.len() > 3
            || !super::supported(output.format, output.target)
            || output.drms.windows(2).any(|pair| pair[0] >= pair[1])
            || (output.conformance == ManifestValidation::Required
                && output.format == OutputFormat::Mp4)
            || !drm_profile(output)
        {
            return Err(INVALID);
        }
    }
    Ok(())
}

fn valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    let reserved = matches!(id, "con" | "prn" | "aux" | "nul")
        || ["com", "lpt"].iter().any(|prefix| {
            id.strip_prefix(prefix)
                .is_some_and(|tail| tail.len() == 1 && matches!(tail.as_bytes()[0], b'1'..=b'9'))
        });
    !reserved
        && !bytes.is_empty()
        && bytes.len() <= 32
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn drm_profile(output: &BundleOutputIntent) -> bool {
    if output.drms.is_empty() {
        return true;
    }
    match (output.format, output.target) {
        (OutputFormat::Dash, TargetProtection::Cbcs) => output
            .drms
            .iter()
            .all(|value| matches!(value, DrmSystem::Widevine | DrmSystem::PlayReady)),
        (OutputFormat::Hls, TargetProtection::Cbcs)
        | (OutputFormat::HlsTs, TargetProtection::HlsSampleAes) => {
            output.drms == [DrmSystem::FairPlay]
        }
        _ => false,
    }
}

fn validate_keys(plan: &BundlePlan, keys: &BundleOperatorKeys) -> Result<(), ConversionError> {
    if keys.outputs.iter().any(|(id, keys)| {
        !plan
            .outputs
            .iter()
            .any(|output| &output.id == id && output.target != TargetProtection::Clear)
            || !keys.input.keys.is_empty()
            || !keys.hls_input.0.is_empty()
    }) {
        return Err(KEYS);
    }
    for output in &plan.outputs {
        let configured = keys.outputs.get(&output.id);
        if output.target != TargetProtection::Clear && configured.is_none() {
            return Err(KEYS);
        }
        let actual = configured
            .and_then(|keys| keys.signaling.as_ref())
            .map(OutputSignaling::systems)
            .unwrap_or_default();
        if actual != output.drms {
            return Err(DRM);
        }
    }
    Ok(())
}

#[derive(Serialize)]
struct Identity<'a> {
    kind: &'a str,
    schema_version: u32,
    source: &'a SourceInventory,
    source_format: OutputFormat,
    output_root: &'a Path,
    outputs: &'a [BundleOutputIntent],
}

fn identity(plan: &BundlePlan) -> Result<String, ConversionError> {
    let bytes = serde_json::to_vec(&Identity {
        kind: &plan.kind,
        schema_version: plan.schema_version,
        source: &plan.source,
        source_format: plan.source_format,
        output_root: &plan.output_root,
        outputs: &plan.outputs,
    })
    .map_err(|_| INVALID)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

pub(super) fn validate(plan: &BundlePlan) -> Result<(), ConversionError> {
    if plan.kind != KIND
        || plan.schema_version != 2
        || plan.plan_id.len() != 64
        || !plan
            .plan_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || !absolute(&plan.output_root)
    {
        return Err(INVALID);
    }
    frozen::validate(&plan.source).map_err(|_| INVALID)?;
    validate_outputs(&plan.outputs)?;
    if identity(plan)? != plan.plan_id {
        return Err(INVALID);
    }
    check_output_location(plan)
}

fn check_output_location(plan: &BundlePlan) -> Result<(), ConversionError> {
    if (plan.source_format != OutputFormat::Mp4 && plan.output_root.starts_with(&plan.source.root))
        || plan.source.files.iter().any(|file| {
            let source = plan.source.root.join(&file.path);
            source.starts_with(&plan.output_root) || plan.output_root.starts_with(source)
        })
    {
        return Err(UNSAFE);
    }
    Ok(())
}

pub(super) fn write_new(path: &Path, plan: &BundlePlan) -> Result<(), ConversionError> {
    validate(plan)?;
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(EXISTS);
    }
    let path = super::super::destination(path).map_err(|_| UNSAFE)?;
    if path.starts_with(&plan.output_root)
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

#[cfg(test)]
mod tests;
