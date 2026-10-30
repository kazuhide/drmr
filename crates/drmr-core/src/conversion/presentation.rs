//! Joint, codec-preserving presentations with independent evidence for every source stream.
mod bundle_source;
mod dash_preparation;
mod hls_preparation;
pub(in crate::conversion) use bundle_source::BundlePrepared;
pub(in crate::conversion) mod metadata;
mod native_preparation;
mod normalization;
#[cfg(test)]
mod packager_jobs_tests;
#[cfg(test)]
mod signaling_tests;
#[cfg(test)]
mod source_work_tests;
#[cfg(test)]
mod staging_tests;
mod ts_output;
use super::{
    ContentKey, ConversionError, ConversionReport, Converter, HlsOutputKey, InputKeys, LocalTrack,
    OutputKey, VerificationReport, audio, copy_private, destination, hls, signaling::Objects,
    verify::StreamView, workspace,
};
use crate::inspection::{
    self, Evidence, InputFormat, Inspection, OutputFormat, Protection, TargetProtection, TrackKind,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;
/// Execution inputs. Paths and declared roles are freshly validated; no saved report is trusted.
#[derive(Clone, Copy)]
pub struct TrackConversionRequest<'a> {
    pub tracks: &'a [LocalTrack],
    pub output: &'a Path,
    pub format: OutputFormat,
    pub target: TargetProtection,
    pub input_keys: &'a InputKeys,
    pub output_key: Option<&'a OutputKey>,
    /// Public signaling supplied by the operator; never an instruction to retrieve a key.
    pub hls_key_uri: Option<&'a str>,
    pub hls_output_key: Option<&'a HlsOutputKey>,
}
#[derive(Clone)]
struct Prepared {
    metadata: metadata::Metadata,
    outer_protection: SourceProtection,
    compatibility: Vec<SourceCompatibilityNote>,
    source_dictionary: bool,
    source: PathBuf,
    media: PathBuf,
    index: usize,
    report: Inspection,
    key_id: Option<String>,
    transport_index: Option<usize>,
    transport_demux_view: Option<PathBuf>,
    transport_native: Option<std::sync::Arc<super::ts::NativeEvidence>>,
    transport_sample: bool,
    playlist_reference: Option<PathBuf>,
    dash_reference: bool,
    audio_presentation: Option<super::audio_presentation::AudioPresentation>,
    clock: Option<super::native::ManifestAacClock>,
    hls_target_seconds: Option<u64>,
    hls_video_segment_ms: Option<u32>,
}

pub(in crate::conversion) struct BundleAdmission<'a> {
    pub source: VerificationSource<'a>,
    pub inventory: &'a super::SourceInventory,
    pub qualified: &'a SourceQualification,
    pub processing: super::ProcessingMode,
    pub depth: super::ValidationDepth,
}

/// Binds the private clock context to the exact public local-track selection.
struct TrackClock {
    source: PathBuf,
    index: usize,
    kind: TrackKind,
    clock: Option<super::native::ManifestAacClock>,
}
impl TrackClock {
    fn from_prepared(prepared: &Prepared) -> Self {
        Self {
            source: prepared.media.clone(),
            index: prepared.index,
            kind: prepared.report.tracks[0].kind,
            clock: prepared.clock,
        }
    }
    fn bind(&self, track: &LocalTrack, prepared: &mut Prepared) -> Result<(), ConversionError> {
        if self.source != track.source || self.index != track.index || self.kind != track.kind {
            return Err(ConversionError("native-clock-profile-mismatch"));
        }
        prepared.clock = self.clock;
        Ok(())
    }
}

fn manifested_clock(
    path: &Path,
    backend: &super::backend::Backend,
) -> Result<Option<super::native::ManifestAacClock>, ConversionError> {
    super::native::ManifestAacClock::qualify_manifest(&super::native_media::inspect(
        path,
        &backend.stop,
    )?)
}
fn presentation_processing(
    backend: &super::backend::Backend,
    format: OutputFormat,
) -> inspection::ActualProcessing {
    backend.processing(
        if format == OutputFormat::Dash {
            inspection::ProcessingRouteId::DashPresentation
        } else {
            inspection::ProcessingRouteId::Fmp4HlsPresentation
        },
        inspection::InputPreparation::LocalTrackFiles,
    )
}
impl Converter {
    pub fn convert_tracks(
        &self,
        request: TrackConversionRequest<'_>,
    ) -> Result<ConversionReport, ConversionError> {
        self.check_complete_file_processing()?;
        if self
            .output_signaling
            .as_ref()
            .is_some_and(|value| value.is_multi())
        {
            let first = request
                .tracks
                .first()
                .ok_or(super::signaling::UNSUPPORTED)?;
            if request
                .tracks
                .iter()
                .any(|track| track.source != first.source)
            {
                return Err(super::signaling::UNSUPPORTED);
            }
            let frozen = self.capture_source(&first.source)?;
            let tracks: Vec<_> = request
                .tracks
                .iter()
                .map(|track| LocalTrack {
                    source: frozen.entrypoint().to_path_buf(),
                    kind: track.kind,
                    index: track.index,
                })
                .collect();
            return self.using_frozen_source(&frozen).convert_tracks_inner(
                TrackConversionRequest {
                    tracks: &tracks,
                    ..request
                },
                None,
                None,
                None,
                None,
            );
        }
        self.convert_tracks_inner(request, None, None, None, None)
    }
    fn convert_tracks_inner(
        &self,
        request: TrackConversionRequest<'_>,
        transport_stage: Option<&super::files::Stage>,
        metadata: Option<&[metadata::Metadata]>,
        presentations: Option<&[Option<super::audio_presentation::AudioPresentation>]>,
        clocks: Option<&[TrackClock]>,
    ) -> Result<ConversionReport, ConversionError> {
        self.validate_signaling_tracks(&request, metadata.is_some())?;
        if request.format == OutputFormat::HlsTs {
            return Err(ConversionError("ts-original-evidence-required"));
        }
        validate_request(&request)?;
        if request.format == OutputFormat::Mp4 {
            return self.convert_complete_mp4(&request);
        }
        let destination = transport_stage
            .is_none()
            .then(|| destination(request.output))
            .transpose()?;
        let work = workspace()?;
        let (mut sources, clears) = self.normalize_tracks(&request, work.path(), clocks)?;
        if let Some(ranges) = presentations {
            if ranges.len() != sources.len() {
                return Err(super::audio_presentation::MISMATCH);
            }
            for (source, range) in sources.iter_mut().zip(ranges) {
                source.audio_presentation = *range;
            }
        }
        metadata::apply(&mut sources, metadata)?;
        metadata::materialize(&mut sources, &self.backend, request.input_keys)?;
        self.package_prepared_tracks(
            &request,
            transport_stage,
            destination,
            &sources,
            &clears,
            &work,
        )
    }

    fn package_prepared_tracks(
        &self,
        request: &TrackConversionRequest<'_>,
        transport_stage: Option<&super::files::Stage>,
        destination: Option<PathBuf>,
        sources: &[Prepared],
        clears: &[PathBuf],
        work: &super::files::Stage,
    ) -> Result<ConversionReport, ConversionError> {
        let owned_stage = destination
            .as_deref()
            .map(|path| self.output_stage(path))
            .transpose()?;
        let stage = transport_stage
            .or(owned_stage.as_ref())
            .ok_or(ConversionError("output-path-invalid"))?;
        let clear_stage = work.path().join("packaged");
        super::files::create_dir(&clear_stage)
            .map_err(|_| ConversionError("output-staging-failed"))?;
        let package_root = if request.target == TargetProtection::HlsAes128 {
            clear_stage.as_path()
        } else {
            stage.path()
        };
        let owner = if request.target == TargetProtection::HlsAes128 {
            work
        } else {
            stage
        };
        self.package_joint(clears, package_root, request, work, owner)?;
        if request.format == OutputFormat::Hls
            && sources.len() == 1
            && sources[0].report.tracks[0].kind == TrackKind::Audio
            && sources[0].metadata.audio.is_none()
        {
            ts_output::single_audio_variant(package_root)?;
        }
        self.finish_packaged_presentation(package_root, stage.path(), sources, request)?;
        self.finish_output_options(stage, request.format)?;
        let (mut verification, output_tracks, input_tracks) =
            self.verify_joint(sources, request, stage.path(), work.path())?;
        secure_tree(stage.path())?;
        let format = if request.format == OutputFormat::Dash {
            InputFormat::Dash
        } else {
            InputFormat::Hls
        };
        let manifest = stage.path().join(if request.format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        let declared =
            inspection::inspect(&manifest).map_err(|_| ConversionError("invalid-output-media"))?;
        let output = Inspection::new(
            format,
            Evidence::ContainerMetadata,
            output_tracks,
            declared.drm_systems,
            Vec::new(),
        );
        let input = Inspection::new(
            InputFormat::Mp4,
            Evidence::ContainerMetadata,
            input_tracks,
            Vec::new(),
            Vec::new(),
        );
        self.backend.checkpoint(&[work, stage]);
        let native_manifest = if let Some(destination) = destination {
            Some(self.publish_manifest_output(stage, &destination, &manifest, request.format)?)
        } else {
            None
        };
        verification.native_manifest = native_manifest;
        verification.refresh_passed();
        Ok(ConversionReport {
            schema_version: 2,
            input,
            output,
            verification,
            processing: Some(presentation_processing(&self.backend, request.format)),
        })
    }
    fn finish_packaged_presentation(
        &self,
        package_root: &Path,
        output_root: &Path,
        sources: &[Prepared],
        request: &TrackConversionRequest<'_>,
    ) -> Result<(), ConversionError> {
        metadata::finish(package_root, request.format, sources)?;
        if sources
            .iter()
            .any(|source| source.audio_presentation.is_some())
        {
            // The editor reopens only private files after every packaging writer exits.
            super::files::seal(package_root)?;
        }
        for (ordinal, source) in sources.iter().enumerate() {
            if let Some(range) = source.audio_presentation {
                super::audio_presentation::restore(
                    &package_root.join(format!("track-{ordinal:04}/init.mp4")),
                    range,
                    &self.backend.stop,
                )?;
            }
        }
        if request.format == OutputFormat::Hls {
            super::audio_presentation::finish_hls(package_root, sources.len(), &self.backend.stop)?;
        }

        if request.format == OutputFormat::Dash {
            super::dash::finish_transport(&package_root.join("manifest.mpd"))?;
        }
        if let Some(key) = request.hls_output_key {
            encrypt_packaged_hls(package_root, output_root, sources.len(), key)?;
        }
        self.finish_multi_signaling(
            output_root,
            request.format,
            request.target,
            request.output_key,
            request.hls_key_uri,
            request.hls_output_key,
        )?;
        if request.format == OutputFormat::Hls {
            self.finish_hls_segment_target(output_root, sources.len())?;
        }
        Ok(())
    }
    fn convert_complete_mp4(
        &self,
        request: &TrackConversionRequest<'_>,
    ) -> Result<ConversionReport, ConversionError> {
        let work = workspace()?;
        let snapshot = work.path().join("source.mp4");
        let first = &request.tracks[0];
        if request
            .tracks
            .iter()
            .enumerate()
            .any(|(index, track)| track.source != first.source || track.index != index)
        {
            return Err(ConversionError("mp4-requires-one-complete-presentation"));
        }
        copy_private(&first.source, &snapshot)?;
        let actual =
            inspection::inspect(&snapshot).map_err(|_| ConversionError("invalid-input-media"))?;
        if actual.tracks.len() != request.tracks.len() {
            return Err(ConversionError("mp4-requires-one-complete-presentation"));
        }
        for track in request.tracks {
            prepare(&snapshot, track.index, track.kind, request.input_keys)?;
        }
        self.convert_mp4(
            &snapshot,
            request.output,
            request.target,
            request.input_keys,
            request.output_key,
        )
    }
    fn verify_joint(
        &self,
        sources: &[Prepared],
        request: &TrackConversionRequest<'_>,
        stage: &Path,
        work: &Path,
    ) -> Result<
        (
            VerificationReport,
            Vec<inspection::Track>,
            Vec<inspection::Track>,
        ),
        ConversionError,
    > {
        let (actual, keys) = self.prepare_generated_presentation(request, stage, work)?;
        if actual.len() != sources.len() {
            return Err(ConversionError("presentation-track-count-mismatch"));
        }
        let expected = match request.target {
            TargetProtection::Clear | TargetProtection::HlsAes128 => Protection::Clear,
            TargetProtection::Cenc => Protection::Cenc,
            TargetProtection::Cbcs => Protection::Cbcs,
            TargetProtection::HlsSampleAes => {
                return Err(ConversionError("target-requires-ts-output"));
            }
        };
        let mut output_tracks = Vec::with_capacity(actual.len());
        for (ordinal, result) in actual.iter().enumerate() {
            if result.report.protection != expected || result.report.tracks.len() != 1 {
                return Err(ConversionError("output-protection-mismatch"));
            }
            let mut track = result.report.tracks[0].clone();
            if let Some(key) = request.output_key
                && track.key_ids != [key.0.id_hex()]
            {
                return Err(ConversionError("output-key-id-mismatch"));
            }
            track.id = format!("track-{ordinal:04}");
            if request.target == TargetProtection::HlsAes128 {
                track.protection = Protection::HlsAes128;
            }
            output_tracks.push(track);
        }
        let input_tracks = sources
            .iter()
            .enumerate()
            .map(|(ordinal, source)| {
                let mut track = source.report.tracks[0].clone();
                track.id = format!("track-{ordinal:04}");
                track
            })
            .collect();
        let verification =
            self.compare_presentations(sources.to_vec(), actual, request.input_keys, &keys)?;
        if !verification.passed {
            return Err(ConversionError("round-trip-verification-failed"));
        }
        self.verify_signaled_output(&sources[0].source, request, stage)?;
        Ok((verification, output_tracks, input_tracks))
    }
    fn prepare_generated_presentation(
        &self,
        request: &TrackConversionRequest<'_>,
        stage: &Path,
        work: &Path,
    ) -> Result<(Vec<Prepared>, InputKeys), ConversionError> {
        let keys = request
            .output_key
            .map(|key| {
                InputKeys::new(vec![ContentKey::from_hex(
                    &key.0.id_hex(),
                    &key.0.key_hex(),
                )?])
            })
            .transpose()?
            .unwrap_or_default();
        let hls_keys = output_hls_keys(request.hls_output_key)?;
        let directory = work.join("manifest-verification");
        super::files::create_dir(&directory)?;
        let manifest = stage.join(if request.format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        let (backend, preparation) = super::media_progress::scoped(
            &self.backend,
            super::media_progress::Subject::Result,
            super::media_progress::Purpose::Conversion,
        );
        let actual = prepare_presentation(
            &VerificationSource {
                input: &manifest,
                input_keys: &keys,
                hls_keys: &hls_keys,
            },
            &directory,
            &backend,
            self.maximum_workers,
            SourceValidationPurpose::Verification,
        )?;
        preparation.finish();
        self.backend.check_cancelled()?;
        Ok((actual, keys))
    }
    fn package_joint(
        &self,
        clear: &[PathBuf],
        root: &Path,
        request: &TrackConversionRequest<'_>,
        input_owner: &super::files::Stage,
        output_owner: &super::files::Stage,
    ) -> Result<(), ConversionError> {
        let _scope = self.backend.scope(super::measurement::Phase::Packaging);
        let mut argv = joint_arguments(
            clear,
            root,
            request,
            &self.backend.packager,
            &self.segment_target_argument()?,
        )?;
        let secret = request.output_key.map(|k| k.0.key_hex());
        let signaling = self
            .output_signaling
            .as_ref()
            .filter(|value| !value.is_multi())
            .map(|value| {
                value.objects(
                    request
                        .output_key
                        .ok_or(ConversionError("output-key-required"))?,
                    request.target,
                )
            })
            .transpose()?;
        let multi_signaling = self
            .output_signaling
            .as_ref()
            .filter(|value| value.is_multi() && request.format == OutputFormat::Dash)
            .map(|value| {
                value.multi_objects(
                    request
                        .output_key
                        .ok_or(ConversionError("output-key-required"))?,
                    request.target,
                )
            })
            .transpose()?;
        if let (Some(key), Some(secret)) = (request.output_key, secret.as_ref()) {
            argv.extend([
                "--enable_raw_key_encryption".into(),
                "--protection_scheme".into(),
                request.target.as_str().into(),
                "--keys".into(),
                format!("label=:key_id={}:key={}", key.0.id_hex(), secret.as_str()),
            ]);
            if request.format == OutputFormat::Hls {
                if self
                    .output_signaling
                    .as_ref()
                    .is_some_and(|value| value.is_multi())
                {
                    argv.extend(["--protection_systems".into(), "FairPlay".into()]);
                }
                argv.extend([
                    "--hls_key_uri".into(),
                    request
                        .hls_key_uri
                        .ok_or(ConversionError("hls-key-uri-required"))?
                        .into(),
                ]);
            }
        }
        let secrets: Vec<&str> = secret
            .iter()
            .map(|s| s.as_str())
            .chain(request.hls_key_uri)
            .chain(signaling.iter().flat_map(Objects::secrets))
            .chain(multi_signaling.iter().flat_map(|value| value.secrets()))
            .collect();
        let pssh = signaling
            .as_ref()
            .map(|value| value.hex.as_str())
            .or_else(|| multi_signaling.as_ref().map(|value| value.hex.as_str()));
        let kinds: Vec<_> = request.tracks.iter().map(|track| track.kind).collect();
        let audio =
            super::packaging_clock::prepare(input_owner, clear, &kinds, &self.backend.stop)?;
        let mut progress =
            super::media_progress::Packaging::new(self.backend.activity.clone(), root, clear);
        self.run_joint_packager(&argv, pssh, &secrets, output_owner, progress.as_mut())?;
        if let Some(progress) = &progress {
            progress.finish();
        }
        super::files::seal(root)?;
        audio.restore(output_owner, root, request.format, &self.backend.stop)?;
        restore_packaged_windows(clear, root, request.format, &self.backend.stop)?;
        if let Some(signaling) = &signaling {
            signaling.finish(
                root,
                clear.len(),
                request
                    .output_key
                    .ok_or(ConversionError("output-key-required"))?,
            )?;
        }
        Ok(())
    }

    fn run_joint_packager(
        &self,
        argv: &[String],
        pssh: Option<&str>,
        secrets: &[&str],
        owner: &super::files::Stage,
        progress: Option<&mut super::media_progress::Packaging>,
    ) -> Result<(), ConversionError> {
        let mut backend = self.backend.clone();
        backend.runner = std::sync::Arc::new(
            self.backend
                .runner
                .cohort(
                    self.maximum_workers
                        .unwrap_or(crate::concurrency::DEFAULT_WORKERS_CAP),
                )
                .for_packaging(),
        );
        if let Some(progress) = progress {
            let mut monitor = || {
                if owner.directory_source().is_ok() {
                    progress.poll();
                }
            };
            if let Some(pssh) = pssh {
                super::signaling::flagfile::run_monitored(
                    &backend,
                    argv,
                    pssh,
                    secrets,
                    &mut monitor,
                )?;
            } else {
                backend.run_monitored(argv, "packager", secrets, &mut monitor)?;
            }
        } else if let Some(pssh) = pssh {
            super::signaling::flagfile::run(&backend, argv, pssh, secrets)?;
        } else {
            backend.run(argv, "packager", secrets)?;
        }
        Ok(())
    }
}
fn restore_packaged_windows(
    clear: &[PathBuf],
    root: &Path,
    format: OutputFormat,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    for (index, source) in clear.iter().enumerate() {
        super::finite::restore(
            source,
            0,
            &root.join(format!("track-{index:04}/init.mp4")),
            stop,
        )?;
    }
    super::finite::manifest::finish(root, clear, format, stop)
}
fn joint_arguments(
    clear: &[PathBuf],
    root: &Path,
    request: &TrackConversionRequest<'_>,
    packager: &str,
    segment_duration: &str,
) -> Result<Zeroizing<Vec<String>>, ConversionError> {
    let mut argv = Zeroizing::new(vec![
        packager.into(),
        "--mp4_reset_initial_composition_offset_to_zero=false".into(),
    ]);
    for (index, path) in clear.iter().enumerate() {
        let name = format!("track-{index:04}");
        let dir = root.join(&name);
        super::files::create_dir(&dir).map_err(|_| ConversionError("output-staging-failed"))?;
        let input = descriptor_path(path)?;
        let output = descriptor_path(&dir)?;
        argv.push(format!("input={input},stream=0,init_segment={output}/init.mp4,segment_template={output}/seg_$Number$.m4s,playlist_name={name}/media.m3u8"));
    }
    argv.extend([
        "--segment_duration".into(),
        segment_duration.into(),
        "--clear_lead".into(),
        "0".into(),
    ]);
    if request.format == OutputFormat::Dash {
        argv.extend([
            "--generate_static_live_mpd".into(),
            "--mpd_output".into(),
            descriptor_path(&root.join("manifest.mpd"))?,
        ]);
    } else {
        argv.extend([
            "--hls_playlist_type".into(),
            "VOD".into(),
            "--hls_master_playlist_output".into(),
            descriptor_path(&root.join("master.m3u8"))?,
        ]);
    }
    Ok(argv)
}
fn descriptor_path(path: &Path) -> Result<String, ConversionError> {
    let value = super::files::tool_path(path)?;
    if value.contains([',', '\n', '\r']) {
        return Err(ConversionError("unsupported-path"));
    }
    Ok(value)
}
fn validate_request(request: &TrackConversionRequest<'_>) -> Result<(), ConversionError> {
    if request.tracks.is_empty() || request.tracks.len() > 64 {
        return Err(ConversionError("track-count-limit"));
    }
    if request.format == OutputFormat::Mp4
        && (request.tracks.len() != 1 || request.tracks[0].index != 0)
        && request.tracks.len() != 2
    {
        return Err(ConversionError("multi-track-mp4-output-not-supported"));
    }
    if request.target == TargetProtection::HlsSampleAes {
        return Err(ConversionError("target-requires-ts-output"));
    }
    if request
        .hls_output_key
        .and_then(HlsOutputKey::associated_iv)
        .is_some()
    {
        return Err(ConversionError("output-iv-not-used"));
    }
    if request.target == TargetProtection::HlsAes128 && request.format != OutputFormat::Hls {
        return Err(ConversionError("target-requires-hls-output"));
    }
    if request.target == TargetProtection::Cenc && request.format == OutputFormat::Hls {
        return Err(ConversionError("cenc-hls-output-not-supported"));
    }
    if matches!(
        request.target,
        TargetProtection::Cenc | TargetProtection::Cbcs
    ) != request.output_key.is_some()
    {
        return Err(ConversionError("output-key-role-mismatch"));
    }
    if (request.target == TargetProtection::HlsAes128) != request.hls_output_key.is_some() {
        return Err(ConversionError("output-key-role-mismatch"));
    }
    if request.format == OutputFormat::Hls && request.target == TargetProtection::Cbcs {
        let uri = request
            .hls_key_uri
            .ok_or(ConversionError("hls-key-uri-required"))?;
        if uri.is_empty() || uri.chars().any(char::is_control) || uri.contains('"') {
            return Err(ConversionError("invalid-hls-key-uri"));
        }
    }
    Ok(())
}
/// Select the source policy for conversion or for comparing supported existing media.
#[derive(Clone, Copy, Debug)]
pub enum SourceValidationPurpose {
    Conversion,
    Verification,
}
fn prepare(
    source: &Path,
    index: usize,
    kind: TrackKind,
    keys: &InputKeys,
) -> Result<Prepared, ConversionError> {
    prepare_for(
        source,
        index,
        kind,
        keys,
        SourceValidationPurpose::Conversion,
    )
}
fn prepare_for(
    source: &Path,
    index: usize,
    kind: TrackKind,
    keys: &InputKeys,
    purpose: SourceValidationPurpose,
) -> Result<Prepared, ConversionError> {
    let report = inspection::inspect(source).map_err(|_| ConversionError("invalid-input-media"))?;
    if report.format != InputFormat::Mp4 || report.tracks.len() > 64 {
        return Err(ConversionError("input-format-not-supported"));
    }
    let audio_claim = super::audio_presentation::read_claim(source)?;
    let source_dictionary = audio_claim.is_none()
        && matches!(report.protection, Protection::Cenc | Protection::Cbcs)
        && match purpose {
            SourceValidationPurpose::Conversion => {
                super::progressive::validate_input(source, &report, keys)?
            }
            SourceValidationPurpose::Verification => {
                super::progressive::verification_input(source, &report, keys)?
            }
        };
    let track = report
        .tracks
        .get(index)
        .ok_or(ConversionError("stream-selector-mismatch"))?;
    if !matches!(kind, TrackKind::Audio | TrackKind::Video) || track.kind != kind {
        return Err(ConversionError("track-role-mismatch"));
    }
    if !matches!(
        track.protection,
        Protection::Clear | Protection::Cenc | Protection::Cbcs
    ) {
        return Err(ConversionError("input-protection-not-supported"));
    }
    audio::validate_sample_groups(source)?;
    let key_id = if track.protection == Protection::Clear {
        None
    } else {
        if track.key_ids.len() != 1 || !keys.contains(&track.key_ids[0]) {
            return Err(ConversionError("input-key-required"));
        }
        Some(track.key_ids[0].clone())
    };
    let selected = Inspection::new(
        InputFormat::Mp4,
        Evidence::ContainerMetadata,
        vec![track.clone()],
        report.drm_systems,
        Vec::new(),
    );
    Ok(Prepared {
        metadata: metadata::Metadata::default(),
        compatibility: Vec::new(),
        outer_protection: match track.protection {
            Protection::Clear => SourceProtection::Clear,
            Protection::Cenc => SourceProtection::Cenc,
            Protection::Cbcs => SourceProtection::Cbcs,
            _ => return Err(ConversionError("input-protection-not-supported")),
        },
        source_dictionary,
        source: source.into(),
        media: source.into(),
        index,
        report: selected,
        key_id,
        transport_index: None,
        transport_demux_view: None,
        transport_native: None,
        transport_sample: false,
        playlist_reference: None,
        dash_reference: false,
        audio_presentation: None,
        clock: None,
        hls_target_seconds: None,
        hls_video_segment_ms: None,
    })
}
fn key_for<'a>(source: &Prepared, keys: &'a InputKeys) -> Option<&'a ContentKey> {
    source
        .key_id
        .as_ref()
        .and_then(|id| keys.keys.iter().find(|k| k.id_hex() == *id))
}
fn raw_audio_view<'a>(source: &'a Prepared, keys: &'a InputKeys) -> super::ts::View<'a> {
    super::ts::View {
        input: &source.source,
        metadata: &source.source,
        index: source.transport_index.unwrap_or(source.index),
        key: key_for(source, keys),
        all_keys: None,
        transport: source.transport_index.is_some(),
        native: source.transport_native.as_deref(),
        unedited: source.transport_index.is_none(),
    }
    .with_all_keys(keys)
}
fn playback_metadata(source: &Prepared) -> &Path {
    if source.transport_index.is_some() {
        &source.media
    } else {
        &source.source
    }
}
fn secure_tree(root: &Path) -> Result<(), ConversionError> {
    super::files::seal(root)
}

/// Independent key roles for one actual local media input, never an inspection artifact.
#[derive(Clone, Copy)]
pub struct VerificationSource<'a> {
    pub input: &'a Path,
    pub input_keys: &'a InputKeys,
    pub hls_keys: &'a super::HlsInputKeys,
}
/// Compare every actual source and candidate stream with independent media and selection evidence.
#[derive(Clone, Copy)]
pub struct VerificationRequest<'a> {
    pub source: VerificationSource<'a>,
    pub output: VerificationSource<'a>,
}
/// Actual admitted source protection, including changes between HLS media segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceProtection {
    Clear,
    Cenc,
    Cbcs,
    HlsAes128,
    HlsSampleAes,
    Mixed,
}
impl SourceProtection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Cenc => "cenc",
            Self::Cbcs => "cbcs",
            Self::HlsAes128 => "hls-aes128",
            Self::HlsSampleAes => "hls-sample-aes",
            Self::Mixed => "mixed",
        }
    }
    /// A mixed source never satisfies a single output protection requirement.
    pub const fn matches_target(self, target: TargetProtection) -> bool {
        matches!(
            (self, target),
            (Self::Clear, TargetProtection::Clear)
                | (Self::Cenc, TargetProtection::Cenc)
                | (Self::Cbcs, TargetProtection::Cbcs)
                | (Self::HlsAes128, TargetProtection::HlsAes128)
                | (Self::HlsSampleAes, TargetProtection::HlsSampleAes)
        )
    }
}
impl From<TargetProtection> for SourceProtection {
    fn from(target: TargetProtection) -> Self {
        match target {
            TargetProtection::Clear => Self::Clear,
            TargetProtection::Cenc => Self::Cenc,
            TargetProtection::Cbcs => Self::Cbcs,
            TargetProtection::HlsAes128 => Self::HlsAes128,
            TargetProtection::HlsSampleAes => Self::HlsSampleAes,
        }
    }
}
/// Explicit input compatibility exceptions; these do not assert standards conformance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceCompatibilityNote {
    TsEmptyPayloadPadding,
    HlsEmptyCuePair,
}
impl SourceCompatibilityNote {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TsEmptyPayloadPadding => "ts-empty-payload-padding",
            Self::HlsEmptyCuePair => "hls-empty-cue-pair",
        }
    }
}
/// Complete source admission facts. This receipt never authorizes future execution.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SourceQualification {
    #[serde(skip)]
    pub(crate) layout_tracks: Vec<super::layout::QualifiedTrack>,
    #[serde(skip)]
    pub(in crate::conversion) hls_timing: Option<super::processing::segment_target::HlsTiming>,
    pub input: Inspection,
    pub actual_format: OutputFormat,
    pub track_protection: Vec<SourceProtection>,
    pub compatibility_notes: Vec<SourceCompatibilityNote>,
    pub eligible_formats: Vec<OutputFormat>,
    pub verification: VerificationReport,
}
impl Converter {
    fn qualified_hls_timing(
        &self,
        prepared: &[Prepared],
        source: InputFormat,
        purpose: SourceValidationPurpose,
    ) -> Result<Option<super::processing::segment_target::HlsTiming>, ConversionError> {
        if matches!(purpose, SourceValidationPurpose::Conversion) && self.inherits_hls_target() {
            prepared_hls_timing(prepared, source, &self.backend.stop)
        } else {
            Ok(None)
        }
    }

    /// Open and validate every local resource and execute complete media evidence checks.
    /// Conversion admission applies conversion's source policy; verification also permits
    /// supported existing DRM signaling. Output packaging and keys are revalidated later.
    pub fn validate_source(
        &self,
        input: VerificationSource<'_>,
        purpose: SourceValidationPurpose,
    ) -> Result<SourceQualification, ConversionError> {
        let qualified = self.validate_source_inner(input, purpose)?;
        if let Some(sink) = &self.backend.activity {
            sink.emit(
                serde_json::json!({
                    "stage": "conversion",
                    "event": "source-qualified",
                    "purpose": match purpose {
                        SourceValidationPurpose::Conversion => "conversion",
                        SourceValidationPurpose::Verification => "verification",
                    },
                })
                .as_object()
                .unwrap(),
            );
        }
        Ok(qualified)
    }

    pub(in crate::conversion) fn validate_source_preparing(
        &self,
        input: VerificationSource<'_>,
        purpose: SourceValidationPurpose,
        subject: super::media_progress::Subject,
    ) -> Result<SourceQualification, ConversionError> {
        let (backend, preparation) = super::media_progress::scoped(
            &self.backend,
            subject,
            super::media_progress::Purpose::Admission,
        );
        let mut command = self.with_processing_clone(self.processing_mode);
        command.backend = backend;
        let qualified = command.validate_source(input, purpose)?;
        preparation.finish();
        self.backend.check_cancelled()?;
        Ok(qualified)
    }

    fn validate_source_inner(
        &self,
        input: VerificationSource<'_>,
        purpose: SourceValidationPurpose,
    ) -> Result<SourceQualification, ConversionError> {
        if self.processing_mode == super::ProcessingMode::Segments {
            return super::segments::qualify(self, input, purpose);
        }
        let work = workspace()?;
        let original = self.inspect_input(input.input)?;
        let prepared = prepare_presentation_mode(
            &input,
            work.path(),
            &self.backend,
            self.maximum_workers,
            purpose,
            false,
        )?;
        let actual_format = if prepared.iter().all(|t| t.transport_index.is_some()) {
            OutputFormat::HlsTs
        } else {
            OutputFormat::from(original.format)
        };
        let track_protection = prepared.iter().map(|p| p.outer_protection).collect();
        let compatibility_notes = prepared
            .iter()
            .flat_map(|p| p.compatibility.iter().copied())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut eligible_formats = Vec::new();
        let declared_audio = prepared
            .iter()
            .any(|track| track.audio_presentation.is_some());
        if !declared_audio && original.format == InputFormat::Mp4 {
            let first = &prepared[0];
            let report = inspection::inspect(&first.media)
                .map_err(|_| ConversionError("invalid-input-media"))?;
            if super::finite::has_audio(&first.media)? && !audio::movie(&first.media)?.fragmented {
                let finite = match purpose {
                    SourceValidationPurpose::Conversion => {
                        super::progressive::validate_input(&first.media, &report, input.input_keys)?
                    }
                    SourceValidationPurpose::Verification => {
                        super::progressive::verification_input(
                            &first.media,
                            &report,
                            input.input_keys,
                        )?
                    }
                };
                if finite {
                    self.progressive_codecs(
                        &first.media,
                        &report,
                        (report.protection != Protection::Clear)
                            .then_some(super::keys::ProbeKeys::All(input.input_keys)),
                    )?;
                    eligible_formats.push(OutputFormat::Mp4);
                }
            } else if prepared.len() == 1 {
                eligible_formats.push(OutputFormat::Mp4);
            }
        } else if !declared_audio
            && actual_format != OutputFormat::HlsTs
            && prepared.len() == 1
            && prepared[0].metadata.audio.is_none()
            && prepared[0].metadata.audio_group.is_none()
        {
            eligible_formats.push(OutputFormat::Mp4);
        }
        if metadata::dash_representable(&prepared) {
            eligible_formats.push(OutputFormat::Dash);
        }
        if prepared
            .iter()
            .filter(|p| p.report.tracks[0].kind == TrackKind::Audio)
            .count()
            <= 1
        {
            eligible_formats.push(OutputFormat::Hls);
        }
        if actual_format == OutputFormat::HlsTs || ts_output::fragmented_sources(&prepared)? {
            eligible_formats.push(OutputFormat::HlsTs);
        }
        if eligible_formats.contains(&OutputFormat::Mp4)
            && !self.standalone_clocks_supported(&prepared, input.input_keys)?
        {
            eligible_formats.retain(|format| *format != OutputFormat::Mp4);
        }
        let verification = self.validate_prepared_sources(&prepared, input.input_keys)?;
        if !verification.passed {
            return Err(ConversionError("source-media-verification-failed"));
        }
        self.backend.checkpoint(&[&work]);
        Ok(SourceQualification {
            layout_tracks: qualified_layout_tracks(&prepared, original.format),
            hls_timing: self.qualified_hls_timing(&prepared, original.format, purpose)?,
            input: original,
            actual_format,
            track_protection,
            compatibility_notes,
            eligible_formats,
            verification,
        })
    }
    fn validate_prepared_sources(
        &self,
        prepared: &[Prepared],
        input_keys: &InputKeys,
    ) -> Result<VerificationReport, ConversionError> {
        let verified = self.run_tracks(prepared.len(), |ordinal| {
            let track = &prepared[ordinal];
            metadata::pair(&track.metadata, &track.metadata)?;
            if self.backend.validation_depth == super::ValidationDepth::Native {
                if let Some(index) = track.transport_index {
                    return self.backend.validate_transport_source(
                        &track.source,
                        index,
                        track
                            .transport_native
                            .as_deref()
                            .ok_or(ConversionError("ts-original-evidence-required"))?,
                    );
                }
                return self.backend.native_validate_mp4_with_clock(
                    &track.media,
                    track.index,
                    Some(super::keys::ProbeKeys::All(input_keys)),
                    track.clock,
                );
            }
            // Source admission checks each known track directly. Matching decoded
            // content is only needed when comparing independently prepared outputs.
            if let Some(index) = track.transport_index
                && track.audio_presentation.is_none()
            {
                let native = track
                    .transport_native
                    .as_deref()
                    .ok_or(ConversionError("ts-original-evidence-required"))?;
                self.backend
                    .validate_transport_source(&track.source, index, native)
            } else {
                self.verify_prepared_pair(track, track, input_keys, input_keys)
            }
        })?;
        Ok(combine_verification(
            self.backend.validation_depth,
            super::VerificationScope::SourceAdmission,
            verified
                .into_iter()
                .map(VerificationReport::into_source_admission)
                .collect::<Result<Vec<_>, _>>()?,
        ))
    }

    fn standalone_clocks_supported(
        &self,
        prepared: &[Prepared],
        keys: &InputKeys,
    ) -> Result<bool, ConversionError> {
        for track in prepared.iter().filter(|track| track.clock.is_some()) {
            match self.backend.native_validate_mp4(
                &track.media,
                track.index,
                Some(super::keys::ProbeKeys::All(keys)),
            ) {
                Ok(_) => {}
                Err(ConversionError("native-aac-clock-mismatch")) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    fn validate_complete_clock_output(
        &self,
        request: &ConversionRequest<'_>,
        prepared: &[Prepared],
    ) -> Result<(), ConversionError> {
        if request.format != OutputFormat::Mp4 {
            return Ok(());
        }
        if prepared
            .iter()
            .any(|source| source.audio_presentation.is_some())
        {
            return Err(ConversionError("audio-presentation-range-not-supported"));
        }
        if !self.standalone_clocks_supported(prepared, request.input_keys)? {
            return Err(ConversionError("mp4-output-clock-profile-not-supported"));
        }
        Ok(())
    }
}
fn prepared_hls_timing(
    prepared: &[Prepared],
    source: InputFormat,
    stop: &crate::concurrency::Stop,
) -> Result<Option<super::processing::segment_target::HlsTiming>, ConversionError> {
    use super::processing::segment_target::{HlsTiming, cadence};
    if source != InputFormat::Hls {
        return Ok(None);
    }
    let seconds = prepared
        .iter()
        .map(|track| {
            track
                .hls_target_seconds
                .ok_or(ConversionError("hls-invalid-target-duration"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let video = prepared
        .iter()
        .filter(|track| track.report.tracks[0].kind == TrackKind::Video)
        .map(|track| {
            track.transport_native.as_deref().map_or_else(
                || Ok(track.hls_video_segment_ms),
                |native| cadence::transport(native, stop),
            )
        })
        .collect::<Result<Vec<_>, ConversionError>>()?;
    HlsTiming::from_admitted(seconds, video).map(Some)
}

fn qualified_layout_tracks(
    prepared: &[Prepared],
    format: InputFormat,
) -> Vec<super::layout::QualifiedTrack> {
    prepared
        .iter()
        .enumerate()
        .map(|(ordinal, track)| super::layout::QualifiedTrack {
            kind: track.report.tracks[0].kind,
            selector: if format == InputFormat::Dash {
                ordinal
            } else {
                track.transport_index.unwrap_or(track.index)
            },
        })
        .collect()
}
impl Converter {
    pub fn verify(
        &self,
        request: VerificationRequest<'_>,
    ) -> Result<VerificationReport, ConversionError> {
        if self.processing_mode == super::ProcessingMode::Segments {
            return super::segments::verify(self, request);
        }
        self.verify_presentation(request, None)
    }

    /// Complete-track decoding is only an additional proof for admitted segments.
    /// Keep their original output intent when checking the saved source placement.
    pub(in crate::conversion) fn verify_admitted_segments(
        &self,
        request: VerificationRequest<'_>,
        format: OutputFormat,
        native: VerificationReport,
    ) -> Result<VerificationReport, ConversionError> {
        if self.backend.validation_depth == super::ValidationDepth::Native {
            return Ok(native);
        }
        let mut decoded =
            self.verify_presentation(request, Some((format, super::ProcessingMode::Segments)))?;
        decoded.require_native(&native)?;
        Ok(decoded)
    }

    fn verify_presentation(
        &self,
        request: VerificationRequest<'_>,
        layout_intent: Option<(OutputFormat, super::ProcessingMode)>,
    ) -> Result<VerificationReport, ConversionError> {
        let work = workspace()?;
        let source_dir = work.path().join("source");
        let result_dir = work.path().join("result");
        super::files::create_dir(&source_dir)
            .map_err(|_| ConversionError("temporary-workspace-unavailable"))?;
        super::files::create_dir(&result_dir)
            .map_err(|_| ConversionError("temporary-workspace-unavailable"))?;
        let preparation = self
            .backend
            .scope(super::measurement::Phase::InputPreparation);
        let (source_backend, source_preparation) = super::media_progress::scoped(
            &self.backend,
            super::media_progress::Subject::Source,
            super::media_progress::Purpose::Verification,
        );
        let source = prepare_presentation(
            &request.source,
            &source_dir,
            &source_backend,
            self.maximum_workers,
            SourceValidationPurpose::Verification,
        )?;
        source_preparation.finish();
        self.backend.check_cancelled()?;
        let source_format = self.inspect_input(request.source.input)?.format;
        let (format, processing) = match layout_intent {
            Some(intent) => intent,
            None => (
                self.default_output_format(request.output.input)?,
                super::ProcessingMode::Stitch,
            ),
        };
        self.check_admitted_output_options(
            request.source.input,
            source_format,
            &qualified_layout_tracks(&source, source_format),
            format,
            processing,
        )?;
        let checked = self.checked_output_manifest(request.output.input)?;
        let (result_backend, result_preparation) = super::media_progress::scoped(
            &self.backend,
            super::media_progress::Subject::Result,
            super::media_progress::Purpose::Verification,
        );
        let result = prepare_presentation(
            &request.output,
            &result_dir,
            &result_backend,
            self.maximum_workers,
            SourceValidationPurpose::Verification,
        )?;
        result_preparation.finish();
        self.backend.check_cancelled()?;
        drop(preparation);
        let mut report = self.compare_presentations(
            source,
            result,
            request.source.input_keys,
            request.output.input_keys,
        )?;
        if let Some(checked) = checked {
            checked.check(self)?;
            report.passed &= checked.report.is_clean_for_scope();
            report.native_manifest = Some(checked.report);
        }
        self.backend.checkpoint(&[&work]);
        Ok(report)
    }
    fn presentation_digest(
        &self,
        track: &Prepared,
        keys: &InputKeys,
    ) -> Result<String, ConversionError> {
        if self.backend.validation_depth == super::ValidationDepth::Native {
            if let Some(native) = track.transport_native.as_deref() {
                native.validate()?;
                return Ok(hex::encode(native.evidence()?.payload_digest()));
            }
            return self.backend.native_payload_digest(
                &track.media,
                track.index,
                Some(super::keys::ProbeKeys::All(keys)),
                track.clock,
            );
        }
        self.backend.hash_stream_keys_options(
            &track.source,
            track.transport_index.unwrap_or(track.index),
            false,
            if track.source_dictionary {
                Some(super::keys::ProbeKeys::All(keys))
            } else {
                key_for(track, keys).map(super::keys::ProbeKeys::One)
            },
            track.audio_presentation.is_some() && track.transport_index.is_none(),
        )
    }
    fn presentation_payload_digest(
        &self,
        track: &Prepared,
        keys: &InputKeys,
    ) -> Result<String, ConversionError> {
        if self.backend.validation_depth == super::ValidationDepth::Native {
            return self.presentation_digest(track, keys);
        }
        self.backend.hash_stream_keys(
            &track.media,
            track.index,
            true,
            if track.source_dictionary {
                Some(super::keys::ProbeKeys::All(keys))
            } else {
                key_for(track, keys).map(super::keys::ProbeKeys::One)
            },
        )
    }
    fn match_ambiguous_track(
        &self,
        source: &Prepared,
        candidates: &[usize],
        available: &[Option<Prepared>],
        keys: (&InputKeys, &InputKeys),
        payloads: &mut [Option<String>],
    ) -> Result<usize, ConversionError> {
        if candidates.is_empty() {
            return Err(ConversionError("presentation-track-content-mismatch"));
        }
        let source_payload = self.presentation_payload_digest(source, keys.0)?;
        let mut preferred = candidates.to_vec();
        for &index in candidates {
            if payloads[index].is_none() {
                payloads[index] = Some(self.presentation_payload_digest(
                    available[index].as_ref().expect("unused candidate"),
                    keys.1,
                )?);
            }
        }
        // Normalized payload hashes only order candidates. The original-source
        // complete proof below remains mandatory before consuming any candidate.
        preferred.sort_by_key(|&index| payloads[index].as_ref() != Some(&source_payload));
        let mut metadata_matched = false;
        for index in preferred {
            let result = available[index].as_ref().expect("unused candidate");
            match metadata::pair(&source.metadata, &result.metadata) {
                Ok(()) => {}
                Err(ConversionError("presentation-metadata-mismatch")) => continue,
                Err(error) => return Err(error),
            }
            metadata_matched = true;
            // Operational failures always propagate; only a completed negative
            // media proof means this candidate does not represent the source.
            if self
                .verify_prepared_pair(source, result, keys.0, keys.1)?
                .passed
            {
                return Ok(index);
            }
        }
        Err(ConversionError(if metadata_matched {
            "presentation-track-content-mismatch"
        } else {
            "presentation-metadata-mismatch"
        }))
    }
    fn match_presentations(
        &self,
        source: &[Prepared],
        mut result: Vec<Prepared>,
        source_keys: &InputKeys,
        result_keys: &InputKeys,
    ) -> Result<Vec<Prepared>, ConversionError> {
        if source
            .iter()
            .filter(|p| p.report.tracks[0].kind == TrackKind::Video)
            .count()
            > 1
        {
            let result_digests = self.run_tracks(result.len(), |index| {
                self.presentation_digest(&result[index], result_keys)
            })?;
            let result_kinds: Vec<_> = result.iter().map(|p| p.report.tracks[0].kind).collect();
            let mut available = result.into_iter().map(Some).collect::<Vec<_>>();
            let mut payloads = vec![None; available.len()];
            result = Vec::with_capacity(source.len());
            for a in source {
                let kind = a.report.tracks[0].kind;
                let digest = self.presentation_digest(a, source_keys)?;
                let candidates: Vec<_> = available
                    .iter()
                    .enumerate()
                    .filter_map(|(index, b)| {
                        b.as_ref()
                            .filter(|b| {
                                b.report.tracks[0].kind == kind && result_digests[index] == digest
                            })
                            .map(|_| index)
                    })
                    .collect();
                let ambiguous = result_digests
                    .iter()
                    .enumerate()
                    .filter(|(index, d)| result_kinds[*index] == kind && **d == digest)
                    .count()
                    > 1;
                let found = if ambiguous {
                    self.match_ambiguous_track(
                        a,
                        &candidates,
                        &available,
                        (source_keys, result_keys),
                        &mut payloads,
                    )?
                } else {
                    *candidates
                        .first()
                        .ok_or(ConversionError("presentation-track-content-mismatch"))?
                };
                result.push(available[found].take().expect("matched unused track"));
            }
        }
        Ok(result)
    }
    fn verify_prepared_pair(
        &self,
        a: &Prepared,
        b: &Prepared,
        source_keys: &InputKeys,
        result_keys: &InputKeys,
    ) -> Result<VerificationReport, ConversionError> {
        let native = self.native_prepared_pair(a, b, source_keys, result_keys)?;
        if self.backend.validation_depth == super::ValidationDepth::Native {
            return Ok(native);
        }
        let mut report = self.verify_prepared_pair_decoded(a, b, source_keys, result_keys)?;
        report.require_native(&native)?;
        Ok(report)
    }
    fn verify_prepared_pair_decoded(
        &self,
        a: &Prepared,
        b: &Prepared,
        source_keys: &InputKeys,
        result_keys: &InputKeys,
    ) -> Result<VerificationReport, ConversionError> {
        if a.audio_presentation.is_some() || b.audio_presentation.is_some() {
            let (Some(first), Some(second)) = (a.audio_presentation, b.audio_presentation) else {
                return Err(super::audio_presentation::MISMATCH);
            };
            let controls_equal = super::progressive::playback_controls_equal(
                playback_metadata(a),
                a.index,
                playback_metadata(b),
                b.index,
            )?;
            let source = raw_audio_view(a, source_keys);
            let result = raw_audio_view(b, result_keys);
            let mut proof = self.backend.verify_transport(source, result)?;
            let range_equal = self
                .backend
                .verify_audio_presentation(source, result, first, second)?;
            if !controls_equal {
                proof.checks.stream_metadata = super::CheckOutcome::Fail;
            }
            proof.checks.packet_timing_equal &= first == second;
            if let super::DecodeEvidence::ContentCompared { equal } = &mut proof.decode_evidence {
                *equal &= range_equal;
            }
            proof.passed &= controls_equal && first == second && range_equal;
            proof.refresh_passed();
            return Ok(proof);
        }
        if b.transport_index.is_some() {
            return if a.transport_index.is_some() {
                self.verify_transport_pair(a, b)
            } else {
                self.backend.verify_transport(
                    raw_audio_view(a, source_keys),
                    raw_audio_view(b, result_keys),
                )
            };
        }
        if let Some(index) = a.transport_index {
            if b.playlist_reference.is_none() && !b.dash_reference {
                return Err(ConversionError(
                    "ts-verification-output-profile-not-supported",
                ));
            }
            let (result_input, result_key) = b.playlist_reference.as_ref().map_or_else(
                || (&b.source, key_for(b, result_keys)),
                |playlist| (playlist, None),
            );
            return self.backend.verify_transport(
                super::ts::View {
                    input: &a.source,
                    metadata: &a.source,
                    index,
                    key: None,
                    all_keys: None,
                    transport: true,
                    native: a.transport_native.as_deref(),
                    unedited: false,
                }
                .with_all_keys(source_keys),
                super::ts::View {
                    input: result_input,
                    // This HLS decoder reads its private key URI. MP4/DASH
                    // discovery instead needs the complete track-key dictionary.
                    metadata: result_input,
                    index: b.index,
                    key: result_key,
                    all_keys: b.playlist_reference.is_none().then_some(result_keys),
                    transport: false,
                    native: None,
                    unedited: false,
                },
            );
        }
        let a_view = StreamView::new(&a.source, a.index, key_for(a, source_keys))
            .with_metadata(&a.media)
            .with_clock(a.clock);
        let b_view = StreamView::new(&b.source, b.index, key_for(b, result_keys))
            .with_metadata(&b.media)
            .with_clock(b.clock);
        self.backend.verify_views(
            a_view.with_all_keys(source_keys),
            b_view.with_all_keys(result_keys),
        )
    }
    fn compare_presentations(
        &self,
        mut source: Vec<Prepared>,
        mut result: Vec<Prepared>,
        source_keys: &InputKeys,
        result_keys: &InputKeys,
    ) -> Result<VerificationReport, ConversionError> {
        source.sort_by_key(|p| kind_order(p.report.tracks[0].kind));
        result.sort_by_key(|p| kind_order(p.report.tracks[0].kind));
        if source.len() != result.len() {
            return Err(ConversionError("presentation-track-count-mismatch"));
        }
        if source
            .iter()
            .zip(&result)
            .any(|(a, b)| a.report.tracks[0].kind != b.report.tracks[0].kind)
        {
            return Err(ConversionError("presentation-track-role-mismatch"));
        }
        result = self.match_presentations(&source, result, source_keys, result_keys)?;
        for (a, b) in source.iter().zip(&result) {
            metadata::pair(&a.metadata, &b.metadata)?;
        }
        let weights: Option<Vec<_>> = self.backend.activity.as_ref().and_then(|_| {
            source
                .iter()
                .zip(&result)
                .map(|(a, b)| {
                    super::media_progress::file_bytes(&a.media)?
                        .checked_add(super::media_progress::file_bytes(&b.media)?)
                })
                .collect()
        });
        let progress = weights.and_then(|weights| {
            super::media_progress::Batch::comparison(self.backend.activity.clone(), weights)
        });
        let tasks = super::media_progress::Batch::units(
            self.backend.activity.clone(),
            super::media_progress::Operation::Comparison,
            super::media_progress::Subject::Result,
            super::media_progress::Purpose::Verification,
            super::media_progress::Unit::Tracks,
            source.len(),
        );
        if let Some(tasks) = &tasks {
            for (ordinal, source) in source.iter().enumerate() {
                if let Some(track) = source.report.tracks.first() {
                    tasks.describe(ordinal, track.kind, source.metadata.resolution);
                }
            }
        }
        let verified_tracks = self.run_tracks(source.len(), |ordinal| {
            let task = tasks.as_ref().and_then(|tasks| tasks.start(ordinal));
            let report = self.verify_prepared_pair(
                &source[ordinal],
                &result[ordinal],
                source_keys,
                result_keys,
            )?;
            if let Some(progress) = &progress {
                progress.completed(ordinal);
            }
            if let Some(task) = task {
                task.finish();
            }
            Ok(report)
        })?;
        if let Some(tasks) = &tasks {
            tasks.finish();
        }
        if let Some(progress) = &progress {
            // Completion counts finished comparisons; their reports still
            // determine whether the media actually satisfies every check.
            progress.finish();
        }
        Ok(combine_verification(
            self.backend.validation_depth,
            super::VerificationScope::Comparison,
            verified_tracks,
        ))
    }
}
fn combine_verification(
    required_depth: super::ValidationDepth,
    scope: super::VerificationScope,
    verified: impl IntoIterator<Item = VerificationReport>,
) -> VerificationReport {
    VerificationReport::combine(required_depth, scope, verified)
}

fn describe_transport(
    backend: &super::backend::Backend,
    report: &mut ConversionReport,
    sample_transport: bool,
    dash: bool,
) {
    report.processing = Some(backend.processing(
        if dash {
            inspection::ProcessingRouteId::MpegTsDash
        } else if sample_transport {
            inspection::ProcessingRouteId::MpegTsSampleAesHls
        } else {
            inspection::ProcessingRouteId::MpegTsHls
        },
        if sample_transport {
            inspection::InputPreparation::MpegTsSampleAesDecrypt
        } else {
            inspection::InputPreparation::MpegTsDemux
        },
    ));
}
fn encrypt_packaged_hls(
    root: &Path,
    stage: &Path,
    tracks: usize,
    key: &super::HlsOutputKey,
) -> Result<(), ConversionError> {
    for ordinal in 0..tracks {
        let name = format!("track-{ordinal:04}");
        let output = stage.join(&name);
        super::files::create_dir(&output).map_err(|_| ConversionError("output-staging-failed"))?;
        hls::encrypt_media(&root.join(name), &output, key)?;
    }
    copy_private(&root.join("master.m3u8"), &stage.join("master.m3u8"))
}
fn kind_order(kind: TrackKind) -> u8 {
    match kind {
        TrackKind::Video => 0,
        TrackKind::Audio => 1,
        TrackKind::Text => 2,
        TrackKind::Unknown => 3,
    }
}
fn prepare_transport(
    localized: &hls::Localized,
    admission: &super::ts::Admission,
    directory: &Path,
    keys: &InputKeys,
    backend: &super::backend::Backend,
    maximum_workers: Option<usize>,
) -> Result<Vec<Prepared>, ConversionError> {
    let mut prepared = Vec::new();
    let streams = backend.transport_streams(&localized.combined, admission, &localized.native)?;
    let clock_index = streams
        .iter()
        .find(|(_, kind)| *kind == TrackKind::Video)
        .unwrap_or(&streams[0])
        .0;
    let native_presentation = {
        let pid = admission
            .tracks
            .get(clock_index)
            .ok_or(ConversionError("ts-stream-count-mismatch"))?
            .pid;
        super::native::durations::transport(
            localized
                .native
                .get(&pid)
                .ok_or(ConversionError("ts-original-evidence-required"))?,
            &localized.transport_segments,
        )?
    };
    let audio_presentation = if backend.validation_depth == super::ValidationDepth::Native {
        native_presentation
    } else {
        let decoded = backend.transport_durations(
            &localized.transport_segments,
            clock_index,
            maximum_workers,
        )?;
        if decoded != native_presentation {
            return Err(ConversionError("ts-segment-duration-mismatch"));
        }
        decoded
    };
    let projection = directory.join("demux-view.ts");
    let demux_source = if admission.has_empty_payload_padding() {
        super::ts::demux_view(&localized.combined, &projection, admission, &backend.stop)?;
        &projection
    } else {
        &localized.combined
    };
    for (index, kind) in streams {
        let pid = admission
            .tracks
            .iter()
            .find(|track| track.kind == kind)
            .ok_or(ConversionError("ts-stream-count-mismatch"))?
            .pid;
        let native = localized.native.get(&pid).cloned();
        if let Some(anchor) = &native {
            backend.confirm_native_transport(super::ts::View {
                input: &localized.combined,
                metadata: &localized.combined,
                index,
                key: None,
                all_keys: None,
                transport: true,
                native: Some(anchor),
                unedited: false,
            })?;
        }
        let root = directory.join(format!("demux-{index}"));
        super::files::create_dir(&root)?;
        let media = root.join("track.mp4");
        backend.demux_transport(demux_source, kind, &media, native.as_deref())?;
        let mut track = prepare(&media, 0, kind, keys)?;
        track.source.clone_from(&localized.combined);
        track.transport_index = Some(index);
        track.transport_demux_view = admission
            .has_empty_payload_padding()
            .then(|| projection.clone());
        if admission.has_empty_payload_padding() {
            track
                .compatibility
                .push(SourceCompatibilityNote::TsEmptyPayloadPadding);
        }
        track.audio_presentation = audio_presentation.filter(|_| kind == TrackKind::Audio);
        track.clock = native
            .as_deref()
            .map(super::native::ManifestAacClock::from_transport)
            .transpose()?
            .flatten();
        track.transport_native = native;
        track.transport_sample = localized.sample_transport;
        prepared.push(track);
    }
    Ok(prepared)
}
fn bind_audio_presentation(
    track: &mut Prepared,
    backend: &super::backend::Backend,
    keys: &InputKeys,
    directory: &Path,
) -> Result<(), ConversionError> {
    let (view, claim) = {
        let resource_claim = crate::resources::Claim::for_workload(
            backend.runner.registered_workload().unwrap_or_default(),
            crate::toolrun::DEFAULT_STDOUT_BYTES,
            crate::toolrun::DEFAULT_STDERR_BYTES,
        );
        let _permit = backend
            .runner
            .native_permit(resource_claim, &backend.stop, backend.timeout)
            .map_err(|error| match error {
                crate::resources::ReservationError::Cancelled => ConversionError("cancelled"),
                crate::resources::ReservationError::Insufficient => {
                    ConversionError("resource-budget")
                }
                crate::resources::ReservationError::Timeout => {
                    ConversionError("media-tool-timeout")
                }
            })?;
        let Some(claim) = super::audio_presentation::read_claim(&track.media)? else {
            return Ok(());
        };
        let view = directory.join(format!("raw-audio-{}.mp4", track.index));
        copy_private(&track.media, &view)?;
        // Inspect all coded frames separately from the declared final AAC selection.
        super::audio_presentation::strip_claim(&view, claim, &backend.stop)?;
        (view, claim)
    };
    // Evidence readers acquire their own permit; retaining the prefix permit would deadlock at one CPU.
    let evidence = backend.audio_evidence_with_clock(
        super::ts::View {
            input: &view,
            metadata: &view,
            index: track.index,
            key: key_for(track, keys),
            all_keys: None,
            transport: false,
            native: None,
            unedited: true,
        }
        .with_all_keys(keys),
        track.clock,
    )?;
    let range = claim.bind(evidence)?;
    if super::audio_presentation::read(&track.media, evidence)? != Some(range) {
        return Err(super::audio_presentation::MISMATCH);
    }
    track.audio_presentation = Some(range);
    track.media = view;
    Ok(())
}
fn prepare_presentation(
    input: &VerificationSource<'_>,
    stage: &Path,
    backend: &super::backend::Backend,
    maximum_workers: Option<usize>,
    purpose: SourceValidationPurpose,
) -> Result<Vec<Prepared>, ConversionError> {
    let normalize = !matches!(purpose, SourceValidationPurpose::Verification);
    prepare_presentation_mode(input, stage, backend, maximum_workers, purpose, normalize)
}
fn prepare_presentation_mode(
    input: &VerificationSource<'_>,
    stage: &Path,
    backend: &super::backend::Backend,
    maximum_workers: Option<usize>,
    purpose: SourceValidationPurpose,
    normalize: bool,
) -> Result<Vec<Prepared>, ConversionError> {
    let metadata =
        fs::symlink_metadata(input.input).map_err(|_| ConversionError("input-unreadable"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ConversionError("input-not-regular-file"));
    }
    let report =
        inspection::inspect(input.input).map_err(|_| ConversionError("invalid-input-media"))?;
    match report.format {
        InputFormat::Mp4 => {
            if super::audio_presentation::read_claim(input.input)?.is_some() {
                return Err(ConversionError("audio-presentation-range-not-supported"));
            }
            let path = stage.join("source.mp4");
            copy_private(input.input, &path)?;
            let actual =
                inspection::inspect(&path).map_err(|_| ConversionError("invalid-input-media"))?;
            if actual.tracks.is_empty() || actual.tracks.len() > 64 {
                return Err(ConversionError("track-count-limit"));
            }
            let progress = backend.preparation();
            if let Some(progress) = progress {
                progress.begin(actual.tracks.len(), super::media_progress::Unit::Tracks);
            }
            backend.check_cancelled()?;
            actual
                .tracks
                .iter()
                .enumerate()
                .map(|(index, track)| {
                    let task = progress.and_then(|progress| progress.start(index));
                    if let Some(task) = &task {
                        task.describe(track.kind, None);
                    }
                    let prepared =
                        prepare_for(&path, index, track.kind, input.input_keys, purpose)?;
                    if let Some(progress) = progress {
                        progress.completed(index);
                    }
                    if let Some(task) = task {
                        task.finish();
                    }
                    backend.check_cancelled()?;
                    Ok(prepared)
                })
                .collect()
        }
        InputFormat::Hls => {
            prepare_hls_presentation(input, stage, backend, maximum_workers, purpose, normalize)
        }
        InputFormat::Dash => {
            dash_preparation::prepare(input, stage, backend, maximum_workers, purpose)
        }
    }
}
fn prepare_hls_presentation(
    input: &VerificationSource<'_>,
    stage: &Path,
    backend: &super::backend::Backend,
    maximum_workers: Option<usize>,
    purpose: SourceValidationPurpose,
    normalize: bool,
) -> Result<Vec<Prepared>, ConversionError> {
    hls_preparation::prepare(input, stage, backend, maximum_workers, purpose, normalize)
}
fn prepare_native_hls_transport(
    input: &VerificationSource<'_>,
    media: &MediaPath,
    directory: &Path,
    backend: &super::backend::Backend,
    normalize: bool,
    renditions: usize,
) -> Result<Option<Vec<Prepared>>, ConversionError> {
    if backend.validation_depth != super::ValidationDepth::Native || normalize {
        return Ok(None);
    }
    let Some(localized) =
        hls::localize_native_transport(&media.path, directory, input.hls_keys, backend)?
    else {
        return Ok(None);
    };
    let admission = localized
        .transport
        .as_ref()
        .ok_or(ConversionError("ts-original-evidence-required"))?;
    validate_transport_roles(media, admission, renditions)?;
    let mut prepared = native_preparation::transport_segments(&localized, admission, backend)?;
    apply_hls_target(&mut prepared, localized.target_seconds);
    apply_transport_metadata_values(
        &mut prepared,
        media,
        transport_protection(localized.sample_transport, localized.report.protection),
        localized.empty_cue_pair,
        backend,
        input.input_keys,
    )?;
    Ok(Some(prepared))
}
fn apply_hls_target(prepared: &mut [Prepared], seconds: u64) {
    for track in prepared {
        track.hls_target_seconds = Some(seconds);
    }
}
fn validate_transport_roles(
    media: &MediaPath,
    admission: &super::ts::Admission,
    renditions: usize,
) -> Result<(), ConversionError> {
    media.validate_audio_variant(admission.tracks.iter().map(|track| track.kind))?;
    if (renditions != 1 && admission.tracks.len() != 1)
        || media
            .role
            .is_some_and(|kind| admission.tracks.len() != 1 || admission.tracks[0].kind != kind)
    {
        return Err(ConversionError("ts-presentation-layout-not-supported"));
    }
    Ok(())
}
fn transport_protection(sample: bool, declared: Protection) -> SourceProtection {
    match (sample, declared) {
        (true, _) => SourceProtection::HlsSampleAes,
        (_, Protection::HlsAes128) => SourceProtection::HlsAes128,
        (_, Protection::Mixed) => SourceProtection::Mixed,
        _ => SourceProtection::Clear,
    }
}
fn apply_transport_metadata(
    prepared: &mut [Prepared],
    media: &MediaPath,
    localized: &hls::Localized,
    backend: &super::backend::Backend,
    keys: &InputKeys,
) -> Result<(), ConversionError> {
    apply_transport_metadata_values(
        prepared,
        media,
        transport_protection(localized.sample_transport, localized.report.protection),
        localized.empty_cue_pair,
        backend,
        keys,
    )
}
fn apply_transport_metadata_values(
    prepared: &mut [Prepared],
    media: &MediaPath,
    protection: SourceProtection,
    empty_cue_pair: bool,
    backend: &super::backend::Backend,
    keys: &InputKeys,
) -> Result<(), ConversionError> {
    for track in prepared {
        track.metadata = media.metadata.clone();
        // A muxed variant's display size describes its video stream only.
        if track.report.tracks[0].kind == TrackKind::Audio {
            track.metadata.resolution = None;
        }
        if empty_cue_pair {
            track
                .compatibility
                .push(SourceCompatibilityNote::HlsEmptyCuePair);
        }
        track.outer_protection = protection;
        metadata::validate_profile(track, backend, key_for(track, keys))?;
    }
    Ok(())
}
#[derive(Debug)]
pub(in crate::conversion) struct MediaPath {
    pub path: PathBuf,
    pub role: Option<TrackKind>,
    pub variants: usize,
    pub metadata: metadata::Metadata,
    pub audio_variant_alias: bool,
}
impl MediaPath {
    pub(in crate::conversion) fn validate_audio_variant(
        &self,
        roles: impl IntoIterator<Item = TrackKind>,
    ) -> Result<(), ConversionError> {
        if !self.audio_variant_alias {
            return Ok(());
        }
        let mut roles = roles.into_iter();
        if roles.next() == Some(TrackKind::Audio) && roles.next().is_none() {
            Ok(())
        } else {
            Err(ConversionError("hls-audio-association-not-supported"))
        }
    }
}
fn validate_variant(attrs: &crate::m3u8::Attributes) -> Result<(), ConversionError> {
    if attrs.iter().any(|(name, _)| {
        !matches!(
            name,
            "BANDWIDTH"
                | "AVERAGE-BANDWIDTH"
                | "CODECS"
                | "RESOLUTION"
                | "FRAME-RATE"
                | "AUDIO"
                | "CLOSED-CAPTIONS"
        )
    }) || attrs.get("CLOSED-CAPTIONS").is_some_and(|v| v != "NONE")
    {
        return Err(ConversionError("hls-variant-metadata-not-supported"));
    }
    Ok(())
}
fn variant_role(attrs: &crate::m3u8::Attributes) -> Option<TrackKind> {
    attrs.get("CODECS").map(|codecs| {
        if !codecs.is_empty()
            && codecs
                .split(',')
                .all(|codec| codec.trim().starts_with("mp4a."))
        {
            TrackKind::Audio
        } else {
            TrackKind::Video
        }
    })
}
pub(in crate::conversion) fn hls_media_paths(
    path: &Path,
) -> Result<Vec<MediaPath>, ConversionError> {
    let bytes = super::files::read_manifest(path)
        .map_err(|_| ConversionError("hls-manifest-unreadable-or-too-large"))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ConversionError("hls-invalid-utf8"))?;
    if !text
        .lines()
        .any(|l| l.starts_with("#EXT-X-STREAM-INF:") || l.starts_with("#EXT-X-MEDIA:"))
    {
        return Ok(vec![MediaPath {
            path: path.into(),
            role: None,
            variants: 0,
            metadata: metadata::Metadata::default(),
            audio_variant_alias: false,
        }]);
    }
    let root = path
        .parent()
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|_| ConversionError("input-unreadable"))?;
    let mut paths = Vec::new();
    let mut pending = false;
    let mut pending_metadata = None;
    let mut pending_role = None;
    let mut audio_count = 0;
    let mut audio_group = None;
    let mut variant_groups = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attrs = hls::attributes(value)?;
            audio_count += 1;
            if audio_count > 1 {
                return Err(ConversionError("hls-rendition-metadata-not-supported"));
            }
            let rendition = metadata::Rendition::hls(&attrs)?;
            audio_group = Some(
                attrs
                    .get("GROUP-ID")
                    .filter(|group| !group.is_empty() && group.len() <= 256)
                    .ok_or(ConversionError("hls-audio-association-not-supported"))?
                    .to_string(),
            );
            let uri = attrs
                .get("URI")
                .ok_or(ConversionError("hls-media-uri-required"))?;
            paths.push(MediaPath {
                path: hls::local_resource(&root, uri)?,
                role: Some(TrackKind::Audio),
                variants: 0,
                metadata: metadata::Metadata {
                    origin: metadata::Origin::Hls,
                    audio: Some(rendition),
                    ..Default::default()
                },
                audio_variant_alias: false,
            });
        } else if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = hls::attributes(value)?;
            validate_variant(&attrs)?;
            if pending {
                return Err(ConversionError("hls-variant-uri-required"));
            }
            if variant_groups.len() == 64 {
                return Err(ConversionError("track-count-limit"));
            }
            pending_metadata = Some(metadata::variant(&attrs)?);
            pending_role = variant_role(&attrs);
            variant_groups.push(attrs.get("AUDIO").map(str::to_owned));
            pending = true;
        } else if !line.starts_with('#') {
            if !pending {
                return Err(ConversionError("hls-unexpected-master-resource"));
            }
            paths.push(MediaPath {
                path: hls::local_resource(&root, line)?,
                // Resolve variant roles only after every rendition declaration is known.
                role: pending_role.take(),
                variants: 0,
                metadata: pending_metadata
                    .take()
                    .ok_or(ConversionError("hls-variant-uri-required"))?,
                audio_variant_alias: false,
            });
            pending = false;
        } else if line.starts_with("#EXT-X-")
            && !line.starts_with("#EXT-X-VERSION:")
            && line != "#EXT-X-INDEPENDENT-SEGMENTS"
        {
            return Err(ConversionError("hls-master-feature-not-supported"));
        }
        if paths.len() > 64 {
            return Err(ConversionError("track-count-limit"));
        }
    }
    if pending || paths.is_empty() || variant_groups.is_empty() {
        return Err(ConversionError("hls-variant-uri-required"));
    }
    if variant_groups.iter().any(|group| *group != audio_group) {
        return Err(ConversionError("hls-audio-association-not-supported"));
    }
    reconcile_media_paths(paths, audio_group.is_some(), variant_groups.len())
}
fn reconcile_media_paths(
    paths: Vec<MediaPath>,
    separate_audio: bool,
    variants: usize,
) -> Result<Vec<MediaPath>, ConversionError> {
    let audio = paths
        .iter()
        .filter(|media| media.metadata.audio.is_some())
        .map(|media| (media.path.clone(), media.metadata.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let aliases = paths
        .iter()
        .filter(|media| media.metadata.audio.is_none() && audio.contains_key(&media.path))
        .map(|media| media.path.clone())
        .collect::<std::collections::BTreeSet<_>>();
    let mut seen = std::collections::BTreeMap::new();
    let mut unique = Vec::new();
    for mut media in paths {
        media.audio_variant_alias = aliases.contains(&media.path);
        if media.metadata.audio.is_none()
            && let Some(rendition) = audio.get(&media.path)
        {
            if media.role == Some(TrackKind::Video)
                || media.metadata.resolution.is_some()
                || media.metadata.audio_group.as_deref()
                    != rendition.audio.as_ref().map(|audio| audio.group.as_str())
            {
                return Err(ConversionError("hls-audio-association-not-supported"));
            }
            // One audio playlist can be both a rendition and an audio-only variant.
            // Its native stream role is still checked after opening the media.
            media.role = Some(TrackKind::Audio);
            media.metadata = rendition.clone();
        } else if separate_audio && media.role.is_none() {
            media.role = Some(TrackKind::Video);
        } else if !separate_audio && media.role == Some(TrackKind::Video) {
            // Without a separate audio group this variant may contain both streams.
            media.role = None;
        }
        // Canonical aliases must agree before any duplicate resource can be discarded.
        match seen.get(&media.path) {
            Some((role, metadata)) if *role != media.role || *metadata != media.metadata => {
                return Err(ConversionError("hls-audio-association-not-supported"));
            }
            Some(_) => continue,
            None => {
                seen.insert(media.path.clone(), (media.role, media.metadata.clone()));
            }
        }
        media.variants = variants;
        unique.push(media);
    }
    Ok(unique)
}

/// Local conversion inputs with independent input and output keys. No URI retrieves a key.
#[derive(Clone, Copy)]
pub struct ConversionRequest<'a> {
    pub input: &'a Path,
    pub output: &'a Path,
    pub format: OutputFormat,
    pub target: TargetProtection,
    pub input_keys: &'a InputKeys,
    pub hls_input_keys: &'a super::HlsInputKeys,
    pub output_key: Option<&'a OutputKey>,
    pub hls_key_uri: Option<&'a str>,
    pub hls_output_key: Option<&'a HlsOutputKey>,
}
fn validate_prepared_output(
    request: &ConversionRequest<'_>,
    prepared: &[Prepared],
    transport: bool,
) -> Result<(), ConversionError> {
    if transport
        && request.format == OutputFormat::Mp4
        && !prepared
            .iter()
            .any(|p| p.report.tracks[0].kind == TrackKind::Video)
    {
        return Err(ConversionError("ts-audio-only-timing-proof-not-supported"));
    }
    if transport
        && !matches!(
            (request.format, request.target),
            (
                OutputFormat::Hls,
                TargetProtection::Clear | TargetProtection::Cbcs | TargetProtection::HlsAes128
            ) | (
                OutputFormat::Dash,
                TargetProtection::Clear | TargetProtection::Cenc | TargetProtection::Cbcs
            )
        )
    {
        return Err(ConversionError("ts-output-profile-not-supported"));
    }
    if request.format == OutputFormat::Dash && !metadata::dash_representable(prepared) {
        return Err(ConversionError("presentation-selection-not-representable"));
    }
    if request.format == OutputFormat::Hls
        && prepared
            .iter()
            .filter(|p| p.report.tracks[0].kind == TrackKind::Audio)
            .count()
            > 1
    {
        return Err(ConversionError(
            "hls-multiple-audio-renditions-not-supported",
        ));
    }
    if request.format == OutputFormat::Mp4
        && prepared
            .iter()
            .any(|p| p.metadata.audio.is_some() || p.metadata.audio_group.is_some())
    {
        return Err(ConversionError("presentation-selection-not-representable"));
    }
    Ok(())
}
fn output_hls_keys(key: Option<&HlsOutputKey>) -> Result<super::HlsInputKeys, ConversionError> {
    key.map(|key| {
        super::HlsInputKeys::new_with_ivs(vec![(
            key.key_uri().to_owned(),
            super::Aes128Key(key.key.0.clone()),
            key.iv.as_ref().map(|iv| super::SegmentIv(iv.0.clone())),
        )])
    })
    .transpose()
    .map(Option::unwrap_or_default)
}
fn validate_output(request: &ConversionRequest<'_>) -> Result<(), ConversionError> {
    if request.format == OutputFormat::HlsTs {
        if !matches!(
            request.target,
            TargetProtection::Clear | TargetProtection::HlsAes128 | TargetProtection::HlsSampleAes
        ) {
            return Err(ConversionError("ts-output-profile-not-supported"));
        }
        if request.output_key.is_some()
            || request.hls_key_uri.is_some()
            || (request.target != TargetProtection::Clear) != request.hls_output_key.is_some()
        {
            return Err(ConversionError("output-key-role-mismatch"));
        }
        if request.target == TargetProtection::HlsSampleAes
            && request
                .hls_output_key
                .and_then(HlsOutputKey::associated_iv)
                .is_none()
        {
            return Err(ConversionError("hls-sample-output-iv-required"));
        }
    } else if request.target == TargetProtection::HlsSampleAes {
        return Err(ConversionError("target-requires-ts-output"));
    }
    if request.target != TargetProtection::HlsSampleAes
        && request
            .hls_output_key
            .and_then(HlsOutputKey::associated_iv)
            .is_some()
    {
        return Err(ConversionError("output-iv-not-used"));
    }
    Ok(())
}
impl Converter {
    fn inspect_input(&self, input: &Path) -> Result<Inspection, ConversionError> {
        let _scope = self.backend.scope(super::measurement::Phase::Inspection);
        inspection::inspect(input).map_err(|_| ConversionError("input-inspection-failed"))
    }
    fn verify_source_tracks(
        &self,
        prepared: &[Prepared],
        tracks: &mut Vec<LocalTrack>,
        input_keys: &InputKeys,
    ) -> Result<(), ConversionError> {
        let progress = super::media_progress::Batch::units(
            self.backend.activity.clone(),
            super::media_progress::Operation::SourceComparison,
            super::media_progress::Subject::Source,
            super::media_progress::Purpose::Conversion,
            super::media_progress::Unit::Tracks,
            prepared.len(),
        );
        if let Some(progress) = &progress {
            for (ordinal, source) in prepared.iter().enumerate() {
                if let Some(track) = source.report.tracks.first() {
                    progress.describe(ordinal, track.kind, source.metadata.resolution);
                }
            }
        }
        let verified = self.run_tracks(prepared.len(), |ordinal| {
            let task = progress
                .as_ref()
                .and_then(|progress| progress.start(ordinal));
            let source = &prepared[ordinal];
            if source.source != source.media {
                let verified = if source.audio_presentation.is_some() {
                    self.verify_audio_projection(source, input_keys)?
                } else if let Some(index) = source.transport_index {
                    self.backend.verify_transport(
                        super::ts::View {
                            input: &source.source,
                            metadata: &source.source,
                            index,
                            key: None,
                            all_keys: None,
                            transport: true,
                            native: source.transport_native.as_deref(),
                            unedited: false,
                        }
                        .with_all_keys(input_keys),
                        super::ts::View {
                            input: &source.media,
                            metadata: &source.media,
                            index: 0,
                            key: None,
                            all_keys: None,
                            transport: false,
                            native: None,
                            unedited: false,
                        }
                        .with_all_keys(input_keys),
                    )?
                } else {
                    self.backend.verify_views(
                        StreamView::new(&source.source, source.index, key_for(source, input_keys))
                            .with_metadata(&source.media)
                            .with_clock(source.clock),
                        StreamView::new(&source.media, source.index, key_for(source, input_keys))
                            .with_clock(source.clock),
                    )?
                };
                if !verified.passed {
                    return Err(ConversionError("source-decryption-verification-failed"));
                }
            }
            self.backend.check_cancelled()?;
            if let Some(task) = task {
                task.finish();
            }
            Ok(LocalTrack {
                source: source.media.clone(),
                kind: source.report.tracks[0].kind,
                index: source.index,
            })
        })?;
        if let Some(progress) = &progress {
            progress.finish();
        }
        tracks.extend(verified);
        Ok(())
    }

    fn verify_audio_projection(
        &self,
        source: &Prepared,
        keys: &InputKeys,
    ) -> Result<VerificationReport, ConversionError> {
        let _scope = self.backend.scope(super::measurement::Phase::Verification);
        let original = raw_audio_view(source, keys);
        let raw = super::ts::View {
            input: &source.media,
            metadata: &source.media,
            index: source.index,
            key: key_for(source, keys),
            all_keys: Some(keys),
            transport: false,
            native: None,
            unedited: true,
        };
        let native = if source.transport_index.is_some() {
            self.backend.native_verify_transport_with_clock(
                original,
                raw,
                source.clock,
                source.clock,
            )?
        } else {
            let original = StreamView::new(&source.source, source.index, key_for(source, keys))
                .with_all_keys(keys)
                .with_clock(source.clock);
            let raw = StreamView::new(&source.media, source.index, key_for(source, keys))
                .with_all_keys(keys)
                .with_clock(source.clock);
            self.backend.native_verify_audio_projection(
                original,
                raw,
                source
                    .audio_presentation
                    .ok_or(super::audio_presentation::MISMATCH)?,
            )?
        };
        if self.backend.validation_depth == super::ValidationDepth::Native {
            return Ok(native);
        }
        let mut decoded =
            self.backend
                .verify_transport_with_clock(original, raw, source.clock, source.clock)?;
        decoded.require_native(&native)?;
        Ok(decoded)
    }
    fn verify_transport_output(
        &self,
        request: &ConversionRequest<'_>,
        prepared: Vec<Prepared>,
        candidate: &Path,
        work: &Path,
    ) -> Result<VerificationReport, ConversionError> {
        let output_keys = request
            .output_key
            .map(|k| InputKeys::new(vec![ContentKey::from_hex(&k.0.id_hex(), &k.0.key_hex())?]))
            .transpose()?
            .unwrap_or_default();
        let hls_keys = output_hls_keys(request.hls_output_key)?;
        let verification_root = work.join("final-verification");
        super::files::create_dir(&verification_root)?;
        let (backend, preparation) = super::media_progress::scoped(
            &self.backend,
            super::media_progress::Subject::Result,
            super::media_progress::Purpose::Conversion,
        );
        let result = prepare_presentation(
            &VerificationSource {
                input: &candidate.join(if request.format == OutputFormat::Dash {
                    "manifest.mpd"
                } else {
                    "master.m3u8"
                }),
                input_keys: &output_keys,
                hls_keys: &hls_keys,
            },
            &verification_root,
            &backend,
            self.maximum_workers,
            SourceValidationPurpose::Verification,
        )?;
        preparation.finish();
        self.backend.check_cancelled()?;
        let verification =
            self.compare_presentations(prepared, result, request.input_keys, &output_keys)?;
        if !verification.passed {
            return Err(ConversionError("round-trip-verification-failed"));
        }
        Ok(verification)
    }
    pub fn convert(
        &self,
        request: ConversionRequest<'_>,
    ) -> Result<ConversionReport, ConversionError> {
        if self
            .output_signaling
            .as_ref()
            .is_some_and(|value| value.is_multi())
        {
            self.backend.check_cancelled()?;
            let frozen = self.capture_source(request.input)?;
            return self
                .using_frozen_source(&frozen)
                .convert_inner(ConversionRequest {
                    input: frozen.entrypoint(),
                    ..request
                });
        }
        self.convert_inner(request)
    }

    pub(in crate::conversion) fn convert_bundle_admitted(
        &self,
        request: ConversionRequest<'_>,
        admitted: &BundleAdmission<'_>,
        shared: Option<&BundlePrepared<'_>>,
    ) -> Result<ConversionReport, ConversionError> {
        if self.processing_mode != admitted.processing {
            return self.convert(request);
        }
        if request.input != admitted.source.input
            || !std::ptr::eq(request.input_keys, admitted.source.input_keys)
            || !std::ptr::eq(request.hls_input_keys, admitted.source.hls_keys)
            || self.backend.validation_depth != admitted.depth
        {
            return Err(ConversionError("source-admission-mismatch"));
        }
        self.backend.check_cancelled()?;
        if self
            .output_signaling
            .as_ref()
            .is_some_and(|value| value.is_multi())
        {
            let frozen = self.capture_source(request.input)?;
            if frozen.inventory().files != admitted.inventory.files
                || frozen.inventory().entrypoint != admitted.inventory.entrypoint
            {
                return Err(ConversionError("planned-source-changed"));
            }
            let command = self.using_frozen_source(&frozen);
            let request = ConversionRequest {
                input: frozen.entrypoint(),
                ..request
            };
            if let Some(shared) = shared {
                command.convert_prepared_bundle(request, admitted, shared)
            } else {
                command.convert_inner_admitted(request, Some(admitted.qualified))
            }
        } else if let Some(shared) = shared {
            self.convert_prepared_bundle(request, admitted, shared)
        } else {
            self.convert_inner_admitted(request, Some(admitted.qualified))
        }
    }
    fn using_prepared_target(
        &self,
        prepared: &[Prepared],
        source: InputFormat,
        output: OutputFormat,
    ) -> Result<Self, ConversionError> {
        let timing = if output == OutputFormat::Mp4 || !self.inherits_hls_target() {
            None
        } else {
            prepared_hls_timing(prepared, source, &self.backend.stop)?
        };
        Ok(self.using_hls_timing(timing))
    }

    fn convert_inner(
        &self,
        request: ConversionRequest<'_>,
    ) -> Result<ConversionReport, ConversionError> {
        self.convert_inner_admitted(request, None)
    }

    fn convert_inner_admitted(
        &self,
        request: ConversionRequest<'_>,
        admitted: Option<&SourceQualification>,
    ) -> Result<ConversionReport, ConversionError> {
        if self.processing_mode == super::ProcessingMode::Segments {
            validate_output(&request)?;
            return super::segments::convert(self, request);
        }
        self.validate_signaling_input(&request, admitted)?;
        validate_output(&request)?;
        let work = workspace()?;
        let input = VerificationSource {
            input: request.input,
            input_keys: request.input_keys,
            hls_keys: request.hls_input_keys,
        };
        let input_report = self.inspect_input(request.input)?;
        let preparation = self
            .backend
            .scope(super::measurement::Phase::InputPreparation);
        let prepared = prepare_presentation(
            &input,
            work.path(),
            &self.backend,
            self.maximum_workers,
            SourceValidationPurpose::Conversion,
        )?;
        drop(preparation);
        let command = self.using_prepared_output(&request, &prepared, input_report.format)?;
        let transport = prepared.iter().any(|p| p.transport_index.is_some());
        let sample_transport = prepared.iter().any(|p| p.transport_sample);
        if request.format == OutputFormat::HlsTs {
            return command.convert_transport_output(&request, prepared, input_report, &work);
        }
        validate_prepared_output(&request, &prepared, transport)?;
        self.validate_complete_clock_output(&request, &prepared)?;
        let presentations: Vec<_> = prepared.iter().map(|p| p.audio_presentation).collect();
        let clocks: Vec<_> = prepared.iter().map(TrackClock::from_prepared).collect();
        let descriptions: Vec<_> = prepared.iter().map(|p| p.metadata.clone()).collect();
        let mut tracks = Vec::new();
        self.verify_source_tracks(&prepared, &mut tracks, request.input_keys)?;
        let destination = transport.then(|| destination(request.output)).transpose()?;
        let stage = destination
            .as_ref()
            .map(|path| self.output_stage(path))
            .transpose()?;
        let candidate = stage.as_ref().map(|stage| stage.path().to_path_buf());
        let mut report = command.convert_tracks_inner(
            TrackConversionRequest {
                tracks: &tracks,
                output: candidate.as_deref().unwrap_or(request.output),
                format: request.format,
                target: request.target,
                input_keys: request.input_keys,
                output_key: request.output_key,
                hls_key_uri: request.hls_key_uri,
                hls_output_key: request.hls_output_key,
            },
            stage.as_ref(),
            Some(&descriptions),
            Some(&presentations),
            Some(&clocks),
        )?;
        if let (Some(candidate), Some(destination)) = (&candidate, &destination) {
            report.verification =
                self.verify_transport_output(&request, prepared, candidate, work.path())?;
            let stage = stage
                .as_ref()
                .ok_or(ConversionError("output-staging-failed"))?;
            self.backend.checkpoint(&[&work, stage]);
            let manifest = candidate.join(if request.format == OutputFormat::Dash {
                "manifest.mpd"
            } else {
                "master.m3u8"
            });
            report.verification.native_manifest = Some(self.publish_manifest_output(
                stage,
                destination,
                &manifest,
                request.format,
            )?);
        }
        self.backend.checkpoint(&[&work]);
        report.input = input_report;
        if transport {
            describe_transport(
                &self.backend,
                &mut report,
                sample_transport,
                request.format == OutputFormat::Dash,
            );
        } else {
            report.describe_preparation(match report.input.format {
                InputFormat::Mp4 => inspection::InputPreparation::CompleteFile,
                InputFormat::Dash => inspection::InputPreparation::LocalDashResources,
                InputFormat::Hls => inspection::InputPreparation::LocalHlsResources,
            });
        }
        Ok(report)
    }

    fn using_prepared_output(
        &self,
        request: &ConversionRequest<'_>,
        prepared: &[Prepared],
        source_format: InputFormat,
    ) -> Result<Self, ConversionError> {
        self.using_prepared_target(prepared, source_format, request.format)?
            .using_admitted_output_options(
                request.input,
                source_format,
                &qualified_layout_tracks(prepared, source_format),
                request.format,
                super::ProcessingMode::Stitch,
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_only_variant_aliases_retain_rendition_metadata_in_either_order() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("audio.m3u8"), "#EXTM3U\n").unwrap();
        let manifest = directory.path().join("master.m3u8");
        let rendition = "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Main\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"2\",URI=\"audio.m3u8\"\n";
        for codecs in ["", ",CODECS=\"mp4a.40.2\""] {
            let variant =
                format!("#EXT-X-STREAM-INF:BANDWIDTH=100,AUDIO=\"a\"{codecs}\n./audio.m3u8\n");
            for body in [
                format!("{rendition}{variant}"),
                format!("{variant}{rendition}"),
            ] {
                fs::write(&manifest, format!("#EXTM3U\n{body}")).unwrap();
                let paths = hls_media_paths(&manifest).unwrap();
                assert_eq!(paths.len(), 1);
                assert_eq!(paths[0].role, Some(TrackKind::Audio));
                assert_eq!(paths[0].variants, 1);
                assert_eq!(paths[0].metadata.audio_group, None);
                let audio = paths[0].metadata.audio.as_ref().unwrap();
                assert_eq!(audio.name.as_deref(), Some("Main"));
                assert_eq!(audio.language, "en");
                assert_eq!(audio.channels.as_deref(), Some("2"));
                assert_eq!(audio.is_default, Some(true));
            }
        }
    }

    #[test]
    fn audio_only_variant_aliases_cannot_hide_conflicting_video_declarations() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("audio.m3u8"), "#EXTM3U\n").unwrap();
        let manifest = directory.path().join("master.m3u8");
        let rendition = "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Main\",URI=\"audio.m3u8\"\n";
        for attributes in [
            "AUDIO=\"a\",CODECS=\"avc1.64001e,mp4a.40.2\"",
            "AUDIO=\"a\",RESOLUTION=64x64",
            "AUDIO=\"wrong\",CODECS=\"mp4a.40.2\"",
        ] {
            fs::write(
                &manifest,
                format!(
                    "#EXTM3U\n{rendition}#EXT-X-STREAM-INF:BANDWIDTH=100,{attributes}\naudio.m3u8\n"
                ),
            )
            .unwrap();
            assert_eq!(
                hls_media_paths(&manifest).unwrap_err().0,
                "hls-audio-association-not-supported"
            );
        }
    }

    #[test]
    fn audio_associations_cannot_be_added_to_unrelated_variants() {
        let directory = tempfile::tempdir().unwrap();
        for name in ["audio.m3u8", "first.m3u8", "second.m3u8"] {
            fs::write(directory.path().join(name), "#EXTM3U\n").unwrap();
        }
        let manifest = directory.path().join("master.m3u8");
        let audio = "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"stream_0\",DEFAULT=NO,AUTOSELECT=YES,URI=\"audio.m3u8\"\n";
        let first = "#EXT-X-STREAM-INF:BANDWIDTH=100,AUDIO=\"a\"\nfirst.m3u8\n";
        let valid = format!(
            "#EXTM3U\n{audio}{first}#EXT-X-STREAM-INF:BANDWIDTH=200,AUDIO=\"a\"\nsecond.m3u8\n"
        );
        fs::write(&manifest, &valid).unwrap();
        assert_eq!(hls_media_paths(&manifest).unwrap().len(), 3);
        for invalid in [
            valid.replace("BANDWIDTH=200,AUDIO=\"a\"", "BANDWIDTH=200"),
            valid.replace(
                "BANDWIDTH=200,AUDIO=\"a\"",
                "BANDWIDTH=200,AUDIO=\"missing\"",
            ),
            format!("#EXTM3U\n{first}"),
        ] {
            fs::write(&manifest, invalid).unwrap();
            assert_eq!(
                hls_media_paths(&manifest).unwrap_err().0,
                "hls-audio-association-not-supported"
            );
        }
    }
}
