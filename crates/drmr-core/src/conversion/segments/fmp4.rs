//! Fragmented MP4 resources keep their original sample membership and presentation clocks.
mod manifest;
pub(in crate::conversion) use manifest::codec;
mod proof;
mod signaling;
mod source;

use super::source::HlsSource;
use crate::{
    conversion::{
        ConversionError, ConversionReport, ConversionRequest, Converter, InputKeys, ProcessingMode,
        SourceQualification, SourceValidationPurpose, ValidationDepth, VerificationReport,
        VerificationRequest, VerificationSource,
    },
    inspection::{InputPreparation, ProcessingRouteId},
};
use source::{DashSource, Presentation, Source};

fn layout_tracks(
    presentation: &Presentation,
    media: &[crate::conversion::native::SegmentedMedia],
) -> Vec<crate::conversion::layout::QualifiedTrack> {
    media
        .iter()
        .enumerate()
        .map(
            |(ordinal, part)| crate::conversion::layout::QualifiedTrack {
                kind: part.media.tracks[0].descriptor.kind,
                selector: if presentation.format == crate::inspection::OutputFormat::Dash {
                    ordinal
                } else {
                    0
                },
            },
        )
        .collect()
}

pub(super) fn qualify_hls(
    converter: &Converter,
    source: &HlsSource,
    input: VerificationSource<'_>,
    purpose: SourceValidationPurpose,
) -> Result<SourceQualification, ConversionError> {
    converter.backend.check_cancelled()?;
    let presentation = source::hls(converter, source)?;
    let mut qualification = proof::qualify(converter, &presentation, input.input_keys)?;
    qualification.hls_timing = if matches!(purpose, SourceValidationPurpose::Conversion)
        && converter.inherits_hls_target()
    {
        Some(
            crate::conversion::processing::segment_target::HlsTiming::from_admitted(
                source.parts.iter().map(|part| part.media.target_seconds),
                source
                    .parts
                    .iter()
                    .zip(&qualification.layout_tracks)
                    .filter_map(|(part, track)| {
                        (track.kind == crate::inspection::TrackKind::Video)
                            .then_some(part.media.maximum_segment_ms)
                    }),
            )?,
        )
    } else {
        None
    };
    if converter.backend.validation_depth == ValidationDepth::Decoded {
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
        decoded.require_native(&qualification.verification)?;
        qualification.verification = decoded;
    }
    source.check_original()?;
    Ok(qualification)
}

pub(super) fn qualify_dash(
    converter: &Converter,
    input: VerificationSource<'_>,
    purpose: SourceValidationPurpose,
) -> Result<SourceQualification, ConversionError> {
    converter.backend.check_cancelled()?;
    let source = DashSource::load(converter, input)?;
    let mut qualification = proof::qualify(converter, &source.presentation, input.input_keys)?;
    if converter.backend.validation_depth == ValidationDepth::Decoded {
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
        decoded.require_native(&qualification.verification)?;
        qualification.verification = decoded;
    }
    source.check_original()?;
    Ok(qualification)
}

pub(super) fn convert_hls(
    converter: &Converter,
    request: ConversionRequest<'_>,
    source: HlsSource,
) -> Result<ConversionReport, ConversionError> {
    converter.backend.check_cancelled()?;
    let presentation = source::hls(converter, &source)?;
    convert(converter, request, &Source::Hls(source), presentation)
}

pub(super) fn convert_dash(
    converter: &Converter,
    request: ConversionRequest<'_>,
) -> Result<ConversionReport, ConversionError> {
    let input = VerificationSource {
        input: request.input,
        input_keys: request.input_keys,
        hls_keys: request.hls_input_keys,
    };
    let source = DashSource::load(converter, input)?;
    let presentation = source.presentation.clone();
    convert(converter, request, &Source::Dash(source), presentation)
}

fn convert(
    converter: &Converter,
    request: ConversionRequest<'_>,
    source: &Source,
    presentation: Presentation,
) -> Result<ConversionReport, ConversionError> {
    converter.backend.check_cancelled()?;
    manifest::validate_request(converter, &request)?;
    let admitted = proof::admit(converter, &presentation, request.input_keys)?;
    let command = converter.using_admitted_output_options(
        source.entrypoint(),
        presentation.report.format,
        &layout_tracks(&presentation, &admitted),
        request.format,
        ProcessingMode::Segments,
    )?;
    let converter = &command;
    let destination = crate::conversion::destination(request.output)?;
    let stage = converter.output_stage(&destination)?;
    let work = crate::conversion::workspace()?;
    manifest::transform(
        converter,
        &presentation,
        &admitted,
        &request,
        stage.path(),
        work.path(),
    )?;
    finish_manifest(converter, source, &stage, request.format)?;
    let output_keys = proof::output_keys(request.output_key)?;
    let hls_keys = proof::output_hls_keys(request.hls_output_key)?;
    let output_path = super::source::entrypoint(stage.path(), request.format);
    let candidate = Source::load(
        converter,
        VerificationSource {
            input: &output_path,
            input_keys: &output_keys,
            hls_keys: &hls_keys,
        },
    )?;
    let candidate_presentation = candidate.presentation(converter)?;
    let outputs = proof::admit(converter, &candidate_presentation, &output_keys)?;
    proof::bind_output(&candidate_presentation, &outputs, &request)?;
    converter.verify_segment_signaling(
        candidate.frozen(),
        request.format,
        request.target,
        request.output_key,
        request.hls_key_uri,
        request.hls_output_key,
    )?;
    let plaintext_signaling =
        signaling::verify_plaintext(converter, &candidate_presentation, &outputs)?;
    let native = proof::compare(
        converter,
        &presentation,
        &admitted,
        &candidate_presentation,
        &outputs,
    )?;
    let mut verification = converter.verify_admitted_segments(
        VerificationRequest {
            source: VerificationSource {
                input: source.entrypoint(),
                input_keys: request.input_keys,
                hls_keys: request.hls_input_keys,
            },
            output: VerificationSource {
                input: candidate.entrypoint(),
                input_keys: &output_keys,
                hls_keys: &hls_keys,
            },
        },
        request.format,
        native,
    )?;
    let checked = converter.checked_manifest(&output_path, request.format)?;
    checked.require_clean()?;
    verification.native_manifest = Some(checked.report.clone());
    verification.refresh_passed();
    if !verification.passed {
        return Err(ConversionError("round-trip-verification-failed"));
    }
    let output = proof::inspection(&candidate_presentation, &outputs)?;
    crate::conversion::files::seal(stage.path())?;
    converter.backend.checkpoint(&[&work, &stage]);
    converter.publish_directory_checked(&stage, &destination, || {
        source.check_original()?;
        candidate.check_original()?;
        plaintext_signaling.check()?;
        checked.check(converter)
    })?;
    Ok(ConversionReport {
        schema_version: 2,
        input: presentation.report,
        output,
        verification,
        processing: Some(converter.backend.processing(
            ProcessingRouteId::Fmp4Segments,
            InputPreparation::OriginalSegments,
        )),
    })
}

fn finish_manifest(
    converter: &Converter,
    source: &Source,
    stage: &crate::conversion::files::Stage,
    format: crate::inspection::OutputFormat,
) -> Result<(), ConversionError> {
    if format == crate::inspection::OutputFormat::Hls
        && let Source::Hls(source) = source
    {
        source.finish_hls_target(converter, stage.path())?;
    }
    converter.finish_output_options(stage, format)
}

pub(super) fn verify(
    converter: &Converter,
    request: VerificationRequest<'_>,
) -> Result<VerificationReport, ConversionError> {
    converter.backend.check_cancelled()?;
    let source = Source::load(converter, request.source)?;
    let output = Source::load(converter, request.output)?;
    let first = source.presentation(converter)?;
    let second = output.presentation(converter)?;
    let sources = proof::admit(converter, &first, request.source.input_keys)?;
    converter.check_admitted_output_options(
        source.entrypoint(),
        first.report.format,
        &layout_tracks(&first, &sources),
        second.format,
        ProcessingMode::Segments,
    )?;
    let outputs = proof::admit(converter, &second, request.output.input_keys)?;
    let plaintext_signaling = signaling::verify_plaintext(converter, &second, &outputs)?;
    let native = proof::compare(converter, &first, &sources, &second, &outputs)?;
    let mut report = converter.verify_admitted_segments(
        VerificationRequest {
            source: VerificationSource {
                input: source.entrypoint(),
                ..request.source
            },
            output: VerificationSource {
                input: output.entrypoint(),
                ..request.output
            },
        },
        second.format,
        native,
    )?;
    if let Some(checked) = converter.checked_output_manifest(request.output.input)? {
        checked.check(converter)?;
        report.native_manifest = Some(checked.report);
        report.refresh_passed();
    }
    source.check_original()?;
    output.check_original()?;
    plaintext_signaling.check()?;
    Ok(report)
}

#[cfg(test)]
mod tests;
