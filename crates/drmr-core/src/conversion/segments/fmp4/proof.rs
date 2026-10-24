//! Complete native qualification is required in addition to each retained resource receipt.
use super::{ConversionError, Converter, InputKeys, Presentation, SourceQualification};
use crate::{
    conversion::{
        self, ContentKey, HlsInputKeys, HlsOutputKey, OutputKey, SourceCompatibilityNote,
        SourceProtection, ValidationDepth, VerificationReport, VerificationScope, native,
        presentation::metadata,
    },
    inspection::{Evidence, Inspection, OutputFormat, Protection, TrackKind},
    resources::{Claim, ReservationError},
};

pub(super) fn permit(converter: &Converter) -> Result<crate::resources::Permit, ConversionError> {
    converter
        .backend
        .runner
        .native_permit(
            Claim {
                cpu_threads: 1,
                memory_bytes: 208 * 1024 * 1024,
            },
            &converter.backend.stop,
            converter.backend.timeout,
        )
        .map_err(|error| match error {
            ReservationError::Cancelled => ConversionError("cancelled"),
            ReservationError::Timeout => ConversionError("media-tool-timeout"),
            ReservationError::Insufficient => ConversionError("resource-budget"),
        })
}

pub(super) fn admit(
    converter: &Converter,
    presentation: &Presentation,
    keys: &InputKeys,
) -> Result<Vec<native::SegmentedMedia>, ConversionError> {
    let _scope = converter
        .backend
        .scope(conversion::measurement::Phase::Verification);
    if presentation.parts.is_empty() || presentation.parts.len() > 64 {
        return Err(ConversionError("track-count-limit"));
    }
    let media = converter.run_tracks(presentation.parts.len(), |ordinal| {
        let part = &presentation.parts[ordinal];
        let _permit = permit(converter)?;
        let paths = part
            .resources
            .iter()
            .map(|resource| resource.path.clone())
            .collect::<Vec<_>>();
        let native =
            converter
                .backend
                .native_segmented_media(&part.initialization, &paths, keys)?;
        if native.media.tracks.len() != 1 {
            return Err(ConversionError("hls-multiplexed-media-not-supported"));
        }
        let descriptor = &native.media.tracks[0].descriptor;
        if part.role.is_some_and(|kind| kind != descriptor.kind)
            || (part.metadata.audio.is_some() && descriptor.kind != TrackKind::Audio)
        {
            return Err(ConversionError("track-role-mismatch"));
        }
        metadata::validate_profile_value(&part.metadata, &native::mp4_profile(descriptor)?)?;
        metadata::pair(&part.metadata, &part.metadata)?;
        Ok(native)
    })?;
    if media
        .iter()
        .filter(|part| part.media.tracks[0].descriptor.kind == TrackKind::Audio)
        .count()
        > 1
    {
        return Err(ConversionError(
            "hls-multiple-audio-renditions-not-supported",
        ));
    }
    Ok(media)
}

fn protection(part: &super::source::Part, media: &native::SegmentedMedia) -> SourceProtection {
    match part.outer {
        Protection::HlsAes128 => SourceProtection::HlsAes128,
        Protection::Mixed => SourceProtection::Mixed,
        _ => match media.media.tracks[0].descriptor.protection.scheme {
            conversion::native_media::Scheme::Clear => SourceProtection::Clear,
            conversion::native_media::Scheme::Cenc => SourceProtection::Cenc,
            conversion::native_media::Scheme::Cbcs => SourceProtection::Cbcs,
        },
    }
}

pub(super) fn qualify(
    converter: &Converter,
    presentation: &Presentation,
    keys: &InputKeys,
) -> Result<SourceQualification, ConversionError> {
    let media = admit(converter, presentation, keys)?;
    let reports = media
        .iter()
        .map(|part| converter.backend.native_admit_segmented(part))
        .collect::<Result<Vec<_>, _>>()?;
    let verification = VerificationReport::combine(
        ValidationDepth::Native,
        VerificationScope::SourceAdmission,
        reports,
    );
    if !verification.passed {
        return Err(ConversionError("source-media-verification-failed"));
    }
    Ok(SourceQualification {
        input: presentation.report.clone(),
        actual_format: presentation.format,
        track_protection: presentation
            .parts
            .iter()
            .zip(&media)
            .map(|(part, native)| protection(part, native))
            .collect(),
        compatibility_notes: if presentation.parts.iter().any(|part| part.empty_cue_pair) {
            vec![SourceCompatibilityNote::HlsEmptyCuePair]
        } else {
            Vec::new()
        },
        eligible_formats: eligible_formats(converter, presentation, &media)?,
        verification,
    })
}

fn eligible_formats(
    converter: &Converter,
    presentation: &Presentation,
    media: &[native::SegmentedMedia],
) -> Result<Vec<OutputFormat>, ConversionError> {
    let mut formats = Vec::new();
    if let [part] = presentation.parts.as_slice()
        && part.metadata.audio.is_none()
        && part.metadata.audio_group.is_none()
    {
        let _permit = permit(converter)?;
        // Qualification describes all tool routes. A playback claim and explicit
        // audio selection still require a presentation output, as in stitch mode.
        if conversion::audio_presentation::read_claim(&part.initialization)?.is_none() {
            formats.push(OutputFormat::Mp4);
        }
    }
    if presentation
        .parts
        .iter()
        .filter(|part| part.metadata.audio.is_some())
        .count()
        <= 1
    {
        formats.push(OutputFormat::Dash);
    }
    if media
        .iter()
        .flat_map(|part| &part.media.tracks)
        .filter(|track| track.descriptor.kind == TrackKind::Audio)
        .count()
        <= 1
    {
        formats.push(OutputFormat::Hls);
    }
    Ok(formats)
}

pub(super) fn inspection(
    presentation: &Presentation,
    media: &[native::SegmentedMedia],
) -> Result<Inspection, ConversionError> {
    if media.len() != presentation.parts.len() {
        return Err(ConversionError("presentation-track-count-mismatch"));
    }
    let tracks = presentation
        .parts
        .iter()
        .map(|part| {
            let actual = crate::inspection::inspect(&part.initialization)
                .map_err(|_| ConversionError("invalid-output-media"))?;
            if actual.tracks.len() != 1 {
                return Err(ConversionError("presentation-track-count-mismatch"));
            }
            let mut track = actual
                .tracks
                .into_iter()
                .next()
                .ok_or(ConversionError("track-count-limit"))?;
            if part.outer == Protection::HlsAes128 {
                track.protection = Protection::HlsAes128;
            }
            Ok(track)
        })
        .collect::<Result<_, ConversionError>>()?;
    Ok(Inspection::new(
        presentation.report.format,
        Evidence::ContainerMetadata,
        tracks,
        presentation.report.drm_systems.clone(),
        Vec::new(),
    ))
}

pub(super) fn compare(
    converter: &Converter,
    first: &Presentation,
    sources: &[native::SegmentedMedia],
    second: &Presentation,
    outputs: &[native::SegmentedMedia],
) -> Result<VerificationReport, ConversionError> {
    if sources.len() != outputs.len()
        || sources.len() != first.parts.len()
        || outputs.len() != second.parts.len()
    {
        return Err(ConversionError("presentation-track-count-mismatch"));
    }
    let mut used = vec![false; outputs.len()];
    let mut reports = Vec::with_capacity(sources.len());
    for (ordinal, source) in sources.iter().enumerate() {
        let a = &source.media.tracks[0];
        let mut selected = None;
        for (candidate, output) in outputs.iter().enumerate() {
            let b = &output.media.tracks[0];
            if used[candidate]
                || a.descriptor.kind != b.descriptor.kind
                || a.samples.raw_payload != b.samples.raw_payload
            {
                continue;
            }
            if metadata::pair(
                &first.parts[ordinal].metadata,
                &second.parts[candidate].metadata,
            )
            .is_err()
            {
                continue;
            }
            let mut report = converter.backend.native_verify_segmented(source, output)?;
            report.checks.packet_timing_equal &=
                super::source::windows_equal(&first.parts[ordinal], &second.parts[candidate]);
            report.refresh_passed();
            let passed = report.passed;
            if selected.is_none() || passed {
                selected = Some((candidate, report));
            }
            if passed {
                break;
            }
        }
        let (candidate, report) =
            selected.ok_or(ConversionError("presentation-track-content-mismatch"))?;
        used[candidate] = true;
        reports.push(report);
    }
    Ok(VerificationReport::combine(
        ValidationDepth::Native,
        VerificationScope::Comparison,
        reports,
    ))
}

pub(super) fn output_keys(key: Option<&OutputKey>) -> Result<InputKeys, ConversionError> {
    key.map(|key| {
        InputKeys::new(vec![ContentKey {
            id: key.0.id,
            bytes: key.0.bytes.clone(),
        }])
    })
    .transpose()
    .map(Option::unwrap_or_default)
}

pub(super) fn bind_output(
    presentation: &Presentation,
    media: &[native::SegmentedMedia],
    request: &conversion::ConversionRequest<'_>,
) -> Result<(), ConversionError> {
    if presentation.format != request.format {
        return Err(ConversionError("output-protection-mismatch"));
    }
    for (part, media) in presentation.parts.iter().zip(media) {
        if !protection(part, media).matches_target(request.target) {
            return Err(ConversionError("output-protection-mismatch"));
        }
        if let Some(key) = request.output_key {
            let descriptor = &media.media.tracks[0].descriptor;
            if descriptor.protection.key_id != Some(key.0.id) {
                return Err(ConversionError("output-key-id-mismatch"));
            }
        }
    }
    Ok(())
}
pub(super) fn output_hls_keys(key: Option<&HlsOutputKey>) -> Result<HlsInputKeys, ConversionError> {
    key.map(|key| {
        HlsInputKeys::new(vec![(
            key.key_uri().to_owned(),
            conversion::Aes128Key(key.key.0.clone()),
        )])
    })
    .transpose()
    .map(Option::unwrap_or_default)
}
