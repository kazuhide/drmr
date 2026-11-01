//! Original segment processing keeps resource membership separate from cipher metadata.
mod fmp4;
pub(in crate::conversion) use fmp4::codec as fmp4_codec;
mod manifest_hls;
pub(in crate::conversion) use manifest_hls::rates as hls_rates;
mod source;
mod transport;

use super::{
    ConversionError, ConversionReport, ConversionRequest, Converter, SourceQualification,
    SourceValidationPurpose, VerificationReport, VerificationRequest, VerificationSource,
    processing::{ProcessingMode, ProcessingSelection},
};
use crate::inspection::OutputFormat;
use source::HlsSource;

pub(super) fn qualify(
    converter: &Converter,
    input: VerificationSource<'_>,
    purpose: SourceValidationPurpose,
) -> Result<SourceQualification, ConversionError> {
    match converter.default_output_format(input.input)? {
        OutputFormat::Mp4 => Err(ConversionError("processing-segments-source-required")),
        OutputFormat::Dash => fmp4::qualify_dash(converter, input, purpose),
        OutputFormat::Hls | OutputFormat::HlsTs => {
            let source = HlsSource::load(converter, input)?;
            if source.format == OutputFormat::HlsTs {
                transport::qualify(converter, &source, input, purpose)
            } else {
                fmp4::qualify_hls(converter, &source, input, purpose)
            }
        }
    }
}

pub(super) fn convert(
    converter: &Converter,
    request: ConversionRequest<'_>,
) -> Result<ConversionReport, ConversionError> {
    if let Some(signaling) = &converter.output_signaling {
        signaling.validate_profile(request.format, request.target)?;
        signaling.validate_uri(
            request
                .hls_key_uri
                .or_else(|| request.hls_output_key.map(super::HlsOutputKey::key_uri)),
        )?;
    }
    let input = VerificationSource {
        input: request.input,
        input_keys: request.input_keys,
        hls_keys: request.hls_input_keys,
    };
    let declared = converter.default_output_format(input.input)?;
    ProcessingSelection::resolve(ProcessingMode::Segments, declared, request.format)?;
    match declared {
        OutputFormat::Mp4 => Err(ConversionError("processing-segments-source-required")),
        OutputFormat::Dash => fmp4::convert_dash(converter, request),
        OutputFormat::Hls | OutputFormat::HlsTs => {
            let source = HlsSource::load(converter, input)?;
            ProcessingSelection::resolve(ProcessingMode::Segments, source.format, request.format)?;
            if source.format == OutputFormat::HlsTs {
                transport::convert(converter, request, source)
            } else {
                fmp4::convert_hls(converter, request, source)
            }
        }
    }
}

pub(super) fn verify(
    converter: &Converter,
    request: VerificationRequest<'_>,
) -> Result<VerificationReport, ConversionError> {
    match converter.default_output_format(request.source.input)? {
        OutputFormat::Mp4 => Err(ConversionError("processing-segments-source-required")),
        OutputFormat::HlsTs => {
            ProcessingSelection::resolve(
                ProcessingMode::Segments,
                OutputFormat::HlsTs,
                converter.default_output_format(request.output.input)?,
            )?;
            let source = HlsSource::load(converter, request.source)?;
            let output = HlsSource::load(converter, request.output)?;
            transport::verify(converter, &source, &output, request)
        }
        OutputFormat::Hls | OutputFormat::Dash => fmp4::verify(converter, request),
    }
}
