//! Native transport conversion keeps each original resource and its sample membership.
use super::super::{
    ConversionError, ConversionReport, ConversionRequest, Converter, InputKeys, ProcessingMode,
    SourceCompatibilityNote, SourceProtection, SourceQualification, SourceValidationPurpose,
    ValidationDepth, VerificationReport, VerificationRequest, VerificationSource, aes128,
    audio_presentation::AudioPresentation, files, measurement::Phase, native,
    presentation::metadata, ts,
};
use super::{
    manifest_hls::{self, HlsPart, HlsResource},
    source::{HlsSource, Part},
};
use crate::{
    inspection::{self, OutputFormat, Protection, TargetProtection, TrackKind},
    resources::{Claim, ReservationError},
};
use std::{
    io::{Read as _, Write as _},
    path::Path,
};
use zeroize::Zeroizing;

const ORIGINAL: ConversionError = ConversionError("ts-original-evidence-required");
const LAYOUT: ConversionError = ConversionError("ts-presentation-layout-not-supported");

struct Admitted<'a> {
    part: &'a Part,
    tracks: Vec<(usize, TrackKind, &'a ts::NativeEvidence)>,
    window: Option<AudioPresentation>,
}

fn admitted<'a>(
    converter: &Converter,
    source: &'a HlsSource,
) -> Result<Vec<Admitted<'a>>, ConversionError> {
    converter.backend.check_cancelled()?;
    if source.format != OutputFormat::HlsTs || source.parts.is_empty() || source.parts.len() > 64 {
        return Err(ConversionError("processing-segments-source-required"));
    }
    let native_converter = converter
        .with_processing_clone(ProcessingMode::Segments)
        .with_decode(false);
    source
        .parts
        .iter()
        .map(|part| {
            converter.backend.check_cancelled()?;
            let admission = part.media.transport.as_ref().ok_or(ORIGINAL)?;
            if part.media.initialization.is_some()
                || part.media.resources.is_empty()
                || (source.parts.len() != 1 && admission.tracks.len() != 1)
                || part.declaration.role.is_some_and(|kind| {
                    admission.tracks.len() != 1 || admission.tracks[0].kind != kind
                })
            {
                return Err(LAYOUT);
            }
            let streams = native_converter.backend.transport_streams(
                &part.media.resources[0].0,
                admission,
                &part.media.native,
            )?;
            let mut tracks = Vec::with_capacity(streams.len());
            for (index, kind) in streams {
                let pid = admission.tracks.get(index).ok_or(ORIGINAL)?.pid;
                let original = part.media.native.get(&pid).ok_or(ORIGINAL)?;
                let mut declared = part.declaration.metadata.clone();
                match kind {
                    TrackKind::Video => declared.audio = None,
                    TrackKind::Audio => declared.resolution = None,
                    _ => return Err(LAYOUT),
                }
                metadata::validate_profile_value(&declared, &native::transport_profile(original)?)?;
                tracks.push((index, kind, original.as_ref()));
            }
            let clock = tracks
                .iter()
                .find(|(_, kind, _)| *kind == TrackKind::Video)
                .or_else(|| tracks.first())
                .ok_or(ORIGINAL)?
                .2;
            let window = native::durations::transport(clock, &part.media.resources)?;
            Ok(Admitted {
                part,
                tracks,
                window,
            })
        })
        .collect()
}

fn protection(part: &Part) -> SourceProtection {
    match (part.media.sample_transport, part.media.report.protection) {
        (true, _) => SourceProtection::HlsSampleAes,
        (_, Protection::HlsAes128) => SourceProtection::HlsAes128,
        (_, Protection::Mixed) => SourceProtection::Mixed,
        _ => SourceProtection::Clear,
    }
}

pub(super) fn qualify(
    converter: &Converter,
    source: &HlsSource,
    input: VerificationSource<'_>,
    purpose: SourceValidationPurpose,
) -> Result<SourceQualification, ConversionError> {
    let _scope = converter.backend.scope(Phase::Verification);
    source.check_original()?;
    let admitted = admitted(converter, source)?;
    let native_converter = converter
        .with_processing_clone(ProcessingMode::Segments)
        .with_decode(false);
    let mut reports = Vec::new();
    let mut track_protection = Vec::new();
    let mut compatibility = std::collections::BTreeSet::new();
    for admission in &admitted {
        metadata::pair(
            &admission.part.declaration.metadata,
            &admission.part.declaration.metadata,
        )?;
        for (index, _, original) in &admission.tracks {
            reports.push(native_converter.backend.validate_transport_source(
                &admission.part.media.resources[0].0,
                *index,
                original,
            )?);
            track_protection.push(protection(admission.part));
        }
        if admission.part.media.empty_cue_pair {
            compatibility.insert(SourceCompatibilityNote::HlsEmptyCuePair);
        }
        if admission
            .part
            .media
            .transport
            .as_ref()
            .ok_or(ORIGINAL)?
            .has_empty_payload_padding()
        {
            compatibility.insert(SourceCompatibilityNote::TsEmptyPayloadPadding);
        }
    }
    let native = VerificationReport::combine(
        ValidationDepth::Native,
        super::super::VerificationScope::SourceAdmission,
        reports,
    );
    let mut verification = if converter.backend.validation_depth == ValidationDepth::Decoded {
        let mut decoded = converter
            .with_processing_clone(ProcessingMode::Stitch)
            .validate_source(
                VerificationSource {
                    input: source.entrypoint(),
                    ..input
                },
                purpose,
            )?
            .verification;
        decoded.require_native(&native)?;
        decoded
    } else {
        native
    };
    verification.refresh_passed();
    if !verification.passed {
        return Err(ConversionError("source-media-verification-failed"));
    }
    source.check_original()?;
    Ok(SourceQualification {
        input: source.report.clone(),
        actual_format: OutputFormat::HlsTs,
        track_protection,
        compatibility_notes: compatibility.into_iter().collect(),
        eligible_formats: vec![OutputFormat::Dash, OutputFormat::Hls, OutputFormat::HlsTs],
        verification,
    })
}

fn view<'a>(
    part: &'a Admitted<'_>,
    index: usize,
    original: &'a ts::NativeEvidence,
) -> ts::View<'a> {
    ts::View {
        input: &part.part.media.resources[0].0,
        metadata: &part.part.media.resources[0].0,
        index,
        key: None,
        all_keys: None,
        transport: true,
        native: Some(original),
        unedited: false,
    }
}

fn content_pair(a: &Admitted<'_>, b: &Admitted<'_>) -> Result<bool, ConversionError> {
    if a.tracks.len() != b.tracks.len() {
        return Ok(false);
    }
    for (_, kind, first) in &a.tracks {
        let Some((_, _, second)) = b.tracks.iter().find(|(_, other, _)| other == kind) else {
            return Ok(false);
        };
        if first.config != second.config || !first.evidence()?.payload_equal(&second.evidence()?) {
            return Ok(false);
        }
    }
    Ok(true)
}

fn compare_pair(
    converter: &Converter,
    first: &Admitted<'_>,
    second: &Admitted<'_>,
) -> Result<VerificationReport, ConversionError> {
    let mut reports = Vec::new();
    metadata::pair(
        &first.part.declaration.metadata,
        &second.part.declaration.metadata,
    )?;
    let declarations = first.part.media.resources.len() == second.part.media.resources.len()
        && first
            .part
            .media
            .resources
            .iter()
            .zip(&second.part.media.resources)
            .all(|((_, a), (_, b))| {
                manifest_hls::duration(a).ok() == manifest_hls::duration(b).ok()
            });
    for (first_index, kind, original) in &first.tracks {
        let (second_index, _, result) = second
            .tracks
            .iter()
            .find(|(_, other, _)| other == kind)
            .ok_or(LAYOUT)?;
        let mut report = converter.backend.native_verify_transport(
            view(first, *first_index, original),
            view(second, *second_index, result),
        )?;
        let original_boundaries = original.segment_timings()?;
        let result_boundaries = result.segment_timings()?;
        report.checks.packet_payloads_equal &= original_boundaries.len() == result_boundaries.len()
            && original_boundaries
                .iter()
                .zip(&result_boundaries)
                .all(|(a, b)| a.count == b.count);
        report.checks.packet_timing_equal &= original_boundaries == result_boundaries
            && declarations
            && first.window == second.window;
        report.refresh_passed();
        reports.push(report);
    }
    Ok(VerificationReport::combine(
        ValidationDepth::Native,
        super::super::VerificationScope::Comparison,
        reports,
    ))
}

fn compare(
    converter: &Converter,
    source: &HlsSource,
    output: &HlsSource,
    request: VerificationRequest<'_>,
) -> Result<VerificationReport, ConversionError> {
    let _scope = converter.backend.scope(Phase::Verification);
    let a = admitted(converter, source)?;
    let b = admitted(converter, output)?;
    if a.len() != b.len() {
        return Err(ConversionError("presentation-track-count-mismatch"));
    }
    let mut available = vec![true; b.len()];
    let mut reports = Vec::new();
    for first in &a {
        converter.backend.check_cancelled()?;
        let mut found = None;
        let mut failed = None;
        for (index, second) in b.iter().enumerate() {
            converter.backend.check_cancelled()?;
            if available[index]
                && content_pair(first, second)?
                && metadata::pair(
                    &first.part.declaration.metadata,
                    &second.part.declaration.metadata,
                )
                .is_ok()
            {
                let proof = compare_pair(converter, first, second)?;
                if proof.passed {
                    found = Some((index, proof));
                    break;
                }
                if failed.is_none() {
                    failed = Some(proof);
                }
            }
        }
        let Some((index, proof)) = found else {
            reports.push(failed.ok_or(ConversionError("presentation-track-content-mismatch"))?);
            break;
        };
        available[index] = false;
        reports.push(proof);
    }
    let native = VerificationReport::combine(
        ValidationDepth::Native,
        super::super::VerificationScope::Comparison,
        reports,
    );
    if converter.backend.validation_depth == ValidationDepth::Decoded {
        let mut decoded = converter
            .with_processing_clone(ProcessingMode::Stitch)
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: source.entrypoint(),
                    ..request.source
                },
                output: VerificationSource {
                    input: output.entrypoint(),
                    ..request.output
                },
            })?;
        decoded.require_native(&native)?;
        Ok(decoded)
    } else {
        Ok(native)
    }
}

pub(super) fn verify(
    converter: &Converter,
    source: &HlsSource,
    output: &HlsSource,
    request: VerificationRequest<'_>,
) -> Result<VerificationReport, ConversionError> {
    source.check_original()?;
    output.check_original()?;
    let checked = converter.checked_manifest(output.entrypoint(), OutputFormat::HlsTs)?;
    let mut report = compare(converter, source, output, request)?;
    checked.check(converter)?;
    report.native_manifest = Some(checked.report);
    report.refresh_passed();
    source.check_original()?;
    output.check_original()?;
    Ok(report)
}

fn validate_request(request: &ConversionRequest<'_>) -> Result<(), ConversionError> {
    if request.format != OutputFormat::HlsTs {
        return Err(ConversionError("processing-container-change-not-supported"));
    }
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
            .and_then(super::super::HlsOutputKey::associated_iv)
            .is_none()
    {
        return Err(ConversionError("hls-sample-output-iv-required"));
    }
    if request.target != TargetProtection::HlsSampleAes
        && request
            .hls_output_key
            .and_then(super::super::HlsOutputKey::associated_iv)
            .is_some()
    {
        return Err(ConversionError("output-iv-not-used"));
    }
    Ok(())
}

fn copy(source: &Path, destination: &Path, converter: &Converter) -> Result<(), ConversionError> {
    converter.backend.check_cancelled()?;
    let mut input = files::regular_input(source)?;
    let expected = input
        .metadata()
        .map_err(|_| ConversionError("input-unreadable"))?;
    let mut output = files::create_new(destination)?;
    let mut count = 0_u64;
    let mut buffer = Zeroizing::new(vec![0_u8; 64 * 1024]);
    loop {
        converter.backend.check_cancelled()?;
        let got = input
            .read(buffer.as_mut())
            .map_err(|_| ConversionError("media-copy-failed"))?;
        if got == 0 {
            break;
        }
        count = count
            .checked_add(u64::try_from(got).map_err(|_| ConversionError("media-size-limit"))?)
            .filter(|n| *n <= super::super::MAX_MEDIA_BYTES && *n <= expected.len())
            .ok_or(ConversionError("media-size-limit"))?;
        output
            .write_all(&buffer[..got])
            .map_err(|_| ConversionError("media-copy-failed"))?;
    }
    if count != expected.len()
        || input
            .metadata()
            .map_err(|_| ConversionError("input-unreadable"))?
            .modified()
            .ok()
            != expected.modified().ok()
    {
        return Err(ConversionError("native-source-changed"));
    }
    output
        .sync_all()
        .map_err(|_| ConversionError("media-copy-failed"))?;
    converter.backend.check_cancelled()
}

fn protect_resources(
    converter: &Converter,
    directory: &Path,
    part: &Part,
    request: &ConversionRequest<'_>,
) -> Result<(Vec<HlsResource>, Option<String>), ConversionError> {
    let mut encryptor = ts::SegmentEncryptor::new();
    let initial_key_tag = if request.target == TargetProtection::HlsSampleAes {
        let key = request
            .hls_output_key
            .ok_or(ConversionError("output-key-role-mismatch"))?;
        let iv = key
            .associated_iv()
            .ok_or(ConversionError("hls-sample-output-iv-required"))?;
        let mut tag = format!(
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"{}\",IV=0x{}",
            key.key_uri(),
            hex::encode(iv.0.as_ref())
        );
        if converter
            .output_signaling
            .as_ref()
            .is_some_and(|signaling| {
                signaling
                    .systems()
                    .contains(&super::super::DrmSystem::FairPlay)
            })
        {
            tag.push_str(",KEYFORMAT=\"com.apple.streamingkeydelivery\",KEYFORMATVERSIONS=\"1\"");
        }
        Some(tag)
    } else {
        None
    };
    let mut resources = Vec::with_capacity(part.media.resources.len());
    for (index, (source, duration)) in part.media.resources.iter().enumerate() {
        converter.backend.check_cancelled()?;
        let path = directory.join(format!("segment-{index:05}.ts"));
        let key_tag = match request.target {
            TargetProtection::Clear => {
                copy(source, &path, converter)?;
                None
            }
            TargetProtection::HlsAes128 => {
                let key = request
                    .hls_output_key
                    .ok_or(ConversionError("output-key-role-mismatch"))?;
                let iv = super::super::SegmentIv::random()?;
                aes128::encrypt_segment_with_stop(
                    source,
                    &path,
                    &key.key,
                    &iv,
                    &converter.backend.stop,
                )?;
                Some(format!(
                    "#EXT-X-KEY:METHOD=AES-128,URI=\"{}\",IV=0x{}",
                    key.key_uri(),
                    hex::encode(iv.0.as_ref())
                ))
            }
            TargetProtection::HlsSampleAes => {
                let key = request
                    .hls_output_key
                    .ok_or(ConversionError("output-key-role-mismatch"))?;
                encryptor.encrypt_segment(
                    source,
                    &path,
                    &key.key,
                    key.associated_iv()
                        .ok_or(ConversionError("hls-sample-output-iv-required"))?,
                    &converter.backend.stop,
                )?;
                None
            }
            _ => return Err(ConversionError("ts-output-profile-not-supported")),
        };
        resources.push(HlsResource {
            path,
            duration: duration.clone(),
            key_tag,
        });
    }
    if request.target == TargetProtection::HlsSampleAes {
        encryptor.finish()?;
    }
    Ok((resources, initial_key_tag))
}

fn output_part(
    converter: &Converter,
    root: &Path,
    ordinal: usize,
    part: &Part,
    request: &ConversionRequest<'_>,
) -> Result<HlsPart, ConversionError> {
    let _permit = converter
        .backend
        .runner
        .native_permit(
            Claim {
                cpu_threads: 1,
                memory_bytes: 128 * 1024 * 1024,
            },
            &converter.backend.stop,
            converter.backend.timeout,
        )
        .map_err(|error| match error {
            ReservationError::Cancelled => ConversionError("cancelled"),
            ReservationError::Timeout | ReservationError::Insufficient => {
                ConversionError("resource-budget")
            }
        })?;
    let directory = root.join(format!("track-{ordinal:04}"));
    files::create_dir(&directory)?;
    let (resources, initial_key_tag) = protect_resources(converter, &directory, part, request)?;
    let mut codecs = Vec::new();
    let mut resolution = None;
    for native in part.media.native.values() {
        match native.metadata()? {
            ts::canonical::TrackMetadata::Video(video) => {
                codecs.push(format!(
                    "avc1.{:02x}{:02x}{:02x}",
                    video.profile_idc, video.constraint_flags, video.level_idc
                ));
                resolution = Some((u64::from(video.width), u64::from(video.height)));
            }
            ts::canonical::TrackMetadata::Audio { .. } => codecs.push("mp4a.40.2".into()),
        }
    }
    Ok(HlsPart {
        initialization: None,
        resources,
        codecs,
        resolution,
        metadata: part.declaration.metadata.clone(),
        initial_key_tag,
    })
}

fn output_keys(
    request: &ConversionRequest<'_>,
) -> Result<super::super::HlsInputKeys, ConversionError> {
    let keys = request
        .hls_output_key
        .map(|key| {
            super::super::HlsInputKeys::new_with_ivs(vec![(
                key.key_uri().to_owned(),
                super::super::Aes128Key(key.key.0.clone()),
                key.associated_iv()
                    .map(|iv| super::super::SegmentIv(iv.0.clone())),
            )])
        })
        .transpose()?
        .unwrap_or_default();
    Ok(keys)
}

pub(super) fn convert(
    converter: &Converter,
    request: ConversionRequest<'_>,
    source: HlsSource,
) -> Result<ConversionReport, ConversionError> {
    validate_request(&request)?;
    let native_converter = converter
        .with_processing_clone(ProcessingMode::Segments)
        .with_decode(false);
    let input = VerificationSource {
        input: source.entrypoint(),
        input_keys: request.input_keys,
        hls_keys: request.hls_input_keys,
    };
    qualify(
        &native_converter,
        &source,
        input,
        SourceValidationPurpose::Conversion,
    )?;
    let destination = super::super::destination(request.output)?;
    let stage = converter.output_stage(&destination)?;
    {
        let _scope = converter.backend.scope(Phase::Packaging);
        let parts = converter.run_tracks(source.parts.len(), |index| {
            output_part(
                converter,
                stage.path(),
                index,
                &source.parts[index],
                &request,
            )
        })?;
        manifest_hls::write(stage.path(), &parts, &converter.backend.stop)?;
        converter.finish_multi_signaling(
            stage.path(),
            request.format,
            request.target,
            request.output_key,
            request.hls_key_uri,
            request.hls_output_key,
        )?;
        files::seal(stage.path())?;
    }
    let manifest = stage.path().join("master.m3u8");
    let checked = converter.checked_manifest(&manifest, OutputFormat::HlsTs)?;
    checked.require_clean()?;
    let output_keys = output_keys(&request)?;
    let empty = InputKeys::default();
    let output_input = VerificationSource {
        input: &manifest,
        input_keys: &empty,
        hls_keys: &output_keys,
    };
    let output = HlsSource::load(converter, output_input)?;
    let qualified = qualify(
        &native_converter,
        &output,
        output_input,
        SourceValidationPurpose::Verification,
    )?;
    if qualified
        .track_protection
        .iter()
        .any(|protection| !protection.matches_target(request.target))
    {
        return Err(ConversionError("output-protection-mismatch"));
    }
    let mut verification = compare(
        converter,
        &source,
        &output,
        VerificationRequest {
            source: input,
            output: output_input,
        },
    )?;
    verification.native_manifest = Some(checked.report.clone());
    verification.refresh_passed();
    if !verification.passed {
        return Err(ConversionError("media-verification-failed"));
    }
    converter.backend.checkpoint(&[&stage]);
    converter.publish_directory_checked(&stage, &destination, || {
        source.check_original()?;
        output.check_original()?;
        checked.check(converter)
    })?;
    Ok(ConversionReport {
        schema_version: 2,
        input: source.report,
        output: output.report,
        verification,
        processing: Some(converter.backend.processing(
            inspection::ProcessingRouteId::MpegTsSegments,
            inspection::InputPreparation::OriginalSegments,
        )),
    })
}

#[cfg(test)]
mod tests;
