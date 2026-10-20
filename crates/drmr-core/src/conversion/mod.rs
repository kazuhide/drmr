//! Local, no-transcode conversion with explicit key roles and independent round-trip checks.
//!
//! Qualified local MP4, static DASH, and fragmented-MP4 HLS inputs use fresh source bytes.
//! Joint DASH/HLS output preserves audio and video without transcoding.
//! Qualified finite-edit AAC with optional H.264 from the same original progressive file
//! supports clear progressive or encrypted fragmented MP4 output. Other MP4 output
//! supports one track. Unknown protection and unsupported features fail closed.
//! Shaka's raw-key interface passes keys to its child process as arguments. Records and
//! errors are redacted, but operating-system process inspection can observe those arguments.
mod aes128;
mod download;
pub mod download_policy;
pub use download::{DownloadedSource, preflight_download_output};
mod audio;
mod backend;
pub(crate) mod dash;
mod execution;
mod finite;
pub mod key_file;
mod packets;
pub mod plan;
pub use aes128::{
    Aes128Key, HlsInputKeys, HlsOutputKey, SegmentIv, decrypt_segment, encrypt_segment,
};
mod cenc;
mod files;
mod frozen;
pub use frozen::{FrozenSource, SourceFile, SourceInventory};
mod hls;
mod keys;
mod signaling;
pub use signaling::OutputSignaling;
mod presentation;
mod progressive;
mod timing;
mod ts;
pub use presentation::{
    ConversionRequest, SourceProtection, SourceQualification, SourceValidationPurpose,
    TrackConversionRequest, VerificationRequest, VerificationSource,
};
mod verify;
use crate::{
    inspection::{self, InputFormat, Inspection, Protection, TargetProtection, TrackKind},
    toolrun::ToolRunner,
};
use backend::Backend;
pub use keys::{ContentKey, InputKeys, OutputKey};
use serde::Serialize;
use std::{
    fs::{self, File},
    io::Read,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
pub use verify::{CheckOutcome, VerificationChecks, VerificationReport};
pub mod measurement;
const MAX_MEDIA_BYTES: u64 = 16 * 1024 * 1024 * 1024;
/// Fixed failure codes contain no paths, key material, or backend output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConversionError(pub &'static str);
#[derive(Clone, Debug, Serialize)]
pub struct ConversionReport {
    pub schema_version: u32,
    pub input: Inspection,
    pub output: Inspection,
    pub verification: VerificationReport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub processing: Option<inspection::ActualProcessing>,
}
impl ConversionReport {
    fn describe_preparation(&mut self, preparation: inspection::InputPreparation) {
        if let Some(processing) = &mut self.processing {
            processing.input_preparation = preparation;
        }
    }
}
/// A local source and a zero-based stream selector. Execution revalidates the actual media.
/// No inspection report or declared media role authorizes a conversion.
pub struct LocalTrack {
    pub source: PathBuf,
    pub kind: TrackKind,
    pub index: usize,
}
impl std::fmt::Debug for LocalTrack {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalTrack")
            .field("kind", &self.kind)
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}
/// Explicit backend configuration; no executable is selected from source metadata.
pub struct Converter {
    backend: Backend,
    maximum_workers: Option<usize>,
    downloaded: Option<Arc<download::Seal>>,
    frozen_source: Option<Arc<frozen::OriginalGuard>>,
    planned_destination: Option<Arc<plan::destination::OutputGuard>>,
    output_signaling: Option<Arc<OutputSignaling>>,
}
impl Converter {
    pub fn new(
        runner: Arc<ToolRunner>,
        packager: impl Into<String>,
        ffmpeg: impl Into<String>,
    ) -> Self {
        Self {
            maximum_workers: None,
            downloaded: None,
            frozen_source: None,
            planned_destination: None,
            output_signaling: None,
            backend: Backend {
                stop: crate::concurrency::Stop::new(),
                runner,
                packager: packager.into(),
                ffmpeg: ffmpeg.into(),
                ffprobe: "ffprobe".into(),
                timeout: Duration::from_hours(1),
                measurement: None,
                activity: None,
            },
        }
    }
    /// Observe fixed activity starts and scope closures, independently of optional measurements.
    /// Scopes can overlap or nest; closure is not proof of success. No paths, keys or arguments
    /// are supplied. The default converter does not emit these additional events.
    #[must_use]
    pub fn with_activity_sink(mut self, sink: Arc<dyn crate::events::Sink>) -> Self {
        self.backend.activity = Some(sink);
        self
    }
    /// Observe one operation in a fresh context, also when this converter is reused concurrently.
    /// The ordinary media result and errors are returned unchanged.
    pub fn measure<T>(
        &self,
        execute: impl FnOnce(&Self) -> Result<T, ConversionError>,
    ) -> (Result<T, ConversionError>, measurement::MeasurementReport) {
        let measurement = Arc::new(measurement::Measurements::default());
        let mut backend = self.backend.clone();
        backend.runner = Arc::new(self.backend.runner.observed(measurement.clone()));
        backend.measurement = Some(Arc::clone(&measurement));
        let command = Self {
            backend,
            maximum_workers: self.maximum_workers,
            downloaded: self.downloaded.clone(),
            frozen_source: self.frozen_source.clone(),
            planned_destination: self.planned_destination.clone(),
            output_signaling: self.output_signaling.clone(),
        };
        let result = execute(&command);
        (result, measurement.report())
    }
    /// Share a cooperative cancellation flag with scheduling and media subprocesses.
    #[must_use]
    pub fn with_stop(mut self, stop: crate::concurrency::Stop) -> Self {
        self.backend.stop = stop;
        self
    }
    /// Opt into one qualified output-signaling profile; unsupported routes refuse it.
    #[must_use]
    pub fn with_output_signaling(mut self, signaling: impl Into<Option<OutputSignaling>>) -> Self {
        self.output_signaling = signaling.into().map(Arc::new);
        self
    }
    fn refuse_signaling(&self) -> Result<(), ConversionError> {
        if self.output_signaling.is_some() {
            return Err(ConversionError("output-signaling-profile-not-supported"));
        }
        Ok(())
    }
    fn publish(&self, source: &Path, destination: &Path) -> Result<(), ConversionError> {
        let _scope = self.backend.scope(measurement::Phase::Publication);
        self.backend.check_cancelled()?;
        self.check_plan_destination()?;
        files::publish_file_committing(
            source,
            destination,
            Some(&self.backend.stop),
            || {
                self.check_downloaded()?;
                self.check_frozen_source()
            },
            |source, destination| self.commit_publication(source, destination),
        )
    }
    fn publish_directory(&self, source: &Path, destination: &Path) -> Result<(), ConversionError> {
        let _scope = self.backend.scope(measurement::Phase::Publication);
        self.backend.check_cancelled()?;
        self.check_plan_destination()?;
        files::publish_directory_committing(
            source,
            destination,
            Some(&self.backend.stop),
            || {
                self.check_downloaded()?;
                self.check_frozen_source()
            },
            |source, destination| self.commit_publication(source, destination),
        )
    }
    #[must_use]
    pub fn with_ffprobe(mut self, binary: impl Into<String>) -> Self {
        self.backend.ffprobe = binary.into();
        self
    }
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.backend.timeout = timeout;
        self
    }
    pub fn convert_hls_aes128(
        &self,
        input: &Path,
        output: &Path,
        input_keys: &InputKeys,
        output_key: &HlsOutputKey,
    ) -> Result<ConversionReport, ConversionError> {
        self.hls_output(input, output, input_keys, output_key)
    }
    pub fn convert_hls_to_hls_aes128(
        &self,
        input: &Path,
        output: &Path,
        input_keys: &InputKeys,
        hls_keys: &HlsInputKeys,
        output_key: &HlsOutputKey,
    ) -> Result<ConversionReport, ConversionError> {
        self.refuse_signaling()?;
        if output_key.associated_iv().is_some() {
            return Err(ConversionError("output-iv-not-used"));
        }
        let workspace = workspace()?;
        let localized = hls::localize(input, workspace.path(), hls_keys, &self.backend)?;
        if localized.transport.is_some() {
            return Err(ConversionError("ts-input-requires-generic-hls-output"));
        }
        let actual = validate(&localized.combined, input_keys)?;
        let key = source_key(&actual, input_keys);
        let check = self.backend.verify_views(
            verify::StreamView::new(&localized.reference, 0, key)
                .with_metadata(&localized.combined),
            verify::StreamView::new(&localized.combined, 0, key),
        )?;
        if !check.passed {
            return Err(ConversionError("source-decryption-verification-failed"));
        }
        let mut report = self.hls_output(&localized.combined, output, input_keys, output_key)?;
        report.input = localized.report;
        report.describe_preparation(inspection::InputPreparation::LocalHlsResources);
        Ok(report)
    }
    pub fn convert_from_hls(
        &self,
        input: &Path,
        output: &Path,
        target: TargetProtection,
        input_keys: &InputKeys,
        hls_keys: &HlsInputKeys,
        output_key: Option<&OutputKey>,
    ) -> Result<ConversionReport, ConversionError> {
        self.refuse_signaling()?;
        self.hls_input(input, output, target, input_keys, hls_keys, output_key)
    }
    pub fn convert_mp4(
        &self,
        input: &Path,
        output: &Path,
        target: TargetProtection,
        input_keys: &InputKeys,
        output_key: Option<&OutputKey>,
    ) -> Result<ConversionReport, ConversionError> {
        self.refuse_signaling()?;
        if target == TargetProtection::HlsSampleAes {
            return Err(ConversionError("target-requires-ts-output"));
        }
        if target == TargetProtection::HlsAes128 {
            return Err(ConversionError("target-requires-hls-output"));
        }
        if target != TargetProtection::Clear && output_key.is_none() {
            return Err(ConversionError("output-key-required"));
        }
        if target == TargetProtection::Clear && output_key.is_some() {
            return Err(ConversionError("output-key-not-used"));
        }
        let destination = destination(output)?;
        let workspace = workspace()?;
        let snapshot = workspace.path().join("source.mp4");
        {
            let _scope = self.backend.scope(measurement::Phase::InputPreparation);
            copy_private(input, &snapshot)?;
        }
        let inspected = {
            let _scope = self.backend.scope(measurement::Phase::Inspection);
            inspection::inspect(&snapshot).map_err(|_| ConversionError("invalid-input-media"))?
        };
        if inspected.format != InputFormat::Mp4 {
            return Err(ConversionError("input-format-not-supported"));
        }
        if let Some(report) = self.finite_mp4(
            &snapshot,
            &destination,
            &inspected,
            input_keys,
            output_key,
            target,
        )? {
            return Ok(report);
        }
        let input_report = validate(&snapshot, input_keys)?;
        self.backend
            .profile(&snapshot, source_key(&input_report, input_keys))?;
        let clear = workspace.path().join("clear.mp4");
        let normalization = self.backend.scope(measurement::Phase::Normalization);
        self.backend.package(
            &snapshot,
            &clear,
            (input_report.protection != Protection::Clear).then_some(input_keys),
            None,
        )?;
        drop(normalization);
        let candidate = workspace.path().join("candidate.mp4");
        let packaging = self.backend.scope(measurement::Phase::Packaging);
        self.backend.package(
            &clear,
            &candidate,
            None,
            output_key.map(|key| (key, target)),
        )?;
        drop(packaging);
        let output_report =
            inspection::inspect(&candidate).map_err(|_| ConversionError("invalid-output-media"))?;
        let expected = match target {
            TargetProtection::Clear => Protection::Clear,
            TargetProtection::Cenc => Protection::Cenc,
            TargetProtection::Cbcs => Protection::Cbcs,
            TargetProtection::HlsAes128 | TargetProtection::HlsSampleAes => {
                return Err(ConversionError("target-requires-hls-output"));
            }
        };
        if output_report.protection != expected || output_report.tracks.len() != 1 {
            return Err(ConversionError("output-protection-mismatch"));
        }
        if let Some(key) = output_key
            && output_report.tracks[0].key_ids != [key.0.id_hex()]
        {
            return Err(ConversionError("output-key-id-mismatch"));
        }
        let verification = self.backend.verify_media(
            &snapshot,
            &candidate,
            source_key(&input_report, input_keys),
            output_key.map(|key| &key.0),
        )?;
        if !verification.passed {
            return Err(ConversionError("round-trip-verification-failed"));
        }
        self.backend.checkpoint(&[&workspace]);
        self.publish(&candidate, &destination)?;
        Ok(ConversionReport {
            schema_version: 1,
            input: input_report,
            output: output_report,
            verification,
            processing: Some(inspection::ActualProcessing::new(
                inspection::ProcessingRouteId::FragmentedMp4,
                inspection::InputPreparation::CompleteFile,
            )),
        })
    }
    pub fn verify_mp4(
        &self,
        input: &Path,
        input_keys: &InputKeys,
        output: &Path,
        output_keys: &InputKeys,
    ) -> Result<VerificationReport, ConversionError> {
        let workspace = workspace()?;
        let source = workspace.path().join("source.mp4");
        let result = workspace.path().join("result.mp4");
        copy_private(input, &source)?;
        copy_private(output, &result)?;
        let inspected =
            inspection::inspect(&source).map_err(|_| ConversionError("invalid-input-media"))?;
        if inspected.format != InputFormat::Mp4 {
            return Err(ConversionError("input-format-not-supported"));
        }
        let result_report =
            inspection::inspect(&result).map_err(|_| ConversionError("invalid-input-media"))?;
        if result_report.format != InputFormat::Mp4 {
            return Err(ConversionError("input-format-not-supported"));
        }
        if result_report.protection == Protection::Clear
            && inspected.tracks.len() > 1
            && progressive::verification_input(&source, &inspected, input_keys)?
        {
            if result_report.tracks.len() != inspected.tracks.len() {
                return Err(ConversionError("output-protection-mismatch"));
            }
            return self.verify_progressive(&source, &result, &inspected, input_keys);
        }
        if inspected.tracks.len() == 2
            && matches!(
                result_report.protection,
                Protection::Cenc | Protection::Cbcs
            )
            && progressive::verification_input(&source, &inspected, input_keys)?
            && progressive::validate_input(&source, &inspected, input_keys)?
        {
            if result_report.tracks.len() != inspected.tracks.len() {
                return Err(ConversionError("output-protection-mismatch"));
            }
            // The same qualified finite whole-file profile accepted by convert_mp4.
            // Keep the two key domains independent throughout every original-track proof.
            let hls = HlsInputKeys::default();
            return self.verify(VerificationRequest {
                source: VerificationSource {
                    input: &source,
                    input_keys,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: &result,
                    input_keys: output_keys,
                    hls_keys: &hls,
                },
            });
        }
        let source_report = validate(&source, input_keys)?;
        let result_report = validate(&result, output_keys)?;
        self.backend
            .profile(&source, source_key(&source_report, input_keys))?;
        self.backend
            .profile(&result, source_key(&result_report, output_keys))?;
        self.backend.verify_media(
            &source,
            &result,
            source_key(&source_report, input_keys),
            source_key(&result_report, output_keys),
        )
    }
}
fn source_key<'a>(report: &Inspection, keys: &'a InputKeys) -> Option<&'a ContentKey> {
    if report.protection == Protection::Clear {
        return None;
    }
    let id = report.tracks[0].key_ids.first()?;
    keys.keys.iter().find(|key| key.id_hex() == *id)
}
fn validate(path: &Path, keys: &InputKeys) -> Result<Inspection, ConversionError> {
    let report = inspection::inspect(path).map_err(|_| ConversionError("invalid-input-media"))?;
    if report.format != InputFormat::Mp4 {
        return Err(ConversionError("input-format-not-supported"));
    }
    if report.tracks.len() != 1 {
        return Err(ConversionError("single-track-mp4-required"));
    }
    if !matches!(report.tracks[0].kind, TrackKind::Audio | TrackKind::Video) {
        return Err(ConversionError("media-track-not-supported"));
    }
    if !matches!(
        report.protection,
        Protection::Clear | Protection::Cenc | Protection::Cbcs
    ) {
        return Err(ConversionError("input-protection-not-supported"));
    }
    audio::validate_sample_groups(path)?;
    if report.protection != Protection::Clear
        && (report.tracks[0].key_ids.is_empty()
            || report.tracks[0].key_ids.iter().any(|id| !keys.contains(id)))
    {
        return Err(ConversionError("input-key-required"));
    }
    Ok(report)
}
fn workspace() -> Result<files::Stage, ConversionError> {
    files::Stage::new(None)
}
fn destination(path: &Path) -> Result<PathBuf, ConversionError> {
    #[cfg(windows)]
    files::check_destination(path)?;
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
        || fs::symlink_metadata(path).is_ok()
    {
        return Err(ConversionError("output-exists-or-unsafe"));
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if fs::symlink_metadata(parent)
        .map_err(|_| ConversionError("output-parent-unavailable"))?
        .file_type()
        .is_symlink()
    {
        return Err(ConversionError("output-parent-unsafe"));
    }
    let parent = parent
        .canonicalize()
        .map_err(|_| ConversionError("output-parent-unavailable"))?;
    let name = path
        .file_name()
        .ok_or(ConversionError("output-path-invalid"))?;
    Ok(parent.join(name))
}
fn copy_private(source: &Path, destination: &Path) -> Result<(), ConversionError> {
    let metadata = fs::symlink_metadata(source).map_err(|_| ConversionError("input-unreadable"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_MEDIA_BYTES
    {
        return Err(ConversionError("input-not-regular-or-too-large"));
    }
    let source = files::regular_input(source)?;
    validate_held_input(&source)?;
    let mut output = files::create_new(destination)?;
    let count = std::io::copy(&mut source.take(MAX_MEDIA_BYTES + 1), &mut output)
        .map_err(|_| ConversionError("media-copy-failed"))?;
    if count > MAX_MEDIA_BYTES {
        return Err(ConversionError("media-size-limit"));
    }
    Ok(())
}
fn private_file_mode(path: &Path) -> Result<(), ConversionError> {
    files::seal(
        path.parent()
            .ok_or(ConversionError("private-file-permissions-failed"))?,
    )
}
fn publish(source: &Path, destination: &Path) -> Result<(), ConversionError> {
    files::publish_file(source, destination, None)
}

fn validate_held_input(file: &File) -> Result<(), ConversionError> {
    let metadata = file
        .metadata()
        .map_err(|_| ConversionError("input-unreadable"))?;
    if !metadata.is_file() || metadata.len() > MAX_MEDIA_BYTES {
        return Err(ConversionError("input-not-regular-or-too-large"));
    }
    Ok(())
}
#[cfg(all(test, unix))]
mod snapshot_tests {
    use super::*;
    #[test]
    fn opened_handle_must_be_a_bounded_regular_file() {
        let directory = tempfile::tempdir().unwrap();
        let held_directory = File::open(directory.path()).unwrap();
        assert_eq!(
            validate_held_input(&held_directory).unwrap_err().0,
            "input-not-regular-or-too-large"
        );
        let path = directory.path().join("sparse");
        let file = File::create(path).unwrap();
        validate_held_input(&file).unwrap();
        file.set_len(MAX_MEDIA_BYTES + 1).unwrap();
        assert_eq!(
            validate_held_input(&file).unwrap_err().0,
            "input-not-regular-or-too-large"
        );
    }
}
