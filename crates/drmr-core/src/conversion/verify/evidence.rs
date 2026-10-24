//! Required verification depth and evidence actually obtained for one operation.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ValidationDepth {
    #[default]
    Native,
    Decoded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationScope {
    SourceAdmission,
    Comparison,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DecodeNotCheckedReason {
    NotRequested,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum DecodeEvidence {
    NotChecked { reason: DecodeNotCheckedReason },
    SourceDecoded,
    ContentCompared { equal: bool },
}
impl DecodeEvidence {
    pub const fn not_requested() -> Self {
        Self::NotChecked {
            reason: DecodeNotCheckedReason::NotRequested,
        }
    }

    pub const fn satisfies(self, depth: ValidationDepth, scope: VerificationScope) -> bool {
        match (self, scope) {
            (Self::NotChecked { .. }, _) => matches!(depth, ValidationDepth::Native),
            (Self::SourceDecoded, VerificationScope::SourceAdmission) => true,
            (Self::ContentCompared { equal }, VerificationScope::Comparison) => equal,
            _ => false,
        }
    }

    pub const fn completed(self) -> bool {
        !matches!(self, Self::NotChecked { .. })
    }

    const fn has_scope(self, scope: VerificationScope) -> bool {
        matches!(
            (self, scope),
            (Self::NotChecked { .. }, _)
                | (Self::SourceDecoded, VerificationScope::SourceAdmission)
                | (Self::ContentCompared { .. }, VerificationScope::Comparison)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VerificationFailure {
    ChildCheckFailed,
    EvidenceScopeMismatch,
    NoTracks,
}

/// An executed native check, distinct from unrequested decode work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckOutcome {
    Pass,
    Fail,
}
impl CheckOutcome {
    pub const fn is_pass(self) -> bool {
        matches!(self, Self::Pass)
    }
    pub(in crate::conversion) fn include(&mut self, other: Self) {
        if other == Self::Fail {
            *self = Self::Fail;
        }
    }
}

/// All three checks are required, including every packet clock and presentation window.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationChecks {
    pub stream_metadata: CheckOutcome,
    pub packet_payloads_equal: bool,
    pub packet_timing_equal: bool,
}
impl VerificationChecks {
    pub const fn is_pass(&self) -> bool {
        self.stream_metadata.is_pass() && self.packet_payloads_equal && self.packet_timing_equal
    }
}

/// Native correspondence and optional complete decode evidence, without a playback claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerificationReport {
    pub schema_version: u32,
    pub required_depth: ValidationDepth,
    pub scope: VerificationScope,
    pub passed: bool,
    pub checks: VerificationChecks,
    pub decode_evidence: DecodeEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<VerificationFailure>,
    pub license_playback_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_manifest: Option<super::super::manifest_lint::NativeManifestReport>,
}
impl VerificationReport {
    pub fn new(
        required_depth: ValidationDepth,
        scope: VerificationScope,
        checks: VerificationChecks,
        decode_evidence: DecodeEvidence,
    ) -> Self {
        let mut report = Self {
            schema_version: 2,
            required_depth,
            scope,
            passed: false,
            checks,
            decode_evidence,
            failure: (!decode_evidence.has_scope(scope))
                .then_some(VerificationFailure::EvidenceScopeMismatch),
            license_playback_verified: false,
            native_manifest: None,
        };
        report.refresh_passed();
        report
    }

    pub fn refresh_passed(&mut self) {
        self.passed = self.failure.is_none()
            && self.checks.is_pass()
            && self
                .decode_evidence
                .satisfies(self.required_depth, self.scope)
            && self.native_manifest.as_ref().is_none_or(|manifest| {
                manifest.errors == 0 && !manifest.truncated && !manifest.incomplete
            });
    }

    /// Complete decoding adds evidence to the mandatory native checks.
    pub(crate) fn require_native(
        &mut self,
        native: &Self,
    ) -> Result<(), super::super::ConversionError> {
        if self.scope != native.scope {
            return Err(super::super::ConversionError(
                "verification-evidence-scope-mismatch",
            ));
        }
        if native.required_depth != ValidationDepth::Native
            || native.decode_evidence != DecodeEvidence::not_requested()
        {
            return Err(super::super::ConversionError(
                "verification-native-evidence-required",
            ));
        }
        let mut checked_native = native.clone();
        checked_native.refresh_passed();
        if (!self.passed || !native.passed || !checked_native.passed) && self.failure.is_none() {
            self.failure = Some(VerificationFailure::ChildCheckFailed);
        }
        self.checks
            .stream_metadata
            .include(native.checks.stream_metadata);
        self.checks.packet_payloads_equal &= native.checks.packet_payloads_equal;
        self.checks.packet_timing_equal &= native.checks.packet_timing_equal;
        self.refresh_passed();
        Ok(())
    }

    pub fn combine(
        required_depth: ValidationDepth,
        scope: VerificationScope,
        reports: impl IntoIterator<Item = Self>,
    ) -> Self {
        let mut checks = VerificationChecks {
            stream_metadata: CheckOutcome::Pass,
            packet_payloads_equal: true,
            packet_timing_equal: true,
        };
        let mut count = 0;
        let mut all_decoded = true;
        let mut decode_mismatch = false;
        let mut failure = None;
        for mut report in reports {
            let reported_passed = report.passed;
            report.refresh_passed();
            count += 1;
            if report.scope != scope {
                failure = Some(VerificationFailure::EvidenceScopeMismatch);
            }
            checks
                .stream_metadata
                .include(report.checks.stream_metadata);
            checks.packet_payloads_equal &= report.checks.packet_payloads_equal;
            checks.packet_timing_equal &= report.checks.packet_timing_equal;
            all_decoded &= matches!(
                (scope, report.decode_evidence),
                (
                    VerificationScope::SourceAdmission,
                    DecodeEvidence::SourceDecoded
                ) | (
                    VerificationScope::Comparison,
                    DecodeEvidence::ContentCompared { .. }
                )
            );
            decode_mismatch |= matches!(
                report.decode_evidence,
                DecodeEvidence::ContentCompared { equal: false }
            );
            if (!reported_passed || !report.passed) && failure.is_none() {
                failure = Some(VerificationFailure::ChildCheckFailed);
            }
        }
        if count == 0 {
            failure = Some(VerificationFailure::NoTracks);
        }
        let decode = if scope == VerificationScope::Comparison && decode_mismatch {
            DecodeEvidence::ContentCompared { equal: false }
        } else if count != 0 && all_decoded {
            match scope {
                VerificationScope::SourceAdmission => DecodeEvidence::SourceDecoded,
                VerificationScope::Comparison => DecodeEvidence::ContentCompared { equal: true },
            }
        } else {
            DecodeEvidence::not_requested()
        };
        let mut report = Self::new(required_depth, scope, checks, decode);
        report.failure = failure.or(report.failure);
        report.refresh_passed();
        report
    }

    /// Admission callers use this only after checking views of the same fixed source.
    pub(in crate::conversion) fn into_source_admission(
        mut self,
    ) -> Result<Self, super::super::ConversionError> {
        if matches!(
            self.decode_evidence,
            DecodeEvidence::ContentCompared { equal: false }
        ) {
            return Err(super::super::ConversionError(
                "source-decode-comparison-failed",
            ));
        }
        if !self.decode_evidence.has_scope(self.scope) {
            return Err(super::super::ConversionError(
                "verification-evidence-scope-mismatch",
            ));
        }
        if !self.passed && self.failure.is_none() {
            self.failure = Some(VerificationFailure::ChildCheckFailed);
        }
        self.scope = VerificationScope::SourceAdmission;
        if self.decode_evidence.completed() {
            self.decode_evidence = DecodeEvidence::SourceDecoded;
        }
        self.refresh_passed();
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checks() -> VerificationChecks {
        VerificationChecks {
            stream_metadata: CheckOutcome::Pass,
            packet_payloads_equal: true,
            packet_timing_equal: true,
        }
    }

    #[test]
    fn unrequested_decode_is_explicit_and_cannot_satisfy_required_decode() {
        for scope in [
            VerificationScope::SourceAdmission,
            VerificationScope::Comparison,
        ] {
            let native = VerificationReport::new(
                ValidationDepth::Native,
                scope,
                checks(),
                DecodeEvidence::not_requested(),
            );
            assert!(native.passed);
            let json = serde_json::to_value(&native).unwrap();
            assert_eq!(json["schema_version"], 2);
            assert_eq!(json["decode_evidence"]["status"], "not-checked");
            assert!(json.get("media_verified").is_none());
            assert!(json["checks"].get("decoded_content_equal").is_none());
            let decoded = VerificationReport::new(
                ValidationDepth::Decoded,
                scope,
                checks(),
                DecodeEvidence::not_requested(),
            );
            assert!(!decoded.passed);
        }
    }

    #[test]
    fn source_decode_and_content_equality_are_not_interchangeable() {
        let source = VerificationReport::new(
            ValidationDepth::Decoded,
            VerificationScope::SourceAdmission,
            checks(),
            DecodeEvidence::SourceDecoded,
        );
        assert!(source.passed);
        let wrong_scope = VerificationReport::new(
            ValidationDepth::Decoded,
            VerificationScope::Comparison,
            checks(),
            DecodeEvidence::SourceDecoded,
        );
        assert!(!wrong_scope.passed);
        let combined = VerificationReport::combine(
            ValidationDepth::Native,
            VerificationScope::Comparison,
            [source],
        );
        assert!(!combined.passed);
        for depth in [ValidationDepth::Native, ValidationDepth::Decoded] {
            let mismatch = VerificationReport::new(
                depth,
                VerificationScope::Comparison,
                checks(),
                DecodeEvidence::ContentCompared { equal: false },
            );
            assert!(!mismatch.passed);
        }
        let empty =
            VerificationReport::combine(ValidationDepth::Native, VerificationScope::Comparison, []);
        assert!(!empty.passed);
    }

    #[test]
    fn an_aggregate_failure_is_retained_after_refresh() {
        let mut child = VerificationReport::new(
            ValidationDepth::Decoded,
            VerificationScope::Comparison,
            checks(),
            DecodeEvidence::ContentCompared { equal: true },
        );
        child.passed = false;
        let mut combined = VerificationReport::combine(
            ValidationDepth::Decoded,
            VerificationScope::Comparison,
            [child],
        );
        assert_eq!(
            combined.failure,
            Some(VerificationFailure::ChildCheckFailed)
        );
        combined.refresh_passed();
        assert!(!combined.passed);
        let mismatch = VerificationReport::new(
            ValidationDepth::Decoded,
            VerificationScope::Comparison,
            checks(),
            DecodeEvidence::ContentCompared { equal: false },
        );
        assert_eq!(
            mismatch.into_source_admission().unwrap_err(),
            super::super::super::ConversionError("source-decode-comparison-failed")
        );
    }

    #[test]
    fn successful_decoding_cannot_override_a_native_failure() {
        for scope in [
            VerificationScope::SourceAdmission,
            VerificationScope::Comparison,
        ] {
            let decoded_evidence = match scope {
                VerificationScope::SourceAdmission => DecodeEvidence::SourceDecoded,
                VerificationScope::Comparison => DecodeEvidence::ContentCompared { equal: true },
            };
            for failed_check in 0..4 {
                let mut native = VerificationReport::new(
                    ValidationDepth::Native,
                    scope,
                    checks(),
                    DecodeEvidence::not_requested(),
                );
                match failed_check {
                    0 => native.checks.stream_metadata = CheckOutcome::Fail,
                    1 => native.checks.packet_payloads_equal = false,
                    2 => native.checks.packet_timing_equal = false,
                    _ => native.failure = Some(VerificationFailure::ChildCheckFailed),
                }
                native.refresh_passed();
                let mut decoded = VerificationReport::new(
                    ValidationDepth::Decoded,
                    scope,
                    checks(),
                    decoded_evidence,
                );
                decoded.require_native(&native).unwrap();
                assert_eq!(decoded.decode_evidence, decoded_evidence);
                assert_eq!(decoded.checks, native.checks);
                assert_eq!(decoded.failure, Some(VerificationFailure::ChildCheckFailed));
                decoded.refresh_passed();
                assert!(!decoded.passed);
            }
        }
    }

    #[test]
    fn combining_rechecks_a_childs_explicit_failure_even_if_its_flag_is_stale() {
        let mut child = VerificationReport::new(
            ValidationDepth::Decoded,
            VerificationScope::Comparison,
            checks(),
            DecodeEvidence::ContentCompared { equal: true },
        );
        child.failure = Some(VerificationFailure::ChildCheckFailed);
        assert!(child.passed);
        let mut combined = VerificationReport::combine(
            ValidationDepth::Decoded,
            VerificationScope::Comparison,
            [child],
        );
        combined.refresh_passed();
        assert_eq!(
            combined.failure,
            Some(VerificationFailure::ChildCheckFailed)
        );
        assert!(!combined.passed);
    }

    #[test]
    fn native_requirement_rejects_wrong_scope_and_additional_decode_evidence() {
        let mut decoded = VerificationReport::new(
            ValidationDepth::Decoded,
            VerificationScope::Comparison,
            checks(),
            DecodeEvidence::ContentCompared { equal: true },
        );
        let mut native = VerificationReport::new(
            ValidationDepth::Native,
            VerificationScope::SourceAdmission,
            checks(),
            DecodeEvidence::not_requested(),
        );
        assert!(decoded.require_native(&native).is_err());
        native.scope = VerificationScope::Comparison;
        native.decode_evidence = DecodeEvidence::ContentCompared { equal: true };
        assert!(decoded.require_native(&native).is_err());
        native.decode_evidence = DecodeEvidence::not_requested();
        native.required_depth = ValidationDepth::Decoded;
        assert!(decoded.require_native(&native).is_err());
        native.required_depth = ValidationDepth::Native;
        native.passed = false;
        decoded.require_native(&native).unwrap();
        decoded.refresh_passed();
        assert!(!decoded.passed);
    }
}
