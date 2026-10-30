//! MPEG-TS output is independently checked against complete original samples and clocks.
use super::{
    ConversionError, ConversionReport, ConversionRequest, Converter, InputKeys, Inspection, Path,
    Prepared, TargetProtection, TrackKind, VerificationSource, Zeroizing, copy_private,
    descriptor_path, destination, hls, inspection, kind_order, prepare_presentation, secure_tree,
};
use std::{io::Write as _, path::PathBuf};

pub(super) fn fragmented_sources(sources: &[Prepared]) -> Result<bool, ConversionError> {
    if sources.is_empty()
        || sources
            .iter()
            .any(|source| source.transport_index.is_some())
    {
        return Ok(false);
    }
    for source in sources {
        if !super::super::audio::movie(&source.media)?.fragmented {
            return Ok(false);
        }
    }
    Ok(true)
}

fn descriptor(path: &Path) -> Result<String, ConversionError> {
    let value = descriptor_path(path)?;
    if value.contains('$') {
        return Err(ConversionError("unsupported-path"));
    }
    Ok(value)
}
fn require_transport_sources(sources: &[Prepared]) -> Result<(), ConversionError> {
    if sources.is_empty()
        || sources.len() > 64
        || sources.iter().any(|source| {
            source.transport_index.is_none()
                || source.transport_native.is_none()
                || !matches!(
                    source.report.tracks[0].kind,
                    TrackKind::Video | TrackKind::Audio
                )
        })
    {
        return Err(ConversionError("ts-original-evidence-required"));
    }
    Ok(())
}
impl Converter {
    pub(super) fn verify_transport_pair(
        &self,
        source: &Prepared,
        result: &Prepared,
    ) -> Result<super::VerificationReport, ConversionError> {
        let source_index = source
            .transport_index
            .ok_or(ConversionError("ts-original-evidence-required"))?;
        let result_index = result
            .transport_index
            .ok_or(ConversionError("ts-original-evidence-required"))?;
        if source.transport_native.is_none() || result.transport_native.is_none() {
            return Err(ConversionError("ts-original-evidence-required"));
        }
        self.backend.verify_transport(
            super::super::ts::View {
                input: &source.source,
                metadata: &source.source,
                index: source_index,
                key: None,
                all_keys: None,
                transport: true,
                native: source.transport_native.as_deref(),
                unedited: false,
            },
            super::super::ts::View {
                input: &result.source,
                metadata: &result.source,
                index: result_index,
                key: None,
                all_keys: None,
                transport: true,
                native: result.transport_native.as_deref(),
                unedited: false,
            },
        )
    }

    pub(super) fn convert_transport_output(
        &self,
        request: &ConversionRequest<'_>,
        mut sources: Vec<Prepared>,
        input: Inspection,
        work: &super::super::files::Stage,
    ) -> Result<ConversionReport, ConversionError> {
        if self.layout_mode() == crate::conversion::layout::LayoutMode::Normalized {
            sources.sort_by_key(|source| kind_order(source.report.tracks[0].kind));
        }
        let package_inputs = self.transport_package_inputs(&sources, request, work.path())?;
        let transport = package_inputs.is_none();
        let sample_input = sources.iter().any(|source| source.transport_sample);
        let destination = destination(request.output)?;
        let stage = self.output_stage(&destination)?;
        let clear = work.path().join("clear-ts-output");
        let package_root = if request.target == TargetProtection::HlsAes128 {
            super::super::files::create_dir(&clear)?;
            clear.as_path()
        } else {
            stage.path()
        };
        self.package_transport(&sources, package_inputs.as_deref(), package_root, request)?;
        super::metadata::finish(
            package_root,
            crate::inspection::OutputFormat::HlsTs,
            &sources,
        )?;
        if sources.len() == 1
            && sources[0].report.tracks[0].kind == TrackKind::Audio
            && sources[0].metadata.audio.is_none()
        {
            single_audio_variant(package_root)?;
        }
        if request.target == TargetProtection::HlsAes128 {
            let key = request
                .hls_output_key
                .ok_or(ConversionError("output-key-role-mismatch"))?;
            for ordinal in 0..sources.len() {
                let name = format!("track-{ordinal:04}");
                let output = stage.path().join(&name);
                super::super::files::create_dir(&output)?;
                hls::encrypt_transport_media(&package_root.join(name), &output, key)?;
            }
            copy_private(
                &package_root.join("master.m3u8"),
                &stage.path().join("master.m3u8"),
            )?;
        }
        self.finish_transport_audio(stage.path(), &sources, request)?;
        validate_playlists(stage.path(), sources.len(), request)?;
        self.finish_multi_signaling(
            stage.path(),
            request.format,
            request.target,
            request.output_key,
            request.hls_key_uri,
            request.hls_output_key,
        )?;
        self.finish_hls_segment_target(stage.path(), sources.len())?;
        self.finish_output_options(&stage, request.format)?;
        let output_keys = super::output_hls_keys(request.hls_output_key)?;
        let verification_root = work.path().join("ts-output-verification");
        super::super::files::create_dir(&verification_root)?;
        let empty = InputKeys::default();
        let result = prepare_presentation(
            &VerificationSource {
                input: &stage.path().join("master.m3u8"),
                input_keys: &empty,
                hls_keys: &output_keys,
            },
            &verification_root,
            &self.backend,
            self.maximum_workers,
            super::SourceValidationPurpose::Verification,
        )?;
        let mut verification =
            self.compare_presentations(sources, result, request.input_keys, &empty)?;
        if !verification.passed {
            return Err(ConversionError("round-trip-verification-failed"));
        }
        let output = inspection::inspect(&stage.path().join("master.m3u8"))
            .map_err(|_| ConversionError("invalid-output-media"))?;
        secure_tree(stage.path())?;
        self.backend.checkpoint(&[work, &stage]);
        verification.native_manifest = Some(self.publish_manifest_output(
            &stage,
            &destination,
            &stage.path().join("master.m3u8"),
            request.format,
        )?);
        let input_format = input.format;
        Ok(ConversionReport {
            schema_version: 2,
            input,
            output,
            verification,
            processing: Some(transport_processing(
                &self.backend,
                input_format,
                transport,
                sample_input,
            )),
        })
    }
    fn transport_package_inputs(
        &self,
        sources: &[Prepared],
        request: &ConversionRequest<'_>,
        work: &Path,
    ) -> Result<Option<Vec<PathBuf>>, ConversionError> {
        if sources
            .iter()
            .all(|source| source.transport_index.is_some())
        {
            require_transport_sources(sources)?;
            return Ok(None);
        }
        if !fragmented_sources(sources)? {
            return Err(ConversionError("ts-output-profile-not-supported"));
        }
        self.fragmented_transport_inputs(sources, request, work)
            .map(Some)
    }
    fn fragmented_transport_inputs(
        &self,
        sources: &[Prepared],
        request: &ConversionRequest<'_>,
        work: &Path,
    ) -> Result<Vec<PathBuf>, ConversionError> {
        if !self
            .validate_prepared_sources(sources, request.input_keys)?
            .passed
        {
            return Err(ConversionError("source-media-verification-failed"));
        }
        for source in sources {
            if source.audio_presentation.is_none() {
                let movie = super::super::native_media::inspect(&source.media, &self.backend.stop)?;
                let track = movie
                    .tracks
                    .get(source.index)
                    .ok_or(ConversionError("stream-selector-mismatch"))?;
                let representable = match track.edits.as_slice() {
                    [] => true,
                    [edit]
                        if edit.duration == 0
                            && edit.rate_integer == 1
                            && edit.rate_fraction == 0 =>
                    {
                        // A video's unbounded composition correction translates both
                        // PTS and DTS. AAC priming still requires sample-level trimming.
                        edit.media_time == 0
                            || (track.kind == TrackKind::Video && edit.media_time > 0)
                    }
                    _ => false,
                };
                if !representable {
                    return Err(ConversionError("ts-output-presentation-not-representable"));
                }
            }
        }
        let tracks = sources
            .iter()
            .map(|source| super::LocalTrack {
                source: source.media.clone(),
                index: source.index,
                kind: source.report.tracks[0].kind,
            })
            .collect::<Vec<_>>();
        let clocks = sources
            .iter()
            .map(super::TrackClock::from_prepared)
            .collect::<Vec<_>>();
        let directory = work.join("transport-inputs");
        super::super::files::create_dir(&directory)?;
        let (_, clears) = self.normalize_tracks(
            &super::TrackConversionRequest {
                tracks: &tracks,
                output: request.output,
                format: request.format,
                target: request.target,
                input_keys: request.input_keys,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: request.hls_output_key,
            },
            &directory,
            Some(&clocks),
        )?;
        Ok(clears)
    }
    fn finish_transport_audio(
        &self,
        root: &Path,
        sources: &[Prepared],
        request: &ConversionRequest<'_>,
    ) -> Result<(), ConversionError> {
        let keys = super::output_hls_keys(request.hls_output_key)?;
        for (ordinal, source) in sources.iter().enumerate() {
            if let Some(range) = source.audio_presentation {
                super::super::audio_presentation::finish_transport_hls(
                    root,
                    ordinal,
                    range,
                    &keys,
                    &self.backend,
                )?;
            }
        }
        Ok(())
    }
    fn package_transport(
        &self,
        sources: &[Prepared],
        inputs: Option<&[PathBuf]>,
        root: &Path,
        request: &ConversionRequest<'_>,
    ) -> Result<(), ConversionError> {
        let _scope = self
            .backend
            .scope(super::super::measurement::Phase::Packaging);
        let mut argv = Zeroizing::new(vec![self.backend.packager.clone()]);
        for (ordinal, source) in sources.iter().enumerate() {
            let name = format!("track-{ordinal:04}");
            let directory = root.join(&name);
            super::super::files::create_dir(&directory)?;
            // Feeding original multi-frame AAC PES into a foreign TS parser can
            // round 44.1 kHz clocks again. The native MP4 keeps every original
            // 90 kHz frame timestamp and avoids that extra interpolation.
            let exact_audio =
                super::super::ts::requires_native_audio_mp4(source.transport_native.as_deref());
            let input = descriptor(if let Some(inputs) = inputs {
                inputs
                    .get(ordinal)
                    .ok_or(ConversionError("presentation-track-count-mismatch"))?
            } else if exact_audio {
                &source.media
            } else {
                source
                    .transport_demux_view
                    .as_deref()
                    .unwrap_or(&source.source)
            })?;
            let output = descriptor(&directory)?;
            let index = if inputs.is_some() {
                0
            } else if exact_audio {
                source.index
            } else {
                source
                    .transport_index
                    .ok_or(ConversionError("ts-original-evidence-required"))?
            };
            argv.push(format!("input={input},stream={index},segment_template={output}/segment-$Number$.ts,playlist_name={name}/media.m3u8"));
        }
        argv.extend([
            "--segment_duration".into(),
            self.segment_target_argument()?,
            "--clear_lead".into(),
            "0".into(),
            "--transport_stream_timestamp_offset_ms".into(),
            "0".into(),
            "--mp4_reset_initial_composition_offset_to_zero=false".into(),
            "--hls_playlist_type".into(),
            "VOD".into(),
            "--hls_master_playlist_output".into(),
            descriptor(&root.join("master.m3u8"))?,
        ]);
        let mut secrets = Zeroizing::new(Vec::<String>::new());
        if request.target == TargetProtection::HlsSampleAes {
            let key = request
                .hls_output_key
                .ok_or(ConversionError("output-key-role-mismatch"))?;
            let iv = key
                .associated_iv()
                .ok_or(ConversionError("hls-sample-output-iv-required"))?;
            secrets.extend([
                hex::encode(key.key.0.as_ref()),
                hex::encode(iv.0.as_ref()),
                key.key_uri().into(),
            ]);
            argv.extend([
                "--enable_raw_key_encryption".into(),
                "--keys".into(),
                format!(
                    "label=:key_id=00000000000000000000000000000000:key={}",
                    secrets[0]
                ),
                "--iv".into(),
                secrets[1].clone(),
                "--protection_scheme".into(),
                "cbcs".into(),
                "--protection_systems".into(),
                "FairPlay".into(),
                "--hls_key_uri".into(),
                key.key_uri().into(),
            ]);
        }
        let redacted = secrets.iter().map(String::as_str).collect::<Vec<_>>();
        self.backend.run(&argv, "packager", &redacted)?;
        Ok(())
    }
}
fn validate_playlists(
    root: &Path,
    tracks: usize,
    request: &ConversionRequest<'_>,
) -> Result<(), ConversionError> {
    for ordinal in 0..tracks {
        let path = root.join(format!("track-{ordinal:04}/media.m3u8"));
        let bytes = super::super::files::read_manifest(&path)?;
        let text =
            std::str::from_utf8(&bytes).map_err(|_| ConversionError("invalid-output-media"))?;
        let mut protected = request.target == TargetProtection::Clear;
        for line in text.lines().map(str::trim) {
            if line.starts_with("#EXT-X-MAP:") {
                return Err(ConversionError("ts-output-map-not-supported"));
            }
            if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
                let attrs = hls::attributes(value)?;
                let valid = match request.target {
                    TargetProtection::Clear => attrs.get("METHOD") == Some("NONE"),
                    TargetProtection::HlsAes128 | TargetProtection::HlsSampleAes => {
                        let key = request
                            .hls_output_key
                            .ok_or(ConversionError("output-key-role-mismatch"))?;
                        attrs.get("URI") == Some(key.key_uri())
                            && if request.target == TargetProtection::HlsAes128 {
                                attrs.get("METHOD") == Some("AES-128") && attrs.get("IV").is_some()
                            } else {
                                attrs.get("METHOD") == Some("SAMPLE-AES")
                                    && attrs.get("KEYFORMAT")
                                        == Some("com.apple.streamingkeydelivery")
                                    && attrs.get("KEYFORMATVERSIONS") == Some("1")
                            }
                    }
                    _ => false,
                };
                if !valid {
                    return Err(ConversionError("output-protection-mismatch"));
                }
                protected = true;
            } else if !line.is_empty()
                && !line.starts_with('#')
                && (!protected
                    || !Path::new(line)
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("ts")))
            {
                return Err(ConversionError("output-protection-mismatch"));
            }
        }
    }
    Ok(())
}

pub(super) fn single_audio_variant(root: &Path) -> Result<(), ConversionError> {
    let path = root.join("master.m3u8");
    let bytes = super::super::files::read_manifest(&path)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| ConversionError("invalid-output-media"))?;
    let mut master = String::new();
    let mut variants = 0;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attrs = hls::attributes(value)?;
            if attrs.get("TYPE") != Some("AUDIO")
                || attrs.get("URI") != Some("track-0000/media.m3u8")
            {
                return Err(ConversionError("ts-presentation-layout-not-supported"));
            }
            // The sole audio variant already references this exact media playlist.
            continue;
        }
        if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            variants += 1;
            let attrs = hls::attributes(value)?;
            if attrs.get("CODECS") != Some("mp4a.40.2")
                || attrs.iter().any(|(name, _)| {
                    !matches!(
                        name,
                        "BANDWIDTH" | "AVERAGE-BANDWIDTH" | "CODECS" | "AUDIO" | "CLOSED-CAPTIONS"
                    )
                })
            {
                return Err(ConversionError("ts-presentation-layout-not-supported"));
            }
            let fields = attrs
                .iter()
                .filter(|(name, _)| *name != "AUDIO")
                .map(|(name, value)| {
                    if name == "CODECS" {
                        format!("{name}=\"{value}\"")
                    } else {
                        format!("{name}={value}")
                    }
                })
                .collect::<Vec<_>>();
            master.push_str("#EXT-X-STREAM-INF:");
            master.push_str(&fields.join(","));
        } else {
            if !line.is_empty() && !line.starts_with('#') && line != "track-0000/media.m3u8" {
                return Err(ConversionError("ts-presentation-layout-not-supported"));
            }
            master.push_str(line);
        }
        master.push('\n');
    }
    if variants != 1 {
        return Err(ConversionError("ts-presentation-layout-not-supported"));
    }
    // External tools can create ordinary file modes inside the private workspace.
    // Validate and seal the owned tree before acquiring a strict writable leaf.
    secure_tree(root)?;
    crate::native::create_media_or_truncate(&path)
        .map_err(|_| ConversionError("output-staging-failed"))?
        .write_all(master.as_bytes())
        .map_err(|_| ConversionError("output-staging-failed"))
}

fn transport_processing(
    backend: &super::super::backend::Backend,
    input: inspection::InputFormat,
    transport: bool,
    sample_input: bool,
) -> inspection::ActualProcessing {
    let route = if transport {
        inspection::ProcessingRouteId::MpegTsHlsTs
    } else {
        inspection::ProcessingRouteId::Fmp4HlsTs
    };
    let preparation = if !transport {
        match input {
            inspection::InputFormat::Dash => inspection::InputPreparation::LocalDashResources,
            inspection::InputFormat::Hls => inspection::InputPreparation::LocalHlsResources,
            inspection::InputFormat::Mp4 => inspection::InputPreparation::CompleteFile,
        }
    } else if sample_input {
        inspection::InputPreparation::MpegTsSampleAesDecrypt
    } else {
        inspection::InputPreparation::MpegTsDemux
    };
    backend.processing(route, preparation)
}
