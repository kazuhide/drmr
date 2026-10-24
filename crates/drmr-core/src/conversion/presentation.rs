//! Joint, codec-preserving presentations with independent evidence for every source stream.
pub(in crate::conversion) mod metadata;
mod native_preparation;
#[cfg(test)]
mod signaling_tests;
#[cfg(test)]
mod staging_tests;
mod ts_output;
use super::{
    ContentKey, ConversionError, ConversionReport, Converter, HlsOutputKey, InputKeys, LocalTrack,
    OutputKey, VerificationReport, audio, copy_private, destination, hls, verify::StreamView,
    workspace,
};
use crate::inspection::{
    self, Evidence, InputFormat, Inspection, OutputFormat, Protection, TargetProtection, TrackKind,
};
use std::{
    fs,
    io::Write,
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
}
fn presentation_processing(
    backend: &super::backend::Backend,
    format: OutputFormat,
    ffmpeg_copy: bool,
) -> inspection::ActualProcessing {
    backend.processing(
        if format == OutputFormat::Dash {
            inspection::ProcessingRouteId::DashPresentation
        } else {
            inspection::ProcessingRouteId::Fmp4HlsPresentation
        },
        inspection::InputPreparation::LocalTrackFiles,
        ffmpeg_copy,
    )
}
impl Converter {
    pub fn convert_tracks(
        &self,
        request: TrackConversionRequest<'_>,
    ) -> Result<ConversionReport, ConversionError> {
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
            let frozen = super::FrozenSource::capture(&first.source, &self.backend.stop)?;
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
            );
        }
        self.convert_tracks_inner(request, None, None, None)
    }
    fn convert_tracks_inner(
        &self,
        request: TrackConversionRequest<'_>,
        transport_stage: Option<&super::files::Stage>,
        metadata: Option<&[metadata::Metadata]>,
        presentations: Option<&[Option<super::audio_presentation::AudioPresentation>]>,
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
        let (mut sources, clears) = self.normalize_tracks(&request, work.path())?;
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
        self.package_joint(&clears, package_root, &request)?;
        self.finish_packaged_presentation(package_root, stage.path(), &sources, &request)?;
        let (mut verification, output_tracks, input_tracks) =
            self.verify_joint(&sources, &request, stage.path(), work.path())?;
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
        self.backend.checkpoint(&[&work, stage]);
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
            processing: Some(presentation_processing(
                &self.backend,
                request.format,
                sources
                    .iter()
                    .any(|source| source.source_dictionary && source.key_id.is_some()),
            )),
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
        )
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
        let mut output_tracks = Vec::new();
        let mut input_tracks = Vec::new();
        let verified_tracks = self.run_tracks(sources.len(), |ordinal| {
            let source = &sources[ordinal];
            let track_dir = stage.join(format!("track-{ordinal:04}"));
            let combined = work.join(format!("result-{ordinal}.mp4"));
            let actual = if let Some(key) = request.hls_output_key {
                let keys = super::HlsInputKeys::new(vec![(
                    key.key_uri().to_string(),
                    super::Aes128Key(key.key.0.clone()),
                )])?;
                let localized_dir = work.join(format!("localized-{ordinal}"));
                super::files::create_dir(&localized_dir)
                    .map_err(|_| ConversionError("output-staging-failed"))?;
                let localized = hls::localize(
                    &track_dir.join("media.m3u8"),
                    &localized_dir,
                    &keys,
                    &self.backend,
                )?;
                copy_private(&localized.combined, &combined)?;
                inspection::inspect(&combined)
                    .map_err(|_| ConversionError("invalid-output-media"))?
            } else {
                combine_generated(&track_dir, &combined)?;
                inspection::inspect(&combined)
                    .map_err(|_| ConversionError("invalid-output-media"))?
            };
            let expected = match request.target {
                TargetProtection::Clear | TargetProtection::HlsAes128 => Protection::Clear,
                TargetProtection::Cenc => Protection::Cenc,
                TargetProtection::Cbcs => Protection::Cbcs,
                TargetProtection::HlsSampleAes => {
                    return Err(ConversionError("target-requires-ts-output"));
                }
            };
            if actual.protection != expected
                || actual.tracks.len() != 1
                || actual.tracks[0].kind != source.report.tracks[0].kind
            {
                return Err(ConversionError("output-protection-mismatch"));
            }
            if let Some(key) = request.output_key
                && actual.tracks[0].key_ids != [key.0.id_hex()]
            {
                return Err(ConversionError("output-key-id-mismatch"));
            }
            let mut output = actual.tracks[0].clone();
            output.id = format!("track-{ordinal:04}");
            if request.target == TargetProtection::HlsAes128 {
                output.protection = Protection::HlsAes128;
            }
            let mut input = source.report.tracks[0].clone();
            input.id = format!("track-{ordinal:04}");
            Ok((input, output))
        })?;
        for (input, output) in verified_tracks {
            input_tracks.push(input);
            output_tracks.push(output);
        }
        let verification = self.verify_generated_presentation(sources, request, stage, work)?;
        if !verification.passed {
            return Err(ConversionError("round-trip-verification-failed"));
        }
        self.verify_signaled_output(&sources[0].source, request, stage)?;
        Ok((verification, output_tracks, input_tracks))
    }
    fn verify_generated_presentation(
        &self,
        sources: &[Prepared],
        request: &TrackConversionRequest<'_>,
        stage: &Path,
        work: &Path,
    ) -> Result<VerificationReport, ConversionError> {
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
        let actual = prepare_presentation(
            &VerificationSource {
                input: &manifest,
                input_keys: &keys,
                hls_keys: &hls_keys,
            },
            &directory,
            &self.backend,
            self.maximum_workers,
            SourceValidationPurpose::Verification,
        )?;
        self.compare_presentations(sources.to_vec(), actual, request.input_keys, &keys)
    }
    fn normalize_tracks(
        &self,
        request: &TrackConversionRequest<'_>,
        work: &Path,
    ) -> Result<(Vec<Prepared>, Vec<PathBuf>), ConversionError> {
        // Inspect all private snapshots before selecting a content-aware worker count.
        let mut sources = Vec::new();
        for (ordinal, track) in request.tracks.iter().enumerate() {
            // A completed tool seals only its own track directory, while sibling tools
            // may still have writable output handles open on Windows.
            let directory = work.join(format!("normalize-{ordinal:04}"));
            super::files::create_dir(&directory)?;
            let source = directory.join("source.mp4");
            copy_private(&track.source, &source)?;
            if self
                .output_signaling
                .as_ref()
                .is_some_and(|value| !value.is_multi())
            {
                super::signaling::clear_mp4(&source, &self.backend.stop)?;
            }
            let prepared = prepare(&source, track.index, track.kind, request.input_keys)?;
            if super::finite::has_audio(&source)? && !audio::movie(&source)?.fragmented {
                let report = inspection::inspect(&source)
                    .map_err(|_| ConversionError("invalid-input-media"))?;
                if !super::progressive::validate_input(&source, &report, request.input_keys)? {
                    return Err(ConversionError("progressive-mp4-profile-not-supported"));
                }
                self.progressive_codecs(
                    &source,
                    &report,
                    (report.protection != Protection::Clear)
                        .then_some(super::keys::ProbeKeys::All(request.input_keys)),
                )?;
            }

            let profile = self.backend.profile_stream_keys(
                &source,
                track.index,
                if prepared.source_dictionary {
                    Some(super::keys::ProbeKeys::All(request.input_keys))
                } else {
                    key_for(&prepared, request.input_keys).map(super::keys::ProbeKeys::One)
                },
            )?;
            self.validate_signaling_codec(&profile, track.kind)?;
            if profile["codec_type"].as_str()
                != Some(if track.kind == TrackKind::Audio {
                    "audio"
                } else {
                    "video"
                })
            {
                return Err(ConversionError("track-role-mismatch"));
            }
            sources.push(prepared);
        }
        let clears = self.run_tracks(sources.len(), |ordinal| {
            let source = &sources[ordinal];
            let clear = source
                .media
                .parent()
                .ok_or(ConversionError("temporary-workspace-unavailable"))?
                .join("clear.mp4");
            let normalization_scope = self.backend.scope(super::measurement::Phase::Normalization);
            let whole_clear = source
                .media
                .parent()
                .ok_or(ConversionError("temporary-workspace-unavailable"))?
                .join("whole-clear.mp4");
            let packaging_source = if source.source_dictionary {
                let report = inspection::inspect(&source.source)
                    .map_err(|_| ConversionError("invalid-input-media"))?;
                self.clear_finite(&source.source, &whole_clear, &report, request.input_keys)?;
                &whole_clear
            } else {
                &source.media
            };

            self.backend.package_stream(
                packaging_source,
                source.index,
                &clear,
                (source.key_id.is_some() && !source.source_dictionary)
                    .then_some(request.input_keys),
                None,
            )?;
            drop(normalization_scope);
            let a = StreamView::new(
                &source.source,
                source.index,
                key_for(source, request.input_keys),
            );
            let normalization = self.backend.verify_views(
                a.with_all_keys(request.input_keys),
                StreamView::new(&clear, 0, None),
            )?;
            if !normalization.passed {
                return Err(ConversionError("source-normalization-verification-failed"));
            }
            Ok(clear)
        })?;
        Ok((sources, clears))
    }
    fn package_joint(
        &self,
        clear: &[PathBuf],
        root: &Path,
        request: &TrackConversionRequest<'_>,
    ) -> Result<(), ConversionError> {
        let _scope = self.backend.scope(super::measurement::Phase::Packaging);
        let mut argv = joint_arguments(clear, root, request, &self.backend.packager)?;
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
        let mut secrets: Vec<&str> = secret
            .iter()
            .map(|s| s.as_str())
            .chain(request.hls_key_uri)
            .collect();
        if let Some(signaling) = &signaling {
            secrets.extend(signaling.secrets());
        }
        if let Some(signaling) = &multi_signaling {
            secrets.extend(signaling.secrets());
        }
        let pssh = signaling
            .as_ref()
            .map(|value| value.hex.as_str())
            .or_else(|| multi_signaling.as_ref().map(|value| value.hex.as_str()));
        if let Some(pssh) = pssh {
            super::signaling::flagfile::run(&self.backend, &argv, pssh, &secrets)?;
        } else {
            self.backend.run(&argv, "packager", &secrets)?;
        }
        for (index, source) in clear.iter().enumerate() {
            super::finite::restore(
                source,
                0,
                &root.join(format!("track-{index:04}/init.mp4")),
                &self.backend.stop,
            )?;
        }
        super::finite::manifest::finish(root, clear, request.format, &self.backend.stop)?;
        if request.format == OutputFormat::Hls
            && request.tracks.len() == 1
            && request.tracks[0].kind == TrackKind::Audio
        {
            ts_output::single_audio_variant(root)?;
        }
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
}
fn joint_arguments(
    clear: &[PathBuf],
    root: &Path,
    request: &TrackConversionRequest<'_>,
    packager: &str,
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
        "2".into(),
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
fn combine_generated(directory: &Path, output: &Path) -> Result<(), ConversionError> {
    let mut segments = Vec::new();
    for entry in
        fs::read_dir(directory).map_err(|_| ConversionError("packaged-segments-missing"))?
    {
        let entry = entry.map_err(|_| ConversionError("packaged-segments-missing"))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Err(ConversionError("invalid-packaged-resource"));
        };
        if let Some(number) = name
            .strip_prefix("seg_")
            .and_then(|n| n.strip_suffix(".m4s"))
        {
            let number = number
                .parse::<u64>()
                .map_err(|_| ConversionError("invalid-packaged-resource"))?;
            segments.push((number, entry.path()));
        }
    }
    segments.sort_by_key(|(n, _)| *n);
    if segments.is_empty() || segments.len() > 100_000 {
        return Err(ConversionError("packaged-segment-count-limit"));
    }
    let mut file = super::files::create_new(output)?;
    let mut total = 0u64;
    for path in
        std::iter::once(directory.join("init.mp4")).chain(segments.into_iter().map(|(_, p)| p))
    {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|_| ConversionError("packaged-resource-unreadable"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(ConversionError("invalid-packaged-resource"));
        }
        total = total
            .checked_add(metadata.len())
            .filter(|n| *n <= super::MAX_MEDIA_BYTES)
            .ok_or(ConversionError("media-size-limit"))?;
        let mut input =
            fs::File::open(path).map_err(|_| ConversionError("packaged-resource-unreadable"))?;
        std::io::copy(&mut input, &mut file)
            .map_err(|_| ConversionError("output-staging-failed"))?;
    }
    file.flush()
        .map_err(|_| ConversionError("output-staging-failed"))?;
    Ok(())
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
    pub input: Inspection,
    pub actual_format: OutputFormat,
    pub track_protection: Vec<SourceProtection>,
    pub compatibility_notes: Vec<SourceCompatibilityNote>,
    pub eligible_formats: Vec<OutputFormat>,
    pub verification: VerificationReport,
}
impl Converter {
    /// Open and validate every local resource and execute complete media evidence checks.
    /// Conversion admission applies conversion's source policy; verification also permits
    /// supported existing DRM signaling. Output packaging and keys are revalidated later.
    pub fn validate_source(
        &self,
        input: VerificationSource<'_>,
        purpose: SourceValidationPurpose,
    ) -> Result<SourceQualification, ConversionError> {
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
        if actual_format == OutputFormat::HlsTs {
            eligible_formats.push(OutputFormat::HlsTs);
        }
        let verification = self.validate_prepared_sources(&prepared, input.input_keys)?;
        if !verification.passed {
            return Err(ConversionError("source-media-verification-failed"));
        }
        self.backend.checkpoint(&[&work]);
        Ok(SourceQualification {
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
                return self.backend.native_validate_mp4(
                    &track.media,
                    track.index,
                    Some(super::keys::ProbeKeys::All(input_keys)),
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
}
impl Converter {
    pub fn verify(
        &self,
        request: VerificationRequest<'_>,
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
        let source = prepare_presentation(
            &request.source,
            &source_dir,
            &self.backend,
            self.maximum_workers,
            SourceValidationPurpose::Verification,
        )?;
        let checked = self.checked_output_manifest(request.output.input)?;
        let result = prepare_presentation(
            &request.output,
            &result_dir,
            &self.backend,
            self.maximum_workers,
            SourceValidationPurpose::Verification,
        )?;
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
            return self.verify_transport_pair(a, b);
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
        let a_view =
            StreamView::new(&a.source, a.index, key_for(a, source_keys)).with_metadata(&a.media);
        let b_view =
            StreamView::new(&b.source, b.index, key_for(b, result_keys)).with_metadata(&b.media);
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
        let verified_tracks = self.run_tracks(source.len(), |ordinal| {
            self.verify_prepared_pair(&source[ordinal], &result[ordinal], source_keys, result_keys)
        })?;
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
        false,
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
    if super::audio_presentation::read_claim(&track.media)?.is_none() {
        return Ok(());
    }
    let evidence = backend.audio_evidence(
        super::ts::View {
            input: &track.media,
            metadata: &track.media,
            index: track.index,
            key: key_for(track, keys),
            all_keys: None,
            transport: false,
            native: None,
            unedited: true,
        }
        .with_all_keys(keys),
    )?;
    let range = super::audio_presentation::read(&track.media, evidence)?
        .ok_or(super::audio_presentation::MISMATCH)?;
    let view = directory.join(format!("raw-audio-{}.mp4", track.index));
    copy_private(&track.media, &view)?;
    super::audio_presentation::strip(&view, range, &backend.stop)?;
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
            actual
                .tracks
                .iter()
                .enumerate()
                .map(|(index, track)| {
                    prepare_for(&path, index, track.kind, input.input_keys, purpose)
                })
                .collect()
        }
        InputFormat::Hls => {
            prepare_hls_presentation(input, stage, backend, maximum_workers, purpose, normalize)
        }
        InputFormat::Dash => {
            let tracks = super::dash::snapshot_dash(input.input, stage)?;
            let descriptions = metadata::read_dash(&stage.join("dash-manifest.mpd"))?;
            tracks
                .iter()
                .zip(descriptions)
                .map(|(track, metadata)| {
                    let mut prepared = prepare_for(
                        &track.source,
                        track.index,
                        track.kind,
                        input.input_keys,
                        purpose,
                    )?;
                    bind_audio_presentation(&mut prepared, backend, input.input_keys, stage)?;
                    prepared.metadata = metadata;
                    if prepared.metadata.audio.is_some() && track.kind != TrackKind::Audio {
                        return Err(ConversionError("track-role-mismatch"));
                    }
                    prepared.dash_reference = true;
                    Ok(prepared)
                })
                .collect()
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
    let paths = hls_media_paths(input.input)?;
    let mut tracks = Vec::new();
    for (ordinal, media) in paths.iter().enumerate() {
        let directory = stage.join(format!("track-{ordinal:04}"));
        super::files::create_dir(&directory)
            .map_err(|_| ConversionError("temporary-workspace-unavailable"))?;
        let localized = hls::localize(&media.path, &directory, input.hls_keys, backend)?;
        if let Some(admission) = &localized.transport {
            if (paths.len() != 1 && admission.tracks.len() != 1)
                || media.role.is_some_and(|kind| {
                    admission.tracks.len() != 1 || admission.tracks[0].kind != kind
                })
            {
                return Err(ConversionError("ts-presentation-layout-not-supported"));
            }
            let mut prepared =
                if backend.validation_depth == super::ValidationDepth::Native && !normalize {
                    native_preparation::transport(&localized, admission, backend)?
                } else {
                    prepare_transport(
                        &localized,
                        admission,
                        &directory,
                        input.input_keys,
                        backend,
                        maximum_workers,
                    )?
                };
            apply_transport_metadata(&mut prepared, media, &localized, backend, input.input_keys)?;
            tracks.extend(prepared);
            continue;
        }
        let actual = inspection::inspect(&localized.combined)
            .map_err(|_| ConversionError("invalid-localized-hls-media"))?;
        if actual.tracks.len() != 1 {
            return Err(ConversionError("hls-multiplexed-media-not-supported"));
        }
        if media.role.is_some_and(|kind| kind != actual.tracks[0].kind) {
            return Err(ConversionError("track-role-mismatch"));
        }
        let mut track = prepare_for(
            &localized.combined,
            0,
            actual.tracks[0].kind,
            input.input_keys,
            purpose,
        )?;
        bind_audio_presentation(&mut track, backend, input.input_keys, &directory)?;
        track.metadata = media.metadata.clone();
        if localized.empty_cue_pair {
            track
                .compatibility
                .push(SourceCompatibilityNote::HlsEmptyCuePair);
        }
        match localized.report.protection {
            Protection::HlsAes128 => track.outer_protection = SourceProtection::HlsAes128,
            Protection::Mixed => track.outer_protection = SourceProtection::Mixed,
            _ => {}
        }
        metadata::validate_profile(&track, backend, key_for(&track, input.input_keys))?;
        track.playlist_reference = if track.key_id.is_none() {
            Some(localized.playlist.clone())
        } else if actual.protection == Protection::Cbcs {
            Some(hls::sample_reference(
                &localized.playlist,
                key_for(&track, input.input_keys).ok_or(ConversionError("input-key-required"))?,
            )?)
        } else {
            None
        };
        track.source = if track.audio_presentation.is_some() {
            localized.combined
        } else {
            localized.reference
        };
        tracks.push(track);
    }
    if tracks.len() > 64
        || tracks
            .iter()
            .filter(|p| p.report.tracks[0].kind == TrackKind::Audio)
            .count()
            > 1
        || (tracks.iter().any(|track| track.transport_index.is_some())
            && tracks.iter().any(|track| track.transport_index.is_none()))
    {
        return Err(ConversionError("ts-presentation-layout-not-supported"));
    }
    Ok(tracks)
}
fn apply_transport_metadata(
    prepared: &mut [Prepared],
    media: &MediaPath,
    localized: &hls::Localized,
    backend: &super::backend::Backend,
    keys: &InputKeys,
) -> Result<(), ConversionError> {
    for track in prepared {
        track.metadata = media.metadata.clone();
        if localized.empty_cue_pair {
            track
                .compatibility
                .push(SourceCompatibilityNote::HlsEmptyCuePair);
        }
        track.outer_protection = match (localized.sample_transport, localized.report.protection) {
            (true, _) => SourceProtection::HlsSampleAes,
            (_, Protection::HlsAes128) => SourceProtection::HlsAes128,
            (_, Protection::Mixed) => SourceProtection::Mixed,
            _ => SourceProtection::Clear,
        };
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
            variant_groups.push(attrs.get("AUDIO").map(str::to_owned));
            pending = true;
        } else if !line.starts_with('#') {
            if !pending {
                return Err(ConversionError("hls-unexpected-master-resource"));
            }
            paths.push(MediaPath {
                path: hls::local_resource(&root, line)?,
                // Resolve variant roles only after every rendition declaration is known.
                role: None,
                variants: 0,
                metadata: pending_metadata
                    .take()
                    .ok_or(ConversionError("hls-variant-uri-required"))?,
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
    let mut seen = std::collections::BTreeMap::new();
    let mut unique = Vec::new();
    for mut media in paths {
        if separate_audio && media.role.is_none() {
            media.role = Some(TrackKind::Video);
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
        for source in prepared {
            if source.source != source.media {
                let verified = if source.audio_presentation.is_some() {
                    self.backend.verify_transport(
                        raw_audio_view(source, input_keys),
                        super::ts::View {
                            input: &source.media,
                            metadata: &source.media,
                            index: source.index,
                            key: key_for(source, input_keys),
                            all_keys: None,
                            transport: false,
                            native: None,
                            unedited: true,
                        }
                        .with_all_keys(input_keys),
                    )?
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
                            .with_metadata(&source.media),
                        StreamView::new(&source.media, source.index, key_for(source, input_keys)),
                    )?
                };
                if !verified.passed {
                    return Err(ConversionError("source-decryption-verification-failed"));
                }
            }
            tracks.push(LocalTrack {
                source: source.media.clone(),
                kind: source.report.tracks[0].kind,
                index: source.index,
            });
        }
        Ok(())
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
            &self.backend,
            self.maximum_workers,
            SourceValidationPurpose::Verification,
        )?;
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
            let frozen = super::FrozenSource::capture(request.input, &self.backend.stop)?;
            return self
                .using_frozen_source(&frozen)
                .convert_inner(ConversionRequest {
                    input: frozen.entrypoint(),
                    ..request
                });
        }
        self.convert_inner(request)
    }
    fn convert_inner(
        &self,
        request: ConversionRequest<'_>,
    ) -> Result<ConversionReport, ConversionError> {
        self.validate_signaling_input(&request)?;
        validate_output(&request)?;
        let work = workspace()?;
        let input = VerificationSource {
            input: request.input,
            input_keys: request.input_keys,
            hls_keys: request.hls_input_keys,
        };
        let input_report = self.inspect_input(request.input)?;
        if request.format == OutputFormat::HlsTs && input_report.format != InputFormat::Hls {
            return Err(ConversionError("ts-original-evidence-required"));
        }
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
        let transport = prepared.iter().any(|p| p.transport_index.is_some());
        let sample_transport = prepared.iter().any(|p| p.transport_sample);
        if request.format == OutputFormat::HlsTs {
            return self.convert_transport_output(&request, prepared, input_report, &work);
        }
        validate_prepared_output(&request, &prepared, transport)?;
        if request.format == OutputFormat::Mp4
            && prepared.iter().any(|p| p.audio_presentation.is_some())
        {
            return Err(ConversionError("audio-presentation-range-not-supported"));
        }
        let presentations: Vec<_> = prepared.iter().map(|p| p.audio_presentation).collect();
        let descriptions: Vec<_> = prepared.iter().map(|p| p.metadata.clone()).collect();
        let mut tracks = Vec::new();
        self.verify_source_tracks(&prepared, &mut tracks, request.input_keys)?;
        let destination = transport.then(|| destination(request.output)).transpose()?;
        let stage = destination
            .as_ref()
            .map(|path| self.output_stage(path))
            .transpose()?;
        let candidate = stage.as_ref().map(|stage| stage.path().to_path_buf());
        let mut report = self.convert_tracks_inner(
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
