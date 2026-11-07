//! Standalone MP4 preserves complete tracks and only invertible manifest selection metadata.
use super::{
    ConversionError, ConversionReport, ConversionRequest, Converter, InputKeys, Inspection,
    Prepared, TrackKind, VerificationSource, copy_private, inspection, prepare_presentation,
    workspace,
};
use crate::inspection::{Protection, TargetProtection};
use std::io::{Seek as _, SeekFrom, Write as _};

#[cfg(test)]
mod tests;

pub(super) fn transport_comparison(
    output_format: inspection::OutputFormat,
    source: &Prepared,
    result: &Prepared,
) -> bool {
    // HLS inspection can use joined MP4 bytes without a decoder playlist.
    // Only an actual standalone MP4 result uses the container metadata projection.
    output_format == inspection::OutputFormat::Mp4
        && source.transport_index.is_some()
        && result.transport_index.is_none()
        && result.playlist_reference.is_none()
        && !result.dash_reference
}

pub(super) fn complete_track_views(
    path: &std::path::Path,
    directory: &std::path::Path,
    backend: &super::super::backend::Backend,
    tracks: usize,
) -> Result<Option<Vec<std::path::PathBuf>>, ConversionError> {
    if tracks != 2 || !super::super::audio::movie(path)?.fragmented {
        return Ok(None);
    }
    let movie = super::super::native_media::inspect(path, &backend.stop)?;
    let needs_views = movie.fragmented
        && movie.tracks.len() == 2
        && movie.tracks.iter().any(|track| {
            track.edits.len() == 2
                // A finite joint CBCS input is normalized through independently
                // verified track views, rather than relying on cross-track tool state.
                || (track.codec == *b"mp4a"
                    && track.protection.scheme == super::super::native_media::Scheme::Cbcs
                    && !track.edits.is_empty())
                || (track.codec == *b"mp4a"
                    && track.timescale == 90_000
                    && track.sample_entry_metadata.sample_rate == Some(44_100))
        });
    if needs_views {
        super::super::finite::split_parts(path, directory, &backend.stop).map(Some)
    } else {
        Ok(None)
    }
}

use super::metadata::mp4_language as language;

fn validate_mp4_output_keys(request: &ConversionRequest<'_>) -> Result<(), ConversionError> {
    super::super::key_file::validate_output_key_role(
        request.target,
        request.output_key,
        request.hls_output_key,
    )?;
    if request.hls_output_key.is_some() || request.hls_key_uri.is_some() {
        return Err(ConversionError("output-key-role-mismatch"));
    }
    Ok(())
}

pub(super) fn declared_language(source: &Prepared) -> Result<String, ConversionError> {
    language(
        source
            .metadata
            .audio
            .as_ref()
            .map_or("und", |audio| audio.language.as_str()),
    )
}

pub(super) fn supported_metadata(prepared: &[Prepared]) -> bool {
    prepared
        .iter()
        .all(|source| declared_language(source).is_ok())
}

fn write_language(path: &std::path::Path, value: &str) -> Result<(), ConversionError> {
    let value = language(value)?;
    let movie = super::super::audio::movie(path)?;
    let nodes = crate::mp4::parse_boxes(&movie.bytes)
        .map_err(|_| ConversionError("invalid-output-media"))?;
    let all = crate::mp4::walk(&nodes);
    let headers: Vec<_> = all.iter().filter(|node| node.kind() == *b"mdhd").collect();
    let [header] = headers.as_slice() else {
        return Err(ConversionError("presentation-track-count-mismatch"));
    };
    let at = match header.header.payload(&movie.bytes).first() {
        Some(0) => 20,
        Some(1) => 32,
        _ => return Err(ConversionError("invalid-output-media")),
    };
    let packed = value
        .bytes()
        .fold(0u16, |n, b| (n << 5) | u16::from(b - 0x60));
    let mut file = crate::native::open_media_update(path)
        .map_err(|_| ConversionError("output-staging-failed"))?;
    file.seek(SeekFrom::Start(
        movie.offset
            + u64::try_from(header.header.offset + header.header.header_size + at)
                .map_err(|_| ConversionError("output-staging-failed"))?,
    ))
    .and_then(|_| file.write_all(&packed.to_be_bytes()))
    .and_then(|()| file.sync_all())
    .map_err(|_| ConversionError("output-staging-failed"))
}

pub(super) fn metadata_pair(
    a: &Prepared,
    b: &Prepared,
    backend: &super::super::backend::Backend,
    keys: &InputKeys,
) -> Result<(), ConversionError> {
    let expected = declared_language(a)?;
    let actual = backend.profile_stream_keys(
        &b.media,
        b.index,
        Some(super::super::keys::ProbeKeys::All(keys)),
    )?;
    if actual["language"].as_str().unwrap_or("und") != expected {
        return Err(ConversionError("presentation-metadata-mismatch"));
    }
    Ok(())
}

pub(super) fn supported_tracks(prepared: &[Prepared]) -> bool {
    !prepared.is_empty()
        && prepared.len() <= 2
        && prepared.iter().all(|p| {
            p.transport_index.is_some()
                && matches!(p.report.tracks[0].kind, TrackKind::Video | TrackKind::Audio)
        })
        && [TrackKind::Video, TrackKind::Audio]
            .into_iter()
            .all(|kind| {
                prepared
                    .iter()
                    .filter(|p| p.report.tracks[0].kind == kind)
                    .count()
                    <= 1
            })
}

/// Restore only selections created from the same complete private MP4 snapshot.
pub(super) fn complete_tracks(prepared: &[Prepared]) -> Option<Vec<super::LocalTrack>> {
    let first = prepared.first()?.complete_track.as_ref()?;
    prepared
        .iter()
        .enumerate()
        .map(|(index, track)| {
            let selected = track.complete_track.as_ref()?;
            (selected.source == first.source
                && selected.index == index
                && selected.kind == track.report.tracks[0].kind
                && track.metadata.origin == super::metadata::Origin::Container
                && track.transport_index.is_none()
                && track.playlist_reference.is_none()
                && !track.dash_reference)
                .then(|| super::LocalTrack {
                    source: selected.source.clone(),
                    index: selected.index,
                    kind: selected.kind,
                })
        })
        .collect()
}

pub(super) fn canonical_tracks(prepared: &[Prepared], format: inspection::InputFormat) -> bool {
    let joint_manifest = joint_manifest_tracks(prepared, format);
    if format != inspection::InputFormat::Mp4
        && !joint_manifest
        && prepared
            .iter()
            .any(|track| track.metadata.audio.is_some() || track.metadata.audio_group.is_some())
    {
        return false;
    }
    !prepared.is_empty()
        && (prepared.len() == 1
            || (format == inspection::InputFormat::Mp4 && prepared.len() == 2)
            || joint_manifest)
        && prepared.iter().all(|track| {
            track.transport_index.is_none()
                && track.index == 0
                && matches!(
                    track.report.tracks[0].kind,
                    TrackKind::Audio | TrackKind::Video
                )
        })
        && prepared
            .iter()
            .filter(|track| track.audio_presentation.is_some())
            .count()
            <= 1
        && (joint_manifest
            || prepared
                .iter()
                .any(|track| track.audio_presentation.is_some()))
        && [TrackKind::Video, TrackKind::Audio]
            .into_iter()
            .all(|kind| {
                prepared
                    .iter()
                    .filter(|track| track.report.tracks[0].kind == kind)
                    .count()
                    <= 1
            })
        && supported_metadata(prepared)
}

fn container_audio_ordinal(audio: &super::metadata::Rendition, tracks: usize) -> Option<usize> {
    (0..tracks).find(|&ordinal| {
        *audio
            == super::metadata::container_rendition(
                ordinal,
                audio.language.clone(),
                audio.channels.clone(),
            )
    })
}

fn sort_container_tracks(prepared: &mut [Prepared], format: inspection::InputFormat) {
    let audio_first = matches!(
        format,
        inspection::InputFormat::Dash | inspection::InputFormat::Hls
    ) && canonical_tracks(prepared, format)
        && prepared
            .iter()
            .find(|track| track.report.tracks[0].kind == TrackKind::Audio)
            .and_then(|track| track.metadata.audio.as_ref())
            .and_then(|audio| container_audio_ordinal(audio, prepared.len()))
            == Some(0);
    prepared.sort_by_key(|track| {
        let kind = track.report.tracks[0].kind;
        if audio_first {
            u8::from(kind != TrackKind::Audio)
        } else {
            super::kind_order(kind)
        }
    });
}

fn joint_manifest_tracks(prepared: &[Prepared], format: inspection::InputFormat) -> bool {
    if !matches!(
        format,
        inspection::InputFormat::Dash | inspection::InputFormat::Hls
    ) || prepared.len() != 2
        || [TrackKind::Video, TrackKind::Audio]
            .into_iter()
            .any(|kind| {
                prepared
                    .iter()
                    .filter(|track| track.report.tracks[0].kind == kind)
                    .count()
                    != 1
            })
    {
        return false;
    }
    let audio = prepared
        .iter()
        .find(|track| track.report.tracks[0].kind == TrackKind::Audio)
        .expect("one audio");
    let expected = audio.metadata.audio.as_ref();
    if expected.is_some_and(|value| container_audio_ordinal(value, prepared.len()).is_none()) {
        return false;
    }
    prepared.iter().all(|track| {
        track
            .metadata
            .audio
            .as_ref()
            .is_none_or(|value| Some(value) == expected)
            && track
                .metadata
                .audio_group
                .as_ref()
                .is_none_or(|group| expected.is_some_and(|audio| &audio.group == group))
    })
}

fn standalone_audio_ordinal(prepared: &[Prepared]) -> Option<usize> {
    if prepared
        .iter()
        .any(|track| track.metadata.origin != super::metadata::Origin::Container)
    {
        return None;
    }
    if prepared.iter().any(|track| track.complete_track.is_some()) {
        return complete_tracks(prepared)?
            .into_iter()
            .find(|track| track.kind == TrackKind::Audio)
            .map(|track| track.index);
    }
    let first = prepared.first()?;
    if prepared
        .iter()
        .enumerate()
        .any(|(index, track)| track.source != first.source || track.index != index)
    {
        return None;
    }
    prepared
        .iter()
        .find(|track| track.report.tracks[0].kind == TrackKind::Audio)
        .map(|track| track.index)
}

pub(super) fn verify_manifest_order(
    output: inspection::OutputFormat,
    source: &[Prepared],
    result: &[Prepared],
) -> Result<(), ConversionError> {
    if !manifest_comparison(output, source, result) {
        return Ok(());
    }
    let expected = source
        .iter()
        .find(|track| track.report.tracks[0].kind == TrackKind::Audio)
        .and_then(|track| track.metadata.audio.as_ref())
        .and_then(|audio| container_audio_ordinal(audio, source.len()));
    if expected.is_some() && standalone_audio_ordinal(result) != expected {
        return Err(ConversionError("presentation-metadata-mismatch"));
    }
    Ok(())
}

pub(super) fn manifest_comparison(
    output: inspection::OutputFormat,
    source: &[Prepared],
    result: &[Prepared],
) -> bool {
    let format = if source
        .iter()
        .all(|track| track.metadata.origin == super::metadata::Origin::Dash)
    {
        inspection::InputFormat::Dash
    } else if source
        .iter()
        .all(|track| track.metadata.origin == super::metadata::Origin::Hls)
    {
        inspection::InputFormat::Hls
    } else {
        return false;
    };
    output == inspection::OutputFormat::Mp4
        && joint_manifest_tracks(source, format)
        && canonical_tracks(source, format)
        && result.iter().all(|track| {
            track.transport_index.is_none()
                && !track.dash_reference
                && track.playlist_reference.is_none()
        })
}

pub(super) fn manifest_languages(
    prepared: &[Prepared],
) -> Result<Vec<Option<String>>, ConversionError> {
    let format = if prepared
        .iter()
        .all(|track| track.metadata.origin == super::metadata::Origin::Dash)
    {
        inspection::InputFormat::Dash
    } else if prepared
        .iter()
        .all(|track| track.metadata.origin == super::metadata::Origin::Hls)
    {
        inspection::InputFormat::Hls
    } else {
        return Ok(vec![None; prepared.len()]);
    };
    if !joint_manifest_tracks(prepared, format) || !canonical_tracks(prepared, format) {
        return Ok(vec![None; prepared.len()]);
    }
    prepared
        .iter()
        .map(|track| {
            track
                .metadata
                .audio
                .as_ref()
                .map(|_| declared_language(track))
                .transpose()
        })
        .collect()
}

impl Converter {
    pub(super) fn standalone_metadata_supported(
        &self,
        prepared: &[Prepared],
        keys: &InputKeys,
    ) -> Result<bool, ConversionError> {
        for source in prepared {
            if source.metadata.audio.is_some() {
                let profile = self.backend.profile_stream_keys(
                    &source.media,
                    source.index,
                    Some(super::super::keys::ProbeKeys::All(keys)),
                )?;
                let physical = language(profile["language"].as_str().unwrap_or("und"))?;
                if physical != "und" && physical != declared_language(source)? {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn transport_mp4_parts(
        &self,
        prepared: &[Prepared],
        request: &ConversionRequest<'_>,
        directory: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>, ConversionError> {
        self.run_tracks(prepared.len(), |index| {
            let source = &prepared[index];
            // Per-track sealing must not walk a sibling's temporary edit workspace.
            let directory = directory.join(format!("track-{index}"));
            super::super::files::create_dir(&directory)?;
            let part = directory.join("track.mp4");
            let clear = directory.join("clear.mp4");
            let media = if source.key_id.is_some() {
                self.backend.package_stream(
                    &source.media,
                    source.index,
                    &clear,
                    Some(request.input_keys),
                    None,
                )?;
                &clear
            } else {
                &source.media
            };
            if request.target == TargetProtection::Clear {
                if source.transport_index.is_none() && source.key_id.is_none() {
                    self.backend
                        .package_stream(media, source.index, &part, None, None)?;
                } else {
                    copy_private(media, &part)?;
                }
            } else {
                self.backend.package_stream(
                    media,
                    0,
                    &part,
                    None,
                    Some((
                        request
                            .output_key
                            .ok_or(ConversionError("output-key-required"))?,
                        request.target,
                    )),
                )?;
            }
            if let Some(range) = source.audio_presentation {
                super::super::audio_presentation::restore(&part, range, &self.backend.stop)?;
            }
            if let Some(audio) = &source.metadata.audio {
                write_language(&part, &audio.language)?;
            }
            Ok(part)
        })
    }

    pub(super) fn convert_transport_mp4(
        &self,
        request: &ConversionRequest<'_>,
        mut prepared: Vec<Prepared>,
        input: Inspection,
        owner: &super::super::files::Stage,
    ) -> Result<ConversionReport, ConversionError> {
        let transport = supported_tracks(&prepared);
        if !transport && !canonical_tracks(&prepared, input.format) {
            return Err(ConversionError(
                "mp4-requires-one-video-and-audio-selection",
            ));
        }
        if !transport && !self.standalone_metadata_supported(&prepared, request.input_keys)? {
            return Err(ConversionError("presentation-selection-not-representable"));
        }
        validate_mp4_output_keys(request)?;
        for source in &prepared {
            if let Some(audio) = &source.metadata.audio {
                language(&audio.language)?;
            }
        }
        let destination = super::destination(request.output)?;
        sort_container_tracks(&mut prepared, input.format);
        let work = workspace()?;
        let parts = self.transport_mp4_parts(&prepared, request, work.path())?;
        let candidate = work.path().join("result.mp4");
        super::super::finite::combine_parts(&parts, &candidate, &self.backend.stop)?;
        let keys = request
            .output_key
            .map(|key| {
                InputKeys::new(vec![super::super::ContentKey::from_hex(
                    &key.0.id_hex(),
                    &key.0.key_hex(),
                )?])
            })
            .transpose()?
            .unwrap_or_default();
        let empty = super::super::HlsInputKeys::default();
        let output_dir = work.path().join("verification");
        super::super::files::create_dir(&output_dir)?;
        let result = prepare_presentation(
            &VerificationSource {
                input: &candidate,
                input_keys: &keys,
                hls_keys: &empty,
            },
            &output_dir,
            &self.backend,
            self.maximum_workers,
            super::SourceValidationPurpose::Verification,
        )?;
        let verification = self.compare_presentations(
            prepared,
            result,
            request.input_keys,
            &keys,
            inspection::OutputFormat::Mp4,
        )?;
        if !verification.passed {
            return Err(ConversionError("round-trip-verification-failed"));
        }
        let actual =
            inspection::inspect(&candidate).map_err(|_| ConversionError("invalid-output-media"))?;
        let expected = match request.target {
            TargetProtection::Clear => Protection::Clear,
            TargetProtection::Cenc => Protection::Cenc,
            TargetProtection::Cbcs => Protection::Cbcs,
            _ => return Err(ConversionError("target-requires-hls-output")),
        };
        if actual.protection != expected
            || actual.tracks.iter().any(|track| {
                request
                    .output_key
                    .is_some_and(|key| track.key_ids != [key.0.id_hex()])
            })
        {
            return Err(ConversionError("output-protection-mismatch"));
        }
        self.backend.checkpoint(&[owner, &work]);
        self.publish(&work, &candidate, &destination)?;
        let preparation = if transport {
            inspection::InputPreparation::MpegTsDemux
        } else {
            match input.format {
                inspection::InputFormat::Mp4 => inspection::InputPreparation::CompleteFile,
                inspection::InputFormat::Dash => inspection::InputPreparation::LocalDashResources,
                inspection::InputFormat::Hls => inspection::InputPreparation::LocalHlsResources,
            }
        };
        Ok(ConversionReport {
            schema_version: 2,
            input,
            output: actual,
            verification,
            processing: Some(
                self.backend
                    .processing(inspection::ProcessingRouteId::FragmentedMp4, preparation),
            ),
        })
    }
}
