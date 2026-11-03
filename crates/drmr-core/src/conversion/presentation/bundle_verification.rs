//! Reuse the bound original; independently prepare and check every final output.
use super::{
    BundlePrepared, ConversionError, Converter, SourceValidationPurpose, TargetProtection,
    VerificationReport, VerificationRequest, prepare_presentation, qualified_layout_tracks,
    workspace,
};
use crate::{conversion::files, inspection::OutputFormat};

impl Converter {
    pub(in crate::conversion) fn verify_prepared_bundle_output(
        &self,
        request: VerificationRequest<'_>,
        expected: (OutputFormat, TargetProtection),
        shared: &BundlePrepared<'_>,
    ) -> Result<(bool, VerificationReport), ConversionError> {
        let source = shared.verification_tracks(self, request.source)?;
        let mut command = self.with_processing_clone(self.processing_mode);
        command.output_manifest.checking_final_result = true;
        let mut command =
            command.qualification_command(request.output, SourceValidationPurpose::Verification)?;
        let work = workspace()?;
        let result_dir = work.path().join("result");
        files::create_dir(&result_dir)?;
        let preparation = command
            .backend
            .scope(crate::conversion::measurement::Phase::InputPreparation);
        let (backend, result_preparation) = crate::conversion::media_progress::scoped(
            &command.backend,
            crate::conversion::media_progress::Subject::Result,
            crate::conversion::media_progress::Purpose::Verification,
        );
        command.backend = backend;
        let original = command.inspect_input(request.output.input)?;
        let result = prepare_presentation(
            &request.output,
            &result_dir,
            &command.backend,
            command.maximum_workers,
            SourceValidationPurpose::Verification,
        )?;
        let qualified = command.qualify_prepared(
            request.output,
            SourceValidationPurpose::Verification,
            original,
            &result,
        )?;
        let qualified =
            command.finish_qualification(qualified, SourceValidationPurpose::Verification);
        result_preparation.finish();
        command.backend.check_cancelled()?;
        let matches = qualified.actual_format == expected.0
            && !qualified.track_protection.is_empty()
            && qualified
                .track_protection
                .iter()
                .all(|value| value.matches_target(expected.1));
        let source_format = command.inspect_input(request.source.input)?.format;
        let output_format = command.default_output_format(request.output.input)?;
        command.check_admitted_output_options(
            request.source.input,
            source_format,
            &qualified_layout_tracks(&source, source_format),
            output_format,
            crate::conversion::ProcessingMode::Stitch,
        )?;
        let checked = command.checked_output_manifest(request.output.input)?;
        drop(preparation);
        let mut report = command.compare_presentations(
            source,
            result,
            request.source.input_keys,
            request.output.input_keys,
            output_format,
        )?;
        if let Some(checked) = checked {
            checked.check(&command)?;
            checked.apply_to(&mut report);
        }
        command.backend.checkpoint(&[&work]);
        Ok((matches, report))
    }
}
