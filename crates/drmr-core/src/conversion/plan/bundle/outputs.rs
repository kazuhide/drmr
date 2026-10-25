//! Each output owns its destination, signaling and evidence until the bundle commits once.
use super::{
    BundleConversionOutput, BundleOperatorKeys, BundlePlan, BundleVerificationOutput,
    ConversionError, Converter, ManifestValidationTools, OperatorKeys, OutputFormat,
    VerificationSource, child_output, destination, files, output_error, result_entrypoint,
};
use crate::conversion::{ConversionRequest, FrozenSource};

pub(super) struct Conversion<'a> {
    pub owner: &'a Converter,
    pub command: &'a Converter,
    pub plan: &'a BundlePlan,
    pub keys: &'a BundleOperatorKeys,
    pub tools: &'a ManifestValidationTools,
    pub destination: &'a destination::OutputGuard,
    pub frozen: &'a FrozenSource,
    pub stage: &'a files::UnpublishedStage,
}

impl Conversion<'_> {
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
            stage,
        } = *self;
        let output = &plan.outputs[ordinal];
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
            .with_processing(plan.processing_mode(&output.id))
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
            .map_err(super::super::source_error)?;
        owner.backend.check_cancelled()?;
        destination.check()?;
        command.check_downloaded()?;
        frozen
            .check_original()
            .map_err(super::super::source_error)?;
        let entrypoint = result_entrypoint(stage.path(), output);
        let bound = owner.capture_source(&entrypoint)?;
        let source = VerificationSource {
            input: frozen.entrypoint(),
            input_keys: &keys.input,
            hls_keys: &keys.hls_input,
        };
        let (matches, verification) =
            child.bundle_media(source, bound.entrypoint(), output, output_keys)?;
        if !matches || !verification.passed {
            return Err(ConversionError("verification-mismatch"));
        }
        conversion.verification = verification;
        let signaling = command.bundle_signaling(bound.entrypoint(), output, output_keys)?;
        let conformance = command.bundle_conformance(bound.entrypoint(), output, tools)?;
        bound.check_original().map_err(output_error)?;
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
    pub owner: &'a Converter,
    pub command: &'a Converter,
    pub plan: &'a BundlePlan,
    pub keys: &'a BundleOperatorKeys,
    pub tools: &'a ManifestValidationTools,
    pub original: &'a FrozenSource,
}

impl Verification<'_> {
    pub fn run(
        &self,
        ordinal: usize,
    ) -> Result<(FrozenSource, BundleVerificationOutput), ConversionError> {
        let Self {
            owner,
            command,
            plan,
            keys,
            tools,
            original,
        } = *self;
        let intent = &plan.outputs[ordinal];
        owner.backend.check_cancelled()?;
        let result = owner.capture_source(&result_entrypoint(&plan.output_root, intent))?;
        let empty = OperatorKeys::default();
        let output_keys = keys.outputs.get(&intent.id).unwrap_or(&empty);
        let child = command.with_processing_clone(plan.processing_mode(&intent.id));
        let source = VerificationSource {
            input: original.entrypoint(),
            input_keys: &keys.input,
            hls_keys: &keys.hls_input,
        };
        let (output_matches_plan, verification) =
            child.bundle_media(source, result.entrypoint(), intent, output_keys)?;
        let signaling = command.bundle_signaling(result.entrypoint(), intent, output_keys)?;
        let conformance = command.bundle_conformance(result.entrypoint(), intent, tools)?;
        original
            .check_original()
            .map_err(super::super::source_error)?;
        result.check_original().map_err(output_error)?;
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
