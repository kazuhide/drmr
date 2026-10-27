//! Each output owns its destination, signaling and evidence until the bundle commits once.
use super::{
    BundleConversionOutput, BundleOperatorKeys, BundlePlan, BundleVerificationOutput,
    ConversionError, Converter, ManifestValidationTools, OperatorKeys, OutputFormat,
    VerificationSource, child_output, destination, files, output_error, result_entrypoint,
};
use crate::conversion::manifest_lint::{ManifestDemand, ManifestSizing};
use crate::conversion::{ConversionRequest, FrozenSource};

type ManifestEntry = Result<Option<ManifestDemand>, ConversionError>;

pub(super) struct Conversion<'a> {
    pub owner: &'a Converter,
    pub command: &'a Converter,
    pub plan: &'a BundlePlan,
    pub keys: &'a BundleOperatorKeys,
    pub tools: &'a ManifestValidationTools,
    pub destination: &'a destination::OutputGuard,
    pub frozen: &'a FrozenSource,
    pub qualified: &'a super::SourceQualification,
    pub shared: Option<&'a crate::conversion::presentation::BundlePrepared<'a>>,
    pub stage: &'a files::UnpublishedStage,
}

impl Conversion<'_> {
    fn admission(&self) -> crate::conversion::presentation::BundleAdmission<'_> {
        crate::conversion::presentation::BundleAdmission {
            source: VerificationSource {
                input: self.frozen.entrypoint(),
                input_keys: &self.keys.input,
                hls_keys: &self.keys.hls_input,
            },
            inventory: self.frozen.inventory(),
            qualified: self.qualified,
            processing: super::source_processing(
                self.plan
                    .outputs
                    .iter()
                    .map(|output| self.plan.processing_mode(&output.id)),
            ),
            depth: self.command.backend.validation_depth,
        }
    }

    pub fn run(
        &self,
        ordinal: usize,
    ) -> Result<(FrozenSource, BundleConversionOutput), ConversionError> {
        let Self {
            owner,
            command,
            plan,
            keys,
            tools,
            destination,
            frozen,
            qualified: _,
            shared,
            stage,
        } = *self;
        let output = &plan.outputs[ordinal];
        let (command, output_scope) =
            command.pipeline_output(ordinal, plan.outputs.len(), output.format);
        owner.backend.check_cancelled()?;
        destination.check()?;
        if output.format == OutputFormat::Mp4 {
            files::create_dir(&stage.path().join(&output.id))?;
        }
        let path = child_output(stage.path(), output);
        let empty = OperatorKeys::default();
        let output_keys = keys.outputs.get(&output.id).unwrap_or(&empty);
        let guard = destination::OutputGuard::for_bundle_child(&path, destination)?;
        let child = command
            .using_plan_destination(guard)
            .with_plan_processing(
                plan.processing_selection(&output.id),
                plan.schema_version >= 8,
            )
            .using_output_options(plan.output_options(&output.id))
            .with_output_signaling(output_keys.signaling.clone());
        let admitted = self.admission();
        let mut conversion = child
            .convert_bundle_admitted(
                ConversionRequest {
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
                },
                &admitted,
                shared,
            )
            .map_err(super::super::source_error)?;
        owner.backend.check_cancelled()?;
        destination.check()?;
        command.check_downloaded()?;
        frozen
            .check_original()
            .map_err(super::super::source_error)?;
        let entrypoint = result_entrypoint(stage.path(), output);
        let bound = command.capture_source(&entrypoint)?;
        let source = VerificationSource {
            input: frozen.entrypoint(),
            input_keys: &keys.input,
            hls_keys: &keys.hls_input,
        };
        let (matches, verification) =
            child.bundle_media(source, bound.entrypoint(), output, output_keys)?;
        if plan.output_options.is_some() {
            child.verify_selected_output_options(
                bound.entrypoint(),
                output.format,
                plan.processing_mode(&output.id),
            )?;
        }
        if !matches || !verification.passed {
            return Err(ConversionError("verification-mismatch"));
        }
        conversion.verification = verification;
        let signaling = command.bundle_signaling(bound.entrypoint(), output, output_keys)?;
        let conformance = command.bundle_conformance(bound.entrypoint(), output, tools)?;
        bound.check_original().map_err(output_error)?;
        output_scope.finish();
        Ok((
            bound,
            BundleConversionOutput {
                id: output.id.clone(),
                conversion,
                signaling,
                conformance,
            },
        ))
    }
}

pub(super) struct Verification<'a> {
    pub command: &'a Converter,
    pub plan: &'a BundlePlan,
    pub keys: &'a BundleOperatorKeys,
    pub tools: &'a ManifestValidationTools,
    pub original: &'a FrozenSource,
    pub manifests: &'a ManifestDemands,
}

pub(super) struct ManifestDemands {
    manifests: Vec<ManifestEntry>,
}

impl ManifestDemands {
    pub fn capture(command: &Converter, plan: &BundlePlan) -> Result<Self, ConversionError> {
        let mut manifests = Vec::with_capacity(plan.outputs.len());
        for output in &plan.outputs {
            command.backend.check_cancelled()?;
            // An output's preflight failure belongs to its ordered pipeline.
            // It must not hide an earlier output's later media failure.
            manifests.push(command.manifest_demand(&result_entrypoint(&plan.output_root, output)));
        }
        Ok(Self { manifests })
    }

    pub fn minimum_bytes(&self) -> u64 {
        self.manifests
            .iter()
            .filter_map(|manifest| manifest.as_ref().ok())
            .flatten()
            .map(|manifest| manifest.minimum_bytes)
            .max()
            .unwrap_or(0)
    }

    pub fn metadata_bytes(&self) -> u64 {
        self.manifests
            .iter()
            .filter_map(|manifest| manifest.as_ref().ok())
            .flatten()
            .map(|manifest| manifest.metadata_bytes)
            .sum::<u64>()
            + (self.manifests.capacity() * 4 * std::mem::size_of::<ManifestEntry>()) as u64
    }

    pub fn check_preflight(&self, ordinal: usize) -> Result<(), ConversionError> {
        self.manifests[ordinal].map(|_| ())
    }

    pub fn bind(
        &self,
        command: &Converter,
        source: &FrozenSource,
        ordinal: usize,
    ) -> Result<Option<ManifestSizing>, ConversionError> {
        command.bind_manifest_demand(source, self.manifests[ordinal]?)
    }
}

impl Verification<'_> {
    pub fn run(
        &self,
        ordinal: usize,
    ) -> Result<(FrozenSource, BundleVerificationOutput), ConversionError> {
        let Self {
            command,
            plan,
            keys,
            tools,
            original,
            manifests,
        } = *self;
        let intent = &plan.outputs[ordinal];
        command.backend.check_cancelled()?;
        manifests.check_preflight(ordinal)?;
        let result = command.capture_source(&result_entrypoint(&plan.output_root, intent))?;
        result.check_original().map_err(output_error)?;
        let manifest = manifests.bind(command, &result, ordinal)?;
        let empty = OperatorKeys::default();
        let output_keys = keys.outputs.get(&intent.id).unwrap_or(&empty);
        let child = command
            .with_processing_clone(command.processing_mode)
            .with_plan_processing(
                plan.processing_selection(&intent.id),
                plan.schema_version >= 8,
            );
        let child = child.using_output_options(plan.output_options(&intent.id));
        let source = VerificationSource {
            input: original.entrypoint(),
            input_keys: &keys.input,
            hls_keys: &keys.hls_input,
        };
        let (output_matches_plan, verification) =
            child.bundle_media(source, result.entrypoint(), intent, output_keys)?;
        if plan.output_options.is_some() {
            child.verify_selected_output_options(
                result.entrypoint(),
                intent.format,
                plan.processing_mode(&intent.id),
            )?;
        }
        let signaling = command.bundle_signaling(result.entrypoint(), intent, output_keys)?;
        let conformance = command.bundle_conformance(result.entrypoint(), intent, tools)?;
        original
            .check_original()
            .map_err(super::super::source_error)?;
        result.check_original().map_err(output_error)?;
        if let Some(manifest) = manifest {
            manifest.check(command)?;
        }
        Ok((
            result,
            BundleVerificationOutput {
                id: intent.id.clone(),
                output_matches_plan,
                verification,
                signaling,
                conformance,
            },
        ))
    }
}
