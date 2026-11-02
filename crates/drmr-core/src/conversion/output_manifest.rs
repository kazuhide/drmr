//! Output authoring and external checks never change admission of the original source.
use super::{
    ConversionError, Converter, VerificationReport, VerificationRequest,
    conformance::ManifestValidationTools,
    manifest_lint::{CheckedManifest, ManifestProfile},
    plan::bundle::{BundleOutputIntent, ManifestValidation},
};
use crate::inspection::OutputFormat;
use std::{path::Path, sync::Arc};

#[derive(Clone, Default)]
pub(super) struct State {
    pub profile: Option<ManifestProfile>,
    pub external: Option<Arc<ManifestValidationTools>>,
    pub checking_final_result: bool,
}

impl Converter {
    /// Select authoring rules for final output manifests, independently of source admission.
    #[must_use]
    pub fn with_manifest_profile(mut self, profile: ManifestProfile) -> Self {
        self.output_manifest.profile = Some(profile);
        self
    }

    /// Require an external check of final manifests with explicit runtime tool selections.
    #[must_use]
    pub fn with_external_manifest_check(mut self, tools: ManifestValidationTools) -> Self {
        self.output_manifest.external = Some(Arc::new(tools));
        self
    }

    /// Compare media and apply output checks in the existing byte-bound verification pipeline.
    pub fn verify_with_output_checks(
        &self,
        request: VerificationRequest<'_>,
    ) -> Result<VerificationReport, ConversionError> {
        let mut command = self.with_processing_clone(self.processing_mode);
        command.output_manifest.checking_final_result = true;
        command.verify(request)
    }

    pub(super) fn validate_output_manifest_policy(
        &self,
        format: OutputFormat,
    ) -> Result<(), ConversionError> {
        if format == OutputFormat::Mp4
            && (self.output_manifest.profile.is_some() || self.output_manifest.external.is_some())
        {
            return Err(ConversionError("manifest-validation-format-unsupported"));
        }
        if self.output_manifest.profile == Some(ManifestProfile::AppleVod)
            && !matches!(format, OutputFormat::Hls | OutputFormat::HlsTs)
        {
            return Err(ConversionError("manifest-lint-profile-unsupported"));
        }
        Ok(())
    }

    pub(super) fn checked_publication_manifest(
        &self,
        path: &Path,
        format: OutputFormat,
    ) -> Result<CheckedManifest, ConversionError> {
        self.checked_policy_manifest(path, format, true)
    }

    fn checked_policy_manifest(
        &self,
        path: &Path,
        format: OutputFormat,
        publication: bool,
    ) -> Result<CheckedManifest, ConversionError> {
        self.validate_output_manifest_policy(format)?;
        let mut checked = self.checked_manifest_with_profile(
            path,
            format,
            self.output_manifest.profile.unwrap_or_default(),
        )?;
        if publication {
            checked.require_clean()?;
        }
        if checked.report.is_clean_for_scope()
            && let Some(tools) = &self.output_manifest.external
        {
            let external = self.validate_manifest(path, format, tools)?;
            if !external.passed {
                return Err(ConversionError("manifest-conformance-failed"));
            }
            checked.external_manifest = Some(external);
        }
        checked.check(self)?;
        Ok(checked)
    }

    pub(super) fn checked_verification_manifest(
        &self,
        path: &Path,
        format: OutputFormat,
    ) -> Result<CheckedManifest, ConversionError> {
        if self.output_manifest.checking_final_result {
            self.checked_policy_manifest(path, format, false)
        } else {
            self.checked_manifest(path, format)
        }
    }

    pub(super) fn using_saved_manifest_policy(
        &self,
        profile: Option<ManifestProfile>,
        external_required: bool,
    ) -> Result<Self, ConversionError> {
        if self
            .output_manifest
            .profile
            .is_some_and(|requested| requested != profile.unwrap_or_default())
            || self.output_manifest.external.is_some() && !external_required
        {
            return Err(ConversionError(
                "manifest-policy-plan-override-not-supported",
            ));
        }
        if external_required && self.output_manifest.external.is_none() {
            return Err(ConversionError("external-validator-required"));
        }
        let mut command = self.with_processing_clone(self.processing_mode);
        command.output_manifest.profile = profile;
        if !external_required {
            command.output_manifest.external = None;
        }
        Ok(command)
    }

    pub(super) fn using_bundle_output_policy(&self, intent: &BundleOutputIntent) -> Self {
        let mut command = self.with_processing_clone(self.processing_mode);
        command.output_manifest.profile = intent.manifest_profile;
        // The outer bundle owns the one final external check and its report.
        command.output_manifest.external = None;
        command.output_manifest.checking_final_result = false;
        command
    }

    pub(super) fn resolve_bundle_output_policy(&self, outputs: &mut [BundleOutputIntent]) {
        for output in outputs {
            if let Some(profile) = self.output_manifest.profile {
                output.manifest_profile = Some(profile);
            }
            if self.output_manifest.external.is_some() {
                output.conformance = ManifestValidation::Required;
            }
        }
    }

    pub(super) fn check_saved_bundle_manifest_policy(
        &self,
        outputs: &[BundleOutputIntent],
    ) -> Result<(), ConversionError> {
        if outputs.iter().any(|output| {
            self.output_manifest
                .profile
                .is_some_and(|requested| requested != output.manifest_profile.unwrap_or_default())
                || self.output_manifest.external.is_some()
                    && output.conformance != ManifestValidation::Required
        }) {
            return Err(ConversionError(
                "manifest-policy-plan-override-not-supported",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        events::NoSink,
        toolrun::{Secrets, ToolRunner},
    };

    fn converter() -> Converter {
        Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
            "unused",
            "unused",
        )
    }
    fn manifest() -> (crate::native::PrivateTempDir, std::path::PathBuf) {
        let root = crate::native::private_tempdir().unwrap();
        let path = root.path().join("media.m3u8");
        std::fs::write(
            &path,
            b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\npart.ts\n",
        )
        .unwrap();
        (root, path)
    }
    #[test]
    fn output_authoring_never_changes_original_base_lint_and_final_checks_keep_scope() {
        let (_root, path) = manifest();
        let command = converter().with_manifest_profile(ManifestProfile::AppleVod);
        assert!(
            command
                .lint_manifest(&path, OutputFormat::Hls)
                .unwrap()
                .is_clean_for_scope()
        );
        assert_eq!(
            command
                .checked_publication_manifest(&path, OutputFormat::Hls)
                .err(),
            Some(ConversionError("native-manifest-lint-failed"))
        );
        let mut active = command.with_processing_clone(command.processing_mode);
        active.output_manifest.checking_final_result = true;
        assert!(
            active
                .checked_verification_manifest(&path, OutputFormat::Hls)
                .unwrap()
                .report
                .errors
                > 0
        );
    }
    #[test]
    fn external_tools_are_requested_only_for_final_checks_and_missing_tools_never_pass() {
        let (_root, path) = manifest();
        let baseline = converter()
            .checked_publication_manifest(&path, OutputFormat::Hls)
            .unwrap();
        assert!(baseline.external_manifest.is_none());
        let command = converter().with_external_manifest_check(ManifestValidationTools::default());
        assert!(
            command
                .lint_manifest(&path, OutputFormat::Hls)
                .unwrap()
                .is_clean_for_scope()
        );
        assert_eq!(
            command
                .checked_publication_manifest(&path, OutputFormat::Hls)
                .err(),
            Some(ConversionError("manifest-validator-not-configured"))
        );
    }
    #[test]
    fn unsupported_formats_and_saved_policy_overrides_are_refused() {
        let command = converter().with_manifest_profile(ManifestProfile::AppleVod);
        assert!(
            command
                .validate_output_manifest_policy(OutputFormat::Dash)
                .is_err()
        );
        assert!(
            command
                .validate_output_manifest_policy(OutputFormat::Mp4)
                .is_err()
        );
        assert!(
            command
                .using_saved_manifest_policy(Some(ManifestProfile::Base), false)
                .is_err()
        );
        assert!(
            converter()
                .using_saved_manifest_policy(Some(ManifestProfile::Base), true)
                .is_err()
        );
        assert!(
            converter()
                .with_external_manifest_check(ManifestValidationTools::default())
                .using_saved_manifest_policy(Some(ManifestProfile::Base), false)
                .is_err()
        );
    }
}
