//! Byte-bound native checks of supplied manifests, independently of media and external engines.
mod snapshot;

use super::{ConversionError, Converter};
use crate::{
    inspection::OutputFormat,
    resources::{Claim, Hardware, ReservationError},
};
pub use drmr_manifest::Report as NativeManifestReport;
use drmr_manifest::{
    Document, DocumentId, DocumentSize, Format, ManifestLink, Options, Presentation, Profile,
};
use serde::Serialize;
use std::num::NonZeroUsize;
use std::path::Path;

const CONFIG: ConversionError = ConversionError("manifest-lint-configuration-invalid");
pub(super) const CHANGED: ConversionError = ConversionError("manifest-changed-during-validation");
const FAILED: ConversionError = ConversionError("native-manifest-lint-failed");
pub(super) const GRAPH: ConversionError = ConversionError("manifest-validation-graph-limit");

fn manifest_format(path: &Path) -> Result<Option<Format>, ConversionError> {
    use std::io::Read as _;
    let mut prefix = [0; 64];
    let count = super::files::regular_input(path)?
        .read(&mut prefix)
        .map_err(|_| ConversionError("manifest-unreadable"))?;
    Ok(match crate::inspection::format_hint(&prefix[..count]) {
        Some(crate::inspection::InputFormat::Hls) => Some(Format::Hls),
        Some(crate::inspection::InputFormat::Dash) => Some(Format::Dash),
        _ => None,
    })
}

/// The baseline protocol rules or the additional manifest-level Apple VOD authoring rules.
/// Choosing an authoring profile does not establish media or device conformance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum ManifestProfile {
    #[default]
    Base,
    AppleVod,
}
impl ManifestProfile {
    fn select(self, format: Format) -> Result<Profile, ConversionError> {
        match (format, self) {
            (Format::Hls, Self::Base) => Ok(Profile::HlsBaseManifest),
            (Format::Hls, Self::AppleVod) => Ok(Profile::AppleVodManifest),
            (Format::Dash, Self::Base) => Ok(Profile::DashStaticManifest),
            (Format::Dash, Self::AppleVod) => {
                Err(ConversionError("manifest-lint-profile-unsupported"))
            }
        }
    }
}

/// Native findings and explicitly requested external evidence retain separate scopes.
#[derive(Clone, Debug, Serialize)]
pub struct ManifestChecks {
    pub schema_version: u32,
    pub scope: &'static str,
    pub passed: bool,
    pub manifest_count: usize,
    pub native_manifest: NativeManifestReport,
    pub external_requested: bool,
    pub external_manifest: Option<super::conformance::ManifestValidationReport>,
    pub external_error: Option<&'static str>,
}

impl ManifestChecks {
    pub fn failure(&self) -> Option<ConversionError> {
        if self.native_manifest.is_clean_for_scope() {
            self.external_error.map(ConversionError)
        } else {
            Some(FAILED)
        }
    }
}

/// The native report and the exact original manifest inventory checked by it.
/// Publication callers retain this guard through their existing atomic commit checks.
pub(super) struct CheckedManifest {
    pub report: NativeManifestReport,
    snapshot: snapshot::Snapshot,
}

/// Byte-bound discovery retained by a bundle before admitting its output checks.
pub(super) struct ManifestSizing {
    snapshot: snapshot::Snapshot,
}

#[derive(Clone, Copy)]
pub(super) struct ManifestDemand {
    pub minimum_bytes: u64,
    pub metadata_bytes: u64,
    seal: [u8; 32],
    format: Format,
}

impl ManifestSizing {
    fn retained_bytes(snapshot: &snapshot::Snapshot) -> u64 {
        (snapshot.metadata_bytes() + 4 * std::mem::size_of::<Self>()) as u64
    }

    pub fn check(&self, converter: &Converter) -> Result<(), ConversionError> {
        self.snapshot.check(converter)
    }
}

impl CheckedManifest {
    pub fn require_clean(&self) -> Result<(), ConversionError> {
        if self.report.is_clean_for_scope() {
            Ok(())
        } else {
            Err(FAILED)
        }
    }
    pub fn check(&self, converter: &Converter) -> Result<(), ConversionError> {
        self.snapshot.check(converter)
    }
}

impl Converter {
    pub(super) fn manifest_demand(
        &self,
        entrypoint: &Path,
    ) -> Result<Option<ManifestDemand>, ConversionError> {
        let Some(native_format) = manifest_format(entrypoint)? else {
            return Ok(None);
        };
        let captured = snapshot::Snapshot::capture_metadata(self, entrypoint, native_format)?;
        let snapshot = &captured.snapshot;
        let mut options = self.manifest_options(ManifestProfile::Base.select(native_format)?);
        let metadata = snapshot.metadata_bytes();
        let sizes: Vec<_> = snapshot
            .nodes
            .iter()
            .map(|node| DocumentSize {
                id: node.id,
                format: native_format,
                byte_count: node.bytes,
            })
            .collect();
        options.workers = NonZeroUsize::new(1);
        options.limits.max_references = snapshot.reference_bound();
        options.limits.memory_budget_bytes = options
            .limits
            .memory_budget_bytes
            .checked_sub(metadata)
            .filter(|&bytes| bytes > 0)
            .ok_or(ConversionError("resource-budget"))?;
        let estimate =
            drmr_manifest::estimate_document_sizes(&sizes, &options).map_err(|_| CONFIG)?;
        if estimate.admitted_workers == 0 {
            return Err(ConversionError("resource-budget"));
        }
        let minimum_bytes = u64::try_from(estimate.estimated_peak_bytes)
            .ok()
            .and_then(|bytes| bytes.checked_add(metadata as u64))
            .ok_or(ConversionError("resource-budget"))?;
        Ok(Some(ManifestDemand {
            minimum_bytes,
            metadata_bytes: ManifestSizing::retained_bytes(snapshot),
            seal: snapshot.seal(),
            format: native_format,
        }))
    }

    pub(super) fn bind_manifest_demand(
        &self,
        source: &super::FrozenSource,
        demand: Option<ManifestDemand>,
    ) -> Result<Option<ManifestSizing>, ConversionError> {
        let changed = ConversionError("verification-output-changed");
        let format = manifest_format(source.entrypoint())?;
        let Some(demand) = demand else {
            return if format.is_none() {
                Ok(None)
            } else {
                Err(changed)
            };
        };
        if format != Some(demand.format) {
            return Err(changed);
        }
        let captured =
            snapshot::Snapshot::capture_metadata(self, source.entrypoint(), demand.format)
                .map_err(|error| {
                    if error.0 == "cancelled" {
                        error
                    } else {
                        changed
                    }
                })?;
        let metadata = ManifestSizing::retained_bytes(&captured.snapshot);
        if captured.snapshot.seal() != demand.seal || metadata > demand.metadata_bytes {
            return Err(changed);
        }
        let sizing = ManifestSizing {
            snapshot: captured.into_snapshot(),
        };
        sizing.check(self)?;
        Ok(Some(sizing))
    }
    /// Lint the final HLS playlist graph or static DASH manifest without reading media,
    /// keys, licenses, or remote references. Findings contain only rule codes and locations.
    /// Incomplete and truncated reports never establish a clean manifest result.
    pub fn lint_manifest(
        &self,
        entrypoint: &Path,
        format: OutputFormat,
    ) -> Result<NativeManifestReport, ConversionError> {
        self.checked_manifest(entrypoint, format)
            .map(|checked| checked.report)
    }

    /// Select additional authoring rules for a standalone manifest-only check.
    /// Apple VOD is an HLS profile; the baseline profile selects static DASH rules for MPDs.
    pub fn lint_manifest_with_profile(
        &self,
        entrypoint: &Path,
        format: OutputFormat,
        profile: ManifestProfile,
    ) -> Result<NativeManifestReport, ConversionError> {
        self.checked_manifest_with_profile(entrypoint, format, profile)
            .map(|checked| checked.report)
    }

    /// Check native rules by default and retain requested external checks against the same
    /// byte-bound manifest graph. An unavailable engine is separate from native findings.
    pub fn check_manifest(
        &self,
        entrypoint: &Path,
        format: OutputFormat,
        external: Option<&super::conformance::ManifestValidationTools>,
    ) -> Result<ManifestChecks, ConversionError> {
        self.check_manifest_with_profile(entrypoint, format, ManifestProfile::Base, external)
    }

    /// Keep the selected native profile and an explicitly requested external engine distinct.
    pub fn check_manifest_with_profile(
        &self,
        entrypoint: &Path,
        format: OutputFormat,
        profile: ManifestProfile,
        external: Option<&super::conformance::ManifestValidationTools>,
    ) -> Result<ManifestChecks, ConversionError> {
        let checked = self.checked_manifest_with_profile(entrypoint, format, profile)?;
        let (external_manifest, external_error) = match external {
            None => (None, None),
            Some(tools) => match self.validate_manifest(entrypoint, format, tools) {
                Ok(report) => (Some(report), None),
                Err(error) if error.0 == "cancelled" => return Err(error),
                Err(error) => (None, Some(error.0)),
            },
        };
        if external.is_some() {
            checked.check(self)?;
        }
        self.backend.check_cancelled()?;
        Ok(ManifestChecks {
            schema_version: 2,
            scope: "manifest-only",
            passed: checked.report.is_clean_for_scope() && external_error.is_none(),
            manifest_count: checked.report.document_count,
            native_manifest: checked.report,
            external_requested: external.is_some(),
            external_manifest,
            external_error,
        })
    }

    pub(super) fn checked_output_manifest(
        &self,
        path: &Path,
    ) -> Result<Option<CheckedManifest>, ConversionError> {
        use std::io::Read as _;
        self.backend.check_cancelled()?;
        let mut prefix = [0; 64];
        let mut file = super::files::regular_input(path)?;
        let count = file
            .read(&mut prefix)
            .map_err(|_| ConversionError("invalid-output-media"))?;
        let text = prefix[..count]
            .strip_prefix(b"\xef\xbb\xbf")
            .unwrap_or(&prefix[..count])
            .trim_ascii_start();
        let format = if text.starts_with(b"#EXTM3U") {
            OutputFormat::Hls
        } else if text.starts_with(b"<") {
            OutputFormat::Dash
        } else {
            return Ok(None);
        };
        self.checked_manifest(path, format).map(Some)
    }

    pub(super) fn checked_manifest(
        &self,
        entrypoint: &Path,
        format: OutputFormat,
    ) -> Result<CheckedManifest, ConversionError> {
        self.checked_manifest_with_profile(entrypoint, format, ManifestProfile::Base)
    }

    fn checked_manifest_with_profile(
        &self,
        entrypoint: &Path,
        format: OutputFormat,
        profile: ManifestProfile,
    ) -> Result<CheckedManifest, ConversionError> {
        self.backend.check_cancelled()?;
        let format = match format {
            OutputFormat::Hls | OutputFormat::HlsTs => Format::Hls,
            OutputFormat::Dash => Format::Dash,
            OutputFormat::Mp4 => {
                return Err(ConversionError("manifest-validation-format-unsupported"));
            }
        };
        let mut options = self.manifest_options(profile.select(format)?);
        let _scope = self.backend.scope(super::measurement::Phase::Verification);
        let _task = self.backend.task(super::observation::Task::ManifestLint);
        let snapshot = snapshot::Snapshot::capture(self, entrypoint, format, &options)?;
        let sizes: Vec<_> = snapshot
            .nodes
            .iter()
            .map(|node| DocumentSize {
                id: node.id,
                format,
                byte_count: node.bytes,
            })
            .collect();
        options.limits.memory_budget_bytes = options
            .limits
            .memory_budget_bytes
            .checked_sub(snapshot.metadata_bytes())
            .filter(|&bytes| bytes > 0)
            .ok_or(ConversionError("resource-budget"))?;
        options.limits.max_references = snapshot.reference_bound();
        let estimate =
            drmr_manifest::estimate_document_sizes(&sizes, &options).map_err(|_| CONFIG)?;
        if estimate.admitted_workers == 0 {
            return Err(ConversionError("resource-budget"));
        }
        options.workers = NonZeroUsize::new(estimate.admitted_workers);
        let _permit = self.manifest_permit(
            estimate.admitted_workers,
            estimate
                .estimated_peak_bytes
                .saturating_add(snapshot.metadata_bytes()),
        )?;
        // Raw bytes are loaded only after the same admission estimate as the library.
        let bytes = snapshot.read(self)?;
        let documents: Vec<_> = snapshot
            .nodes
            .iter()
            .zip(&bytes)
            .map(|(node, bytes)| Document {
                id: node.id,
                format,
                bytes,
                base_uri: Some(&node.base_uri),
            })
            .collect();
        let links = snapshot.links(&bytes, &options.limits);
        let report = drmr_manifest::lint_presentation(
            Presentation {
                entrypoint: DocumentId(0),
                documents: &documents,
                links: &links,
            },
            &options,
        )
        .map_err(|_| CONFIG)?;
        self.backend.check_cancelled()?;
        snapshot.check_admitted(self)?;
        Ok(CheckedManifest { report, snapshot })
    }

    fn manifest_options(&self, profile: Profile) -> Options {
        let (host_cpu, host_memory) = crate::resources::capacity(Hardware::detect(), 1);
        let (runner_cpu, runner_memory) = self.backend.runner.manifest_capacity();
        let mut options = Options::new(profile);
        options.workers = NonZeroUsize::new(
            host_cpu
                .min(runner_cpu)
                .min(self.maximum_workers.unwrap_or(64))
                .max(1),
        );
        options.limits.max_workers = 64;
        options.limits.max_document_bytes =
            usize::try_from(crate::inspection::MAX_MANIFEST_BYTES).unwrap_or(usize::MAX);
        options.limits.max_total_bytes = 64 * 1024 * 1024;
        options.limits.max_documents = 1024;
        // Existing admission is byte bounded, including large PSSH elements on one XML line.
        options.limits.max_line_bytes = options.limits.max_document_bytes;
        options.limits.max_lines = options.limits.max_document_bytes;
        options.limits.memory_budget_bytes =
            usize::try_from(host_memory.min(runner_memory)).unwrap_or(usize::MAX);
        options
    }

    pub(super) fn publish_manifest_output(
        &self,
        stage: &super::files::Stage,
        destination: &Path,
        manifest: &Path,
        format: OutputFormat,
    ) -> Result<NativeManifestReport, ConversionError> {
        let checked = self.checked_manifest(manifest, format)?;
        checked.require_clean()?;
        self.publish_directory_checked(stage, destination, || checked.check(self))?;
        Ok(checked.report)
    }

    fn manifest_permit(
        &self,
        workers: usize,
        memory: usize,
    ) -> Result<crate::resources::Permit, ConversionError> {
        let capacity = crate::resources::capacity(Hardware::detect(), 1);
        if workers > capacity.0 || u64::try_from(memory).map_or(true, |memory| memory > capacity.1)
        {
            return Err(ConversionError("resource-budget"));
        }
        self.backend
            .runner
            .native_permit(
                Claim {
                    cpu_threads: workers,
                    memory_bytes: u64::try_from(memory)
                        .map_err(|_| ConversionError("resource-budget"))?,
                },
                &self.backend.stop,
                self.backend.timeout,
            )
            .map_err(|error| match error {
                ReservationError::Cancelled => ConversionError("cancelled"),
                ReservationError::Insufficient | ReservationError::Timeout => {
                    ConversionError("resource-budget")
                }
            })
    }
}

#[cfg(test)]
mod tests;
