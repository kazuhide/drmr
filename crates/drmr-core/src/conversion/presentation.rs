//! Joint, codec-preserving presentations with independent evidence for every source stream.
use super::{
    ContentKey, ConversionError, ConversionReport, Converter, HlsOutputKey, InputKeys, LocalTrack,
    OutputKey, VerificationChecks, VerificationReport, audio, copy_private, destination, hls,
    verify::StreamView, workspace,
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
struct Prepared {
    source_dictionary: bool,
    source: PathBuf,
    media: PathBuf,
    index: usize,
    report: Inspection,
    key_id: Option<String>,
    transport_index: Option<usize>,
    playlist_reference: Option<PathBuf>,
}
fn presentation_processing(format: OutputFormat) -> inspection::ActualProcessing {
    inspection::ActualProcessing::new(
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
        validate_request(&request)?;
        if request.format == OutputFormat::Mp4 {
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
            let actual = inspection::inspect(&snapshot)
                .map_err(|_| ConversionError("invalid-input-media"))?;
            if actual.tracks.len() != request.tracks.len() {
                return Err(ConversionError("mp4-requires-one-complete-presentation"));
            }
            for track in request.tracks {
                prepare(&snapshot, track.index, track.kind, request.input_keys)?;
            }
            return self.convert_mp4(
                &snapshot,
                request.output,
                request.target,
                request.input_keys,
                request.output_key,
            );
        }
        let destination = destination(request.output)?;
        let work = workspace()?;
        let (sources, clears) = self.normalize_tracks(&request, work.path())?;
        let parent = destination
            .parent()
            .ok_or(ConversionError("output-path-invalid"))?;
        let stage = super::files::Stage::new(Some(parent))?;
        let clear_stage = work.path().join("packaged");
        super::files::create_dir(&clear_stage)
            .map_err(|_| ConversionError("output-staging-failed"))?;
        let package_root = if request.target == TargetProtection::HlsAes128 {
            clear_stage.as_path()
        } else {
            stage.path()
        };
        self.package_joint(&clears, package_root, &request)?;
        if let Some(key) = request.hls_output_key {
            for ordinal in 0..clears.len() {
                let name = format!("track-{ordinal:04}");
                let output = stage.path().join(&name);
                super::files::create_dir(&output)
                    .map_err(|_| ConversionError("output-staging-failed"))?;
                hls::encrypt_media(&package_root.join(name), &output, key)?;
            }
            copy_private(
                &package_root.join("master.m3u8"),
                &stage.path().join("master.m3u8"),
            )?;
        }
        let (checks, output_tracks, input_tracks) =
            self.verify_joint(&sources, &request, stage.path(), work.path())?;
        secure_tree(stage.path())?;
        let format = if request.format == OutputFormat::Dash {
            InputFormat::Dash
        } else {
            InputFormat::Hls
        };
        let output = Inspection::new(
            format,
            Evidence::ContainerMetadata,
            output_tracks,
            Vec::new(),
            Vec::new(),
        );
        let input = Inspection::new(
            InputFormat::Mp4,
            Evidence::ContainerMetadata,
            input_tracks,
            Vec::new(),
            Vec::new(),
        );
        self.backend.checkpoint(&[&work, &stage]);
        self.publish_directory(stage.path(), &destination)?;
        Ok(ConversionReport {
            schema_version: 1,
            input,
            output,
            verification: VerificationReport {
                schema_version: 1,
                passed: true,
                media_verified: true,
                checks,
                license_playback_verified: false,
            },
            processing: Some(presentation_processing(request.format)),
        })
    }
    fn verify_joint(
        &self,
        sources: &[Prepared],
        request: &TrackConversionRequest<'_>,
        stage: &Path,
        work: &Path,
    ) -> Result<
        (
            VerificationChecks,
            Vec<inspection::Track>,
            Vec<inspection::Track>,
        ),
        ConversionError,
    > {
        let mut checks = VerificationChecks {
            stream_metadata: super::CheckOutcome::Pass,
            packet_payloads_equal: true,
            packet_timing_equal: true,
            decoded_content_equal: true,
        };
        let mut output_tracks = Vec::new();
        let mut input_tracks = Vec::new();
        let verified_tracks = self.run_tracks(sources.len(), |ordinal| {
            let source = &sources[ordinal];
            let track_dir = stage.join(format!("track-{ordinal:04}"));
            let combined = work.join(format!("result-{ordinal}.mp4"));
            let (reference, actual) = if let Some(key) = request.hls_output_key {
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
                    &self.backend.stop,
                )?;
                copy_private(&localized.combined, &combined)?;
                (
                    localized.reference,
                    inspection::inspect(&combined)
                        .map_err(|_| ConversionError("invalid-output-media"))?,
                )
            } else {
                combine_generated(&track_dir, &combined)?;
                (
                    combined.clone(),
                    inspection::inspect(&combined)
                        .map_err(|_| ConversionError("invalid-output-media"))?,
                )
            };
            let expected = match request.target {
                TargetProtection::Clear | TargetProtection::HlsAes128 => Protection::Clear,
                TargetProtection::Cenc => Protection::Cenc,
                TargetProtection::Cbcs => Protection::Cbcs,
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
            let verified = self.backend.verify_views(
                StreamView::new(
                    &source.source,
                    source.index,
                    key_for(source, request.input_keys),
                )
                .with_metadata(&source.media),
                StreamView::new(&reference, 0, request.output_key.map(|k| &k.0))
                    .with_metadata(&combined),
            )?;
            let mut output = actual.tracks[0].clone();
            output.id = format!("track-{ordinal:04}");
            if request.target == TargetProtection::HlsAes128 {
                output.protection = Protection::HlsAes128;
            }
            let mut input = source.report.tracks[0].clone();
            input.id = format!("track-{ordinal:04}");
            Ok((verified.checks, input, output))
        })?;
        for (verified, input, output) in verified_tracks {
            checks.stream_metadata.include(verified.stream_metadata);
            checks.packet_payloads_equal &= verified.packet_payloads_equal;
            checks.packet_timing_equal &= verified.packet_timing_equal;
            checks.decoded_content_equal &= verified.decoded_content_equal;
            input_tracks.push(input);
            output_tracks.push(output);
        }
        let passed = checks.stream_metadata.is_pass()
            && checks.packet_payloads_equal
            && checks.packet_timing_equal
            && checks.decoded_content_equal;
        if !passed {
            return Err(ConversionError("round-trip-verification-failed"));
        }
        Ok((checks, output_tracks, input_tracks))
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
            let prepared = prepare(&source, track.index, track.kind, request.input_keys)?;
            let profile = self.backend.profile_stream(
                &source,
                track.index,
                key_for(&prepared, request.input_keys),
            )?;
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
            self.backend.package_stream(
                &source.media,
                source.index,
                &clear,
                source.key_id.is_some().then_some(request.input_keys),
                None,
            )?;
            drop(normalization_scope);
            let normalization = self.backend.verify_streams(
                &source.source,
                source.index,
                &clear,
                0,
                key_for(source, request.input_keys),
                None,
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
        let mut argv = Zeroizing::new(vec![
            self.backend.packager.clone(),
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
        let secret = request.output_key.map(|k| k.0.key_hex());
        if let (Some(key), Some(secret)) = (request.output_key, secret.as_ref()) {
            argv.extend([
                "--enable_raw_key_encryption".into(),
                "--protection_scheme".into(),
                request.target.as_str().into(),
                "--keys".into(),
                format!("label=:key_id={}:key={}", key.0.id_hex(), secret.as_str()),
            ]);
            if request.format == OutputFormat::Hls {
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
            .collect();
        self.backend.run(&argv, "packager", &secrets)?;
        Ok(())
    }
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
        && !(request.target == TargetProtection::Clear && request.tracks.len() == 2)
    {
        return Err(ConversionError("multi-track-mp4-output-not-supported"));
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
#[derive(Clone, Copy)]
enum Preparation {
    Conversion,
    Verification,
}
fn prepare(
    source: &Path,
    index: usize,
    kind: TrackKind,
    keys: &InputKeys,
) -> Result<Prepared, ConversionError> {
    prepare_for(source, index, kind, keys, Preparation::Conversion)
}
fn prepare_for(
    source: &Path,
    index: usize,
    kind: TrackKind,
    keys: &InputKeys,
    purpose: Preparation,
) -> Result<Prepared, ConversionError> {
    let report = inspection::inspect(source).map_err(|_| ConversionError("invalid-input-media"))?;
    if report.format != InputFormat::Mp4 || report.tracks.len() > 64 {
        return Err(ConversionError("input-format-not-supported"));
    }
    let source_dictionary = report.protection == Protection::Cenc
        && match purpose {
            Preparation::Conversion => super::progressive::validate_input(source, &report, keys)?,
            Preparation::Verification => {
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
        source_dictionary,
        source: source.into(),
        media: source.into(),
        index,
        report: selected,
        key_id,
        transport_index: None,
        playlist_reference: None,
    })
}
fn key_for<'a>(source: &Prepared, keys: &'a InputKeys) -> Option<&'a ContentKey> {
    source
        .key_id
        .as_ref()
        .and_then(|id| keys.keys.iter().find(|k| k.id_hex() == *id))
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
/// Compare all actual source and candidate tracks by media role and stable ordinal.
#[derive(Clone, Copy)]
pub struct VerificationRequest<'a> {
    pub source: VerificationSource<'a>,
    pub output: VerificationSource<'a>,
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
        let source = prepare_presentation(&request.source, &source_dir, &self.backend)?;
        let result = prepare_presentation(&request.output, &result_dir, &self.backend)?;
        drop(preparation);
        let report = self.compare_presentations(
            source,
            result,
            request.source.input_keys,
            request.output.input_keys,
        );
        if report.is_ok() {
            self.backend.checkpoint(&[&work]);
        }
        report
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
        let mut checks = VerificationChecks {
            stream_metadata: super::CheckOutcome::Pass,
            packet_payloads_equal: true,
            packet_timing_equal: true,
            decoded_content_equal: true,
        };
        if source
            .iter()
            .zip(&result)
            .any(|(a, b)| a.report.tracks[0].kind != b.report.tracks[0].kind)
        {
            return Err(ConversionError("presentation-track-role-mismatch"));
        }
        let verified_tracks = self.run_tracks(source.len(), |ordinal| {
            let a = &source[ordinal];
            let b = &result[ordinal];
            if b.transport_index.is_some() {
                return Err(ConversionError(
                    "ts-verification-output-profile-not-supported",
                ));
            }
            if let Some(index) = a.transport_index {
                let playlist = b.playlist_reference.as_ref().ok_or(ConversionError(
                    "ts-verification-output-profile-not-supported",
                ))?;
                return self.backend.verify_transport(
                    super::ts::View {
                        input: &a.source,
                        metadata: &a.source,
                        index,
                        key: None,
                        transport: true,
                    },
                    super::ts::View {
                        input: playlist,
                        // The owned playlist applies its private key URI to every probe.
                        metadata: playlist,
                        index: b.index,
                        key: None,
                        transport: false,
                    },
                );
            }
            let a_view = StreamView::new(&a.source, a.index, key_for(a, source_keys))
                .with_metadata(&a.media);
            let b_view = StreamView::new(&b.source, b.index, key_for(b, result_keys))
                .with_metadata(&b.media);
            self.backend.verify_views(
                if a.source_dictionary {
                    a_view.with_all_keys(source_keys)
                } else {
                    a_view
                },
                if b.source_dictionary {
                    b_view.with_all_keys(result_keys)
                } else {
                    b_view
                },
            )
        })?;
        for check in verified_tracks {
            checks.stream_metadata.include(check.checks.stream_metadata);
            checks.packet_payloads_equal &= check.checks.packet_payloads_equal;
            checks.packet_timing_equal &= check.checks.packet_timing_equal;
            checks.decoded_content_equal &= check.checks.decoded_content_equal;
        }
        let passed = checks.stream_metadata.is_pass()
            && checks.packet_payloads_equal
            && checks.packet_timing_equal
            && checks.decoded_content_equal;
        Ok(VerificationReport {
            schema_version: 1,
            passed,
            media_verified: passed,
            checks,
            license_playback_verified: false,
        })
    }
}
fn kind_order(kind: TrackKind) -> u8 {
    match kind {
        TrackKind::Video => 0,
        TrackKind::Audio => 1,
        TrackKind::Text => 2,
        TrackKind::Unknown => 3,
    }
}
fn prepare_presentation(
    input: &VerificationSource<'_>,
    stage: &Path,
    backend: &super::backend::Backend,
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
                    prepare_for(
                        &path,
                        index,
                        track.kind,
                        input.input_keys,
                        Preparation::Verification,
                    )
                })
                .collect()
        }
        InputFormat::Hls => {
            let paths = hls_media_paths(input.input)?;
            let mut tracks = Vec::new();
            for (ordinal, path) in paths.iter().enumerate() {
                let directory = stage.join(format!("track-{ordinal:04}"));
                super::files::create_dir(&directory)
                    .map_err(|_| ConversionError("temporary-workspace-unavailable"))?;
                let localized = hls::localize(path, &directory, input.hls_keys, &backend.stop)?;
                if let Some(admission) = &localized.transport {
                    if paths.len() != 1 {
                        return Err(ConversionError("ts-single-media-playlist-required"));
                    }
                    let streams = backend.transport_streams(&localized.combined, admission)?;
                    let clock_index = streams
                        .iter()
                        .find(|(_, kind)| *kind == TrackKind::Video)
                        .unwrap_or(&streams[0])
                        .0;
                    backend.transport_durations(&localized.transport_segments, clock_index)?;
                    for (index, kind) in streams {
                        let root = directory.join(format!("demux-{index}"));
                        super::files::create_dir(&root)?;
                        let media = root.join("track.mp4");
                        backend.demux_transport(&localized.combined, kind, &media)?;
                        let mut track = prepare(&media, 0, kind, input.input_keys)?;
                        track.source.clone_from(&localized.reference);
                        track.transport_index = Some(index);
                        tracks.push(track);
                    }
                    continue;
                }
                let actual = inspection::inspect(&localized.combined)
                    .map_err(|_| ConversionError("invalid-localized-hls-media"))?;
                if actual.tracks.len() != 1 {
                    return Err(ConversionError("hls-multiplexed-media-not-supported"));
                }
                let mut track = prepare(
                    &localized.combined,
                    0,
                    actual.tracks[0].kind,
                    input.input_keys,
                )?;
                track.playlist_reference = if track.key_id.is_none() {
                    Some(localized.playlist.clone())
                } else if actual.protection == Protection::Cbcs {
                    Some(hls::sample_reference(
                        &localized.playlist,
                        key_for(&track, input.input_keys)
                            .ok_or(ConversionError("input-key-required"))?,
                    )?)
                } else {
                    None
                };
                track.source = localized.reference;
                tracks.push(track);
            }
            Ok(tracks)
        }
        InputFormat::Dash => {
            let tracks = super::dash::snapshot_dash(input.input, stage)?;
            tracks
                .iter()
                .map(|track| prepare(&track.source, track.index, track.kind, input.input_keys))
                .collect()
        }
    }
}
fn hls_media_paths(path: &Path) -> Result<Vec<PathBuf>, ConversionError> {
    let bytes = super::files::read_manifest(path)
        .map_err(|_| ConversionError("hls-manifest-unreadable-or-too-large"))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ConversionError("hls-invalid-utf8"))?;
    if !text
        .lines()
        .any(|l| l.starts_with("#EXT-X-STREAM-INF:") || l.starts_with("#EXT-X-MEDIA:"))
    {
        return Ok(vec![path.into()]);
    }
    let root = path
        .parent()
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|_| ConversionError("input-unreadable"))?;
    let mut paths = Vec::new();
    let mut pending = false;
    let mut audio_count = 0;
    let mut audio_group = None;
    let mut variant_groups = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attrs = hls::attributes(value)?;
            audio_count += 1;
            if audio_count > 1 || !supported_audio_rendition(&attrs) {
                return Err(ConversionError("hls-rendition-metadata-not-supported"));
            }
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
            paths.push(hls::local_resource(&root, uri)?);
        } else if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = hls::attributes(value)?;
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
            if pending {
                return Err(ConversionError("hls-variant-uri-required"));
            }
            if variant_groups.len() == 64 {
                return Err(ConversionError("track-count-limit"));
            }
            variant_groups.push(attrs.get("AUDIO").map(str::to_owned));
            pending = true;
        } else if !line.starts_with('#') {
            if !pending {
                return Err(ConversionError("hls-unexpected-master-resource"));
            }
            paths.push(hls::local_resource(&root, line)?);
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
    let mut seen = std::collections::BTreeSet::new();
    paths.retain(|p| seen.insert(p.clone()));
    Ok(paths)
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
impl Converter {
    fn verify_source_tracks(
        &self,
        prepared: &[Prepared],
        tracks: &mut Vec<LocalTrack>,
        input_keys: &InputKeys,
    ) -> Result<(), ConversionError> {
        for source in prepared {
            if source.source != source.media {
                let verified = if let Some(index) = source.transport_index {
                    self.backend.verify_transport(
                        super::ts::View {
                            input: &source.source,
                            metadata: &source.source,
                            index,
                            key: None,
                            transport: true,
                        },
                        super::ts::View {
                            input: &source.media,
                            metadata: &source.media,
                            index: 0,
                            key: None,
                            transport: false,
                        },
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
    pub fn convert(
        &self,
        request: ConversionRequest<'_>,
    ) -> Result<ConversionReport, ConversionError> {
        let work = workspace()?;
        let input = VerificationSource {
            input: request.input,
            input_keys: request.input_keys,
            hls_keys: request.hls_input_keys,
        };
        let input_report = inspection::inspect(request.input)
            .map_err(|_| ConversionError("input-inspection-failed"))?;
        let preparation = self
            .backend
            .scope(super::measurement::Phase::InputPreparation);
        let prepared = prepare_presentation(&input, work.path(), &self.backend)?;
        drop(preparation);
        let transport = prepared.iter().any(|p| p.transport_index.is_some());
        if transport
            && (request.format != OutputFormat::Hls
                || !matches!(
                    request.target,
                    TargetProtection::Clear | TargetProtection::Cbcs | TargetProtection::HlsAes128
                ))
        {
            return Err(ConversionError("ts-output-profile-not-supported"));
        }
        let mut tracks = Vec::new();
        self.verify_source_tracks(&prepared, &mut tracks, request.input_keys)?;
        let destination = transport.then(|| destination(request.output)).transpose()?;
        let stage = destination
            .as_ref()
            .map(|path| super::files::Stage::new(path.parent()))
            .transpose()?;
        let candidate = stage
            .as_ref()
            .map(|stage| stage.path().join("presentation"));
        let mut report = self.convert_tracks(TrackConversionRequest {
            tracks: &tracks,
            output: candidate.as_deref().unwrap_or(request.output),
            format: request.format,
            target: request.target,
            input_keys: request.input_keys,
            output_key: request.output_key,
            hls_key_uri: request.hls_key_uri,
            hls_output_key: request.hls_output_key,
        })?;
        if let (Some(candidate), Some(destination)) = (&candidate, &destination) {
            let output_keys = request
                .output_key
                .map(|k| InputKeys::new(vec![ContentKey::from_hex(&k.0.id_hex(), &k.0.key_hex())?]))
                .transpose()?
                .unwrap_or_default();
            let hls_keys = request
                .hls_output_key
                .map(|k| {
                    super::HlsInputKeys::new(vec![(
                        k.key_uri().to_owned(),
                        super::Aes128Key(k.key.0.clone()),
                    )])
                })
                .transpose()?
                .unwrap_or_default();
            let verification_root = work.path().join("final-verification");
            super::files::create_dir(&verification_root)?;
            let result = prepare_presentation(
                &VerificationSource {
                    input: &candidate.join("master.m3u8"),
                    input_keys: &output_keys,
                    hls_keys: &hls_keys,
                },
                &verification_root,
                &self.backend,
            )?;
            report.verification =
                self.compare_presentations(prepared, result, request.input_keys, &output_keys)?;
            if !report.verification.passed {
                return Err(ConversionError("round-trip-verification-failed"));
            }
            if let Some(stage) = &stage {
                self.backend.checkpoint(&[&work, stage]);
            }
            self.publish_directory(candidate, destination)?;
        }
        self.backend.checkpoint(&[&work]);
        report.input = input_report;
        if transport {
            report.processing = Some(inspection::ActualProcessing::new(
                inspection::ProcessingRouteId::MpegTsHls,
                inspection::InputPreparation::MpegTsDemux,
            ));
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

fn supported_audio_rendition(attrs: &crate::m3u8::Attributes) -> bool {
    attrs.get("TYPE") == Some("AUDIO")
        && attrs.get("LANGUAGE").is_none_or(|v| v == "und")
        && attrs.get("NAME").is_none_or(|v| {
            v.strip_prefix("stream_")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        && attrs.get("DEFAULT").is_none_or(|v| v == "NO")
        && attrs.get("AUTOSELECT").is_none_or(|v| v == "YES")
        && attrs.iter().all(|(name, _)| {
            matches!(
                name,
                "TYPE"
                    | "URI"
                    | "GROUP-ID"
                    | "NAME"
                    | "DEFAULT"
                    | "AUTOSELECT"
                    | "LANGUAGE"
                    | "CHANNELS"
            )
        })
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
