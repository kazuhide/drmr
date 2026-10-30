//! Standalone MP4 from qualified transport streams retains the complete selected tracks.
use super::{
    ConversionError, ConversionReport, ConversionRequest, Converter, InputKeys, Inspection,
    Prepared, TrackKind, VerificationSource, copy_private, inspection, prepare_presentation,
    workspace,
};
use crate::inspection::{Protection, TargetProtection};
use std::io::{Seek as _, SeekFrom, Write as _};

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

fn language(value: &str) -> Result<String, ConversionError> {
    let value = value.to_ascii_lowercase();
    Ok(match value.as_str() {
        "en" => "eng",
        "ja" => "jpn",
        "fr" => "fra",
        "de" => "deu",
        "es" => "spa",
        "it" => "ita",
        "pt" => "por",
        "zh" => "zho",
        "ko" => "kor",
        "ru" => "rus",
        "ar" => "ara",
        "nl" => "nld",
        value if value.len() == 3 && value.bytes().all(|b| b.is_ascii_lowercase()) => value,
        _ => return Err(ConversionError("mp4-audio-language-not-supported")),
    }
    .to_owned())
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

impl Converter {
    fn transport_mp4_parts(
        &self,
        prepared: &[Prepared],
        request: &ConversionRequest<'_>,
        directory: &std::path::Path,
    ) -> Result<Vec<std::path::PathBuf>, ConversionError> {
        self.run_tracks(prepared.len(), |index| {
            let source = &prepared[index];
            let part = directory.join(format!("track-{index}.mp4"));
            if request.target == TargetProtection::Clear {
                copy_private(&source.media, &part)?;
            } else {
                self.backend.package_stream(
                    &source.media,
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
            if source.report.tracks[0].kind == TrackKind::Audio {
                write_language(
                    &part,
                    source
                        .metadata
                        .audio
                        .as_ref()
                        .map_or("und", |audio| audio.language.as_str()),
                )?;
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
        if !supported_tracks(&prepared) {
            return Err(ConversionError(
                "mp4-requires-one-video-and-audio-selection",
            ));
        }
        if matches!(
            request.target,
            TargetProtection::Cenc | TargetProtection::Cbcs
        ) != request.output_key.is_some()
            || request.hls_output_key.is_some()
            || request.hls_key_uri.is_some()
        {
            return Err(ConversionError("output-key-role-mismatch"));
        }
        for source in &prepared {
            if let Some(audio) = &source.metadata.audio {
                language(&audio.language)?;
            }
        }
        let destination = super::destination(request.output)?;
        prepared.sort_by_key(|p| super::kind_order(p.report.tracks[0].kind));
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
        let verification =
            self.compare_presentations(prepared, result, request.input_keys, &keys)?;
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
        Ok(ConversionReport {
            schema_version: 2,
            input,
            output: actual,
            verification,
            processing: Some(self.backend.processing(
                inspection::ProcessingRouteId::FragmentedMp4,
                inspection::InputPreparation::MpegTsDemux,
            )),
        })
    }
}
