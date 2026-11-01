//! Native checks compare independently read original and output samples.
use super::super::{
    CheckOutcome, ConversionError, DecodeEvidence, VerificationChecks, VerificationReport,
    VerificationScope, backend::Backend, ts,
};
use super::{Track, samples::Ratio};
use std::path::Path;

fn entry_equal(a: &Track, b: &Track) -> Result<bool, ConversionError> {
    let x = &a.descriptor.sample_entry_metadata;
    let y = &b.descriptor.sample_entry_metadata;
    let aac = a.descriptor.decoder_config_kind == *b"esds"
        && b.descriptor.decoder_config_kind == *b"esds";
    let codec = a.descriptor.codec == b.descriptor.codec
        || matches!(
            (&a.descriptor.codec, &b.descriptor.codec),
            (b"avc1", b"avc3") | (b"avc3", b"avc1") | (b"hvc1", b"hev1") | (b"hev1", b"hvc1")
        );
    let (configuration, progressive_hevc) = if a.descriptor.decoder_config_kind == *b"hvcC"
        && b.descriptor.decoder_config_kind == *b"hvcC"
    {
        let (left, _) = super::super::hevc::Configuration::from_hvcc(&a.descriptor.decoder_config)?;
        let (right, _) =
            super::super::hevc::Configuration::from_hvcc(&b.descriptor.decoder_config)?;
        left.validate_entry(a.descriptor.codec)?;
        right.validate_entry(b.descriptor.codec)?;
        (
            left.semantic_eq(&right),
            left.metadata().progressive && right.metadata().progressive,
        )
    } else {
        let equal = match (&a.config, &b.config) {
            (Some(x), Some(y)) => x == y,
            _ => {
                a.descriptor.decoder_config_kind == b.descriptor.decoder_config_kind
                    && a.descriptor.decoder_config == b.descriptor.decoder_config
            }
        };
        (equal, false)
    };
    Ok(a.descriptor.kind == b.descriptor.kind
        && codec
        && configuration
        && x.width == y.width
        && x.height == y.height
        && (aac || (x.channels == y.channels && x.sample_rate == y.sample_rate))
        && x.sample_size == y.sample_size
        && x.language.as_deref().unwrap_or("und") == y.language.as_deref().unwrap_or("und")
        && x.display_width == y.display_width
        && x.display_height == y.display_height
        && x.volume == y.volume
        && x.layer == y.layer
        && a.descriptor.media_header == b.descriptor.media_header
        && x.track_flags & !4 == y.track_flags & !4
        && (x.alternate_group == y.alternate_group
            || (a.descriptor.kind == crate::inspection::TrackKind::Audio
                && matches!(x.alternate_group, 0 | 1)
                && matches!(y.alternate_group, 0 | 1)))
        && super::semantics::aspect_ratio(&a.descriptor)?
            == super::semantics::aspect_ratio(&b.descriptor)?
        && semantic_boxes_equal(&x.boxes, &y.boxes, progressive_hevc))
}

fn semantic_boxes_equal(
    a: &[super::super::native_media::SemanticBox],
    b: &[super::super::native_media::SemanticBox],
    progressive_hevc: bool,
) -> bool {
    // A one-frame progressive declaration is equivalent to its absence only
    // when both independently parsed HEVC configurations establish progressive pictures.
    let redundant = |node: &&super::super::native_media::SemanticBox| {
        node.kind == *b"pasp" || progressive_hevc && node.kind == *b"fiel" && node.payload == [1, 0]
    };
    let mut a: Vec<_> = a
        .iter()
        .filter(|node| !redundant(node))
        .map(|node| (&node.kind, node.payload.as_slice()))
        .collect();
    let mut b: Vec<_> = b
        .iter()
        .filter(|node| !redundant(node))
        .map(|node| (&node.kind, node.payload.as_slice()))
        .collect();
    a.sort_unstable();
    b.sort_unstable();
    a == b
}

fn report(metadata: bool, payload: bool, timing: bool) -> VerificationReport {
    VerificationReport::new(
        super::super::ValidationDepth::Native,
        VerificationScope::Comparison,
        VerificationChecks {
            stream_metadata: if metadata {
                CheckOutcome::Pass
            } else {
                CheckOutcome::Fail
            },
            packet_payloads_equal: payload,
            packet_timing_equal: timing,
        },
        DecodeEvidence::not_requested(),
    )
}

fn checked_window(
    track: &Track,
    fragmented: bool,
    window: Option<super::super::finite::Window>,
) -> Result<Option<super::super::finite::Window>, ConversionError> {
    if let Some(window) = window {
        let end = window
            .start
            .checked_add(window.duration)
            .ok_or(ConversionError("presentation-window-not-supported"))?;
        let end = Ratio::new(i128::from(end), u64::from(window.scale))?;
        let start = Ratio::new(i128::from(window.start), u64::from(window.scale))?;
        if end.compare(track.samples.end_pts)? == std::cmp::Ordering::Greater
            || end.compare(track.samples.first_pts)? != std::cmp::Ordering::Greater
            || start.compare(track.samples.end_pts)? != std::cmp::Ordering::Less
        {
            return Err(ConversionError("presentation-window-outside-media"));
        }
    } else if !track.descriptor.edits.is_empty() {
        // A fragmented whole-track edit maps media clocks to presentation
        // clocks without declaring a finite end. Retain that exact mapping;
        // comparison must not interpret an unbounded edit as an identity edit.
        let [edit] = track.descriptor.edits.as_slice() else {
            return Err(ConversionError("native-presentation-window-not-supported"));
        };
        if edit.duration != 0
            || edit.media_time < 0
            || (edit.media_time != 0 && !fragmented)
            || track
                .samples
                .presentation_offset
                .compare(track.samples.end_pts)?
                != std::cmp::Ordering::Less
        {
            return Err(ConversionError("native-presentation-window-not-supported"));
        }
    }
    Ok(window)
}

fn fragmented_window(
    movie: &super::media::Media,
    track: &Track,
) -> Result<Option<super::super::finite::Window>, ConversionError> {
    let descriptor = &track.descriptor;
    let candidate = match descriptor.edits.as_slice() {
        [] => None,
        [edit] if edit.media_time < 0 || edit.rate_integer != 1 || edit.rate_fraction != 0 => {
            return Err(ConversionError("presentation-window-not-supported"));
        }
        [edit] if edit.duration == 0 => None,
        [edit] => {
            let ticks = u128::from(edit.duration)
                .checked_mul(u128::from(descriptor.timescale))
                .ok_or(ConversionError("presentation-window-not-supported"))?;
            let scale = movie.movie.timescale;
            if scale == 0 || !ticks.is_multiple_of(u128::from(scale)) {
                return Err(ConversionError("presentation-window-not-supported"));
            }
            Some(super::super::finite::Window {
                start: u64::try_from(edit.media_time)
                    .map_err(|_| ConversionError("presentation-window-not-supported"))?,
                duration: u64::try_from(ticks / u128::from(scale))
                    .map_err(|_| ConversionError("presentation-window-not-supported"))?,
                scale: descriptor.timescale,
                audio: descriptor.kind == crate::inspection::TrackKind::Audio,
            })
        }
        _ => return Err(ConversionError("native-presentation-window-not-supported")),
    };
    checked_window(track, movie.movie.fragmented, candidate)
}

fn admitted_controls(media: &super::media::Media, track: &Track) -> Result<(), ConversionError> {
    let entry = &track.descriptor.sample_entry_metadata;
    let audio = track.descriptor.kind == crate::inspection::TrackKind::Audio;
    if media.movie.rate != 1 << 16
        || media.movie.volume != 0x100
        || !matches!(entry.track_flags, 3 | 7)
        || !(entry.alternate_group == 0 || (audio && entry.alternate_group == 1))
        || entry.volume != if audio { 0x100 } else { 0 }
    {
        return Err(ConversionError("presentation-window-not-supported"));
    }
    Ok(())
}

fn boundaries_equal(a: &super::SegmentedMedia, b: &super::SegmentedMedia) -> bool {
    a.boundaries.len() == b.boundaries.len()
        && a.boundaries.iter().zip(&b.boundaries).all(|(a, b)| {
            a.fragment_count == b.fragment_count
                && a.tracks.len() == b.tracks.len()
                && a.tracks.iter().zip(&b.tracks).all(|(a, b)| {
                    let mut a = a.clone();
                    let mut b = b.clone();
                    // Packet content is a separate outcome from the exact
                    // resource-local clocks, sample grouping and boundaries.
                    a.payload = [0; 32];
                    b.payload = [0; 32];
                    a == b
                })
        })
}

fn compare_tracks(
    first: &super::media::Media,
    a: &Track,
    source_window: Option<super::super::finite::Window>,
    second: &super::media::Media,
    b: &Track,
    result_window: Option<super::super::finite::Window>,
) -> Result<VerificationReport, ConversionError> {
    let windows_equal = match (source_window, result_window) {
        (Some(a), Some(b)) => a.equivalent(b),
        (None, None) => true,
        _ => false,
    };
    let matrices_equal = if a.descriptor.kind == crate::inspection::TrackKind::Video {
        super::semantics::display_matrix(
            &first.movie.matrix,
            &a.descriptor.sample_entry_metadata.display_matrix,
        )? == super::semantics::display_matrix(
            &second.movie.matrix,
            &b.descriptor.sample_entry_metadata.display_matrix,
        )?
    } else {
        // A movie's coordinate transform does not affect sound. Preserve
        // the audio track's own raw matrix rather than applying that transform.
        a.descriptor.sample_entry_metadata.display_matrix
            == b.descriptor.sample_entry_metadata.display_matrix
    };
    let metadata = entry_equal(a, b)?
        && first.movie.rate == second.movie.rate
        && first.movie.volume == second.movie.volume
        && matrices_equal;
    let payload = a.samples.count == b.samples.count
        && match (a.samples.canonical_payload, b.samples.canonical_payload) {
            (Some(x), Some(y)) => x == y,
            _ => a.samples.raw_payload == b.samples.raw_payload,
        };
    let timing = a.samples.count == b.samples.count
        && a.samples.clocks_equal(&b.samples)
        && a.samples.durations == b.samples.durations
        && a.samples.sync_samples == b.samples.sync_samples
        && a.samples.recovery == b.samples.recovery
        && a.samples.presented_first_pts == b.samples.presented_first_pts
        && a.samples.presented_end_pts == b.samples.presented_end_pts
        && windows_equal;
    Ok(report(metadata, payload, timing))
}

impl Backend {
    pub(in crate::conversion) fn native_verify_audio_projection(
        &self,
        source: super::super::verify::StreamView<'_>,
        result: super::super::verify::StreamView<'_>,
        expected: super::super::audio_presentation::AudioPresentation,
    ) -> Result<VerificationReport, ConversionError> {
        let projection = super::audio_projection::AudioProjection::new(expected)?;
        let first = self.native_media_projected(
            source.metadata,
            source.key,
            source.clock,
            Some(&projection),
        )?;
        let second = self.native_media_with_clock(result.metadata, result.key, result.clock)?;
        let a = first
            .tracks
            .get(source.index)
            .ok_or(ConversionError("stream-selector-mismatch"))?;
        let b = second
            .tracks
            .get(result.index)
            .ok_or(ConversionError("stream-selector-mismatch"))?;
        let result_window = checked_window(
            b,
            second.movie.fragmented,
            super::super::finite::window(result.metadata, result.index)?,
        )?;
        compare_tracks(&first, a, None, &second, b, result_window)
    }
    pub(in crate::conversion) fn native_admit_segmented(
        &self,
        media: &super::SegmentedMedia,
    ) -> Result<VerificationReport, ConversionError> {
        self.check_cancelled()?;
        if !media.media.movie.fragmented
            || media.media.tracks.is_empty()
            || media.boundaries.is_empty()
        {
            return Err(ConversionError("native-sample-evidence-incomplete"));
        }
        for track in &media.media.tracks {
            super::mp4_profile(&track.descriptor)?;
            fragmented_window(&media.media, track)?;
            admitted_controls(&media.media, track)?;
        }
        if let Some(presentation) = media.audio_presentation {
            let [track] = media.media.tracks.as_slice() else {
                return Err(super::super::audio_presentation::MISMATCH);
            };
            if presentation
                .claim()
                .bind(super::media::audio_evidence(track)?)?
                != presentation
            {
                return Err(super::super::audio_presentation::MISMATCH);
            }
        }
        let mut result = report(true, true, true);
        result.scope = VerificationScope::SourceAdmission;
        result.refresh_passed();
        Ok(result)
    }

    pub(in crate::conversion) fn native_verify_segmented(
        &self,
        source: &super::SegmentedMedia,
        output: &super::SegmentedMedia,
    ) -> Result<VerificationReport, ConversionError> {
        self.check_cancelled()?;
        let a = &source.media;
        let b = &output.media;
        if a.tracks.len() != b.tracks.len() || a.tracks.is_empty() {
            return Ok(report(false, false, false));
        }
        let mut metadata = true;
        let mut payload = true;
        let mut timing = boundaries_equal(source, output)
            && source.audio_presentation == output.audio_presentation;
        for (first, second) in a.tracks.iter().zip(&b.tracks) {
            let compared = compare_tracks(
                a,
                first,
                fragmented_window(a, first)?,
                b,
                second,
                fragmented_window(b, second)?,
            )?;
            metadata &= compared.checks.stream_metadata.is_pass();
            payload &= compared.checks.packet_payloads_equal
                && first.samples.raw_payload == second.samples.raw_payload;
            timing &= compared.checks.packet_timing_equal;
        }
        self.check_cancelled()?;
        Ok(report(metadata, payload, timing))
    }

    pub(in crate::conversion) fn native_verify_views(
        &self,
        source: super::super::verify::StreamView<'_>,
        result: super::super::verify::StreamView<'_>,
    ) -> Result<VerificationReport, ConversionError> {
        let first = self.native_media_with_clock(source.metadata, source.key, source.clock)?;
        let second = self.native_media_with_clock(result.metadata, result.key, result.clock)?;
        let a = first
            .tracks
            .get(source.index)
            .ok_or(ConversionError("stream-selector-mismatch"))?;
        let b = second
            .tracks
            .get(result.index)
            .ok_or(ConversionError("stream-selector-mismatch"))?;
        let source_window = checked_window(
            a,
            first.movie.fragmented,
            super::super::finite::window(source.metadata, source.index)?,
        )?;
        let result_window = checked_window(
            b,
            second.movie.fragmented,
            super::super::finite::window(result.metadata, result.index)?,
        )?;
        let mut result_report =
            compare_tracks(&first, a, source_window, &second, b, result_window)?;
        result_report.checks.packet_timing_equal &=
            super::super::progressive::presentation_windows_equal(
                source.metadata,
                result.metadata,
            )?;
        result_report.refresh_passed();
        Ok(result_report)
    }
    pub(in crate::conversion) fn native_validate_mp4(
        &self,
        input: &Path,
        index: usize,
        keys: Option<super::super::keys::ProbeKeys<'_>>,
    ) -> Result<VerificationReport, ConversionError> {
        self.native_validate_mp4_with_clock(input, index, keys, None)
    }
    pub(in crate::conversion) fn native_validate_mp4_with_clock(
        &self,
        input: &Path,
        index: usize,
        keys: Option<super::super::keys::ProbeKeys<'_>>,
        clock: Option<super::ManifestAacClock>,
    ) -> Result<VerificationReport, ConversionError> {
        let media = match clock {
            Some(clock) => self.native_media_with_clock(input, keys, Some(clock))?,
            None => self.native_media(input, keys)?,
        };
        let track = media
            .tracks
            .get(index)
            .ok_or(ConversionError("stream-selector-mismatch"))?;
        super::mp4_profile(&track.descriptor)?;
        // Playback edits and controls are independently read, including the
        // declared terminal audio selection. No comparison with a remux is used.
        checked_window(
            track,
            media.movie.fragmented,
            super::super::finite::window(input, index)?,
        )?;
        super::super::progressive::playback_controls_equal(input, index, input, index)?;
        let mut report = report(true, true, true);
        report.scope = VerificationScope::SourceAdmission;
        report.refresh_passed();
        Ok(report)
    }
    pub(in crate::conversion) fn native_verify_transport(
        &self,
        source: ts::View<'_>,
        result: ts::View<'_>,
    ) -> Result<VerificationReport, ConversionError> {
        self.native_verify_transport_with_clock(source, result, None, None)
    }
    pub(in crate::conversion) fn native_verify_transport_with_clock(
        &self,
        source: ts::View<'_>,
        result: ts::View<'_>,
        source_clock: Option<super::ManifestAacClock>,
        result_clock: Option<super::ManifestAacClock>,
    ) -> Result<VerificationReport, ConversionError> {
        match (source.transport, result.transport) {
            (false, false) => self.native_verify_views(
                super::super::verify::StreamView {
                    input: source.input,
                    metadata: source.metadata,
                    index: source.index,
                    key: source.probe_keys(),
                    clock: source_clock,
                },
                super::super::verify::StreamView {
                    input: result.input,
                    metadata: result.metadata,
                    index: result.index,
                    key: result.probe_keys(),
                    clock: result_clock,
                },
            ),
            (true, true) => {
                let a = source
                    .native
                    .ok_or(ConversionError("ts-original-evidence-required"))?;
                let b = result
                    .native
                    .ok_or(ConversionError("ts-original-evidence-required"))?;
                a.validate()?;
                b.validate()?;
                let metadata = a.metadata()? == b.metadata()? && a.config == b.config;
                let first = a.evidence()?;
                let second = b.evidence()?;
                Ok(report(
                    metadata,
                    first.payload_equal(&second),
                    first.timing_equal(&second),
                ))
            }
            (true, false) => self.native_transport_to_mp4(source, result),
            (false, true) => self.native_transport_to_mp4(result, source),
        }
    }
    fn native_transport_to_mp4(
        &self,
        source: ts::View<'_>,
        result: ts::View<'_>,
    ) -> Result<VerificationReport, ConversionError> {
        let original = source
            .native
            .ok_or(ConversionError("ts-original-evidence-required"))?;
        original.validate()?;
        let original_metadata = original.metadata()?;
        let evidence = original.evidence()?;
        let timing = original.timing()?;
        let media = self.native_media_with_clock(
            result.metadata,
            result.probe_keys(),
            super::ManifestAacClock::from_transport(original)?,
        )?;
        let track = media
            .tracks
            .get(result.index)
            .ok_or(ConversionError("stream-selector-mismatch"))?;
        let metadata = track.samples.transport_recovery_compatible
            && super::semantics::transport_controls(&media.movie, &track.descriptor, original)?
            && track.config.as_ref() == Some(&original.config)
            && match original_metadata {
                ts::canonical::TrackMetadata::Video(metadata) => {
                    track.descriptor.decoder_config_kind == *b"avcC"
                        && ts::avc::Configuration::from_avcc(&track.descriptor.decoder_config)?
                            .0
                            .metadata()
                            == &metadata
                }
                ts::canonical::TrackMetadata::Audio {
                    sample_rate,
                    channels,
                    ..
                } => {
                    let config = super::super::native_media::AacConfiguration::parse(
                        &track.descriptor.decoder_config,
                    )?;
                    config.sample_rate == sample_rate && config.channels == u16::from(channels)
                }
            };
        let payload = track.samples.count == evidence.count
            && track.samples.canonical_payload == Some(evidence.payload_digest());
        let window = checked_window(
            track,
            media.movie.fragmented,
            super::super::finite::window(result.metadata, result.index)?,
        )?;
        let full_selection = match window {
            Some(window) => {
                Ratio::new(i128::from(window.start), u64::from(window.scale))?
                    .subtract(track.samples.presentation_offset)?
                    == Ratio::ticks(timing.first_pts, 90_000)?
                    && Ratio::new(i128::from(window.duration), u64::from(window.scale))?
                        == Ratio::new(
                            i128::from(timing.end_pts) - i128::from(timing.first_pts),
                            90_000,
                        )?
            }
            None => true,
        };
        let composition_matches = track.samples.initial_composition_offset
            == Ratio::new(
                i128::from(timing.first_sample_pts) - i128::from(timing.first_dts),
                90_000,
            )?;
        let clocks = full_selection
            && composition_matches
            && track.samples.transport_clocks == Some(evidence.clock_digest())
            && track.samples.count == evidence.count
            && track.samples.presented_first_pts == Ratio::ticks(timing.first_pts, 90_000)?
            && track.samples.presented_end_pts == Ratio::ticks(timing.end_pts, 90_000)?
            && track.samples.continuous_decode;
        Ok(report(metadata, payload, clocks))
    }
}

#[cfg(test)]
mod hevc_metadata_tests {
    use super::super::super::native_media::SemanticBox;
    use super::*;

    #[test]
    fn hevc_field_box_equivalence_requires_progressive_evidence_and_exact_neutral_value() {
        let neutral = SemanticBox {
            kind: *b"fiel",
            payload: vec![1, 0],
        };
        assert!(semantic_boxes_equal(
            std::slice::from_ref(&neutral),
            &[],
            true
        ));
        assert!(!semantic_boxes_equal(
            std::slice::from_ref(&neutral),
            &[],
            false
        ));
        for value in [vec![2, 1], vec![1, 1], vec![1, 0, 0]] {
            assert!(!semantic_boxes_equal(
                &[SemanticBox {
                    kind: *b"fiel",
                    payload: value
                }],
                &[],
                true,
            ));
        }
        let colour = SemanticBox {
            kind: *b"colr",
            payload: vec![1, 2, 3],
        };
        assert!(!semantic_boxes_equal(&[neutral, colour], &[], true));
    }
}
