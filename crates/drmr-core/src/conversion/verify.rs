//! Independent full decoded-content and compressed-payload comparisons.
use super::{ContentKey, ConversionError, backend::Backend};
use std::path::Path;

mod evidence;
pub use evidence::{
    CheckOutcome, DecodeEvidence, DecodeNotCheckedReason, ValidationDepth, VerificationChecks,
    VerificationFailure, VerificationReport, VerificationScope,
};

/// Independent demux/decode input and its byte-preserving local initialization view.
/// HLS demuxers omit some initialization metadata, so it is read from the assembled MP4.
#[derive(Clone, Copy)]
pub(super) struct StreamView<'a> {
    pub(in crate::conversion) input: &'a Path,
    pub(in crate::conversion) metadata: &'a Path,
    pub(in crate::conversion) index: usize,
    pub(in crate::conversion) key: Option<super::keys::ProbeKeys<'a>>,
}
impl<'a> StreamView<'a> {
    pub fn new(input: &'a Path, index: usize, key: Option<&'a ContentKey>) -> Self {
        Self {
            input,
            metadata: input,
            index,
            key: key.map(super::keys::ProbeKeys::One),
        }
    }
    pub fn with_all_keys(mut self, keys: &'a super::InputKeys) -> Self {
        self.key = Some(super::keys::ProbeKeys::All(keys));
        self
    }
    pub fn with_metadata(mut self, metadata: &'a Path) -> Self {
        self.metadata = metadata;
        self
    }
}
fn metadata_key(view: StreamView<'_>) -> Option<super::keys::ProbeKeys<'_>> {
    if crate::inspection::inspect(view.metadata)
        .is_ok_and(|r| r.protection == crate::inspection::Protection::Clear)
    {
        None
    } else {
        view.key
    }
}
impl Backend {
    pub fn verify_media(
        &self,
        source: &Path,
        result: &Path,
        source_key: Option<&ContentKey>,
        result_key: Option<&ContentKey>,
    ) -> Result<VerificationReport, ConversionError> {
        self.verify_streams(source, 0, result, 0, source_key, result_key)
    }
    pub fn verify_streams(
        &self,
        source: &Path,
        source_index: usize,
        result: &Path,
        result_index: usize,
        source_key: Option<&ContentKey>,
        result_key: Option<&ContentKey>,
    ) -> Result<VerificationReport, ConversionError> {
        self.verify_views(
            StreamView::new(source, source_index, source_key),
            StreamView::new(result, result_index, result_key),
        )
    }
    fn verify_finite_audio(
        &self,
        source: StreamView<'_>,
        result: StreamView<'_>,
        a: super::finite::Window,
        b: super::finite::Window,
        source_profile: &serde_json::Value,
    ) -> Result<bool, ConversionError> {
        let rate = source_profile["sample_rate"]
            .as_str()
            .and_then(|v| v.parse::<u32>().ok())
            .ok_or(ConversionError("presentation-window-not-supported"))?;
        // Decode complete coded audio as well as each file's independently declared window.
        // Fragmented MOV demuxing does not apply final discard padding in current FFmpeg.
        let a_key = metadata_key(source);
        let b_key = metadata_key(result);
        let channels = source_profile["channels"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok())
            .ok_or(ConversionError("presentation-window-not-supported"))?;
        let a_decoded = self.finite_audio_decode(
            source.metadata,
            source.index,
            a_key,
            a,
            (rate, channels),
            true,
        )?;
        let b_decoded = self.finite_audio_decode(
            result.metadata,
            result.index,
            b_key,
            b,
            (rate, channels),
            true,
        )?;
        let original_presentation = if super::audio::movie(source.metadata)?.fragmented {
            true
        } else {
            let original = self.finite_audio_decode(
                source.metadata,
                source.index,
                a_key,
                super::finite::Window { start: 0, ..a },
                (rate, channels),
                false,
            )?;
            original.raw == a_decoded.presented
                && original.raw_samples == a_decoded.presented_samples
        };
        let expected = source_profile["nb_read_packets"]
            .as_str()
            .and_then(|v| v.parse::<u64>().ok())
            .and_then(|v| v.checked_mul(1024))
            .ok_or(ConversionError("decoded-presentation-range-unavailable"))?;
        Ok(original_presentation && a_decoded.raw_samples == expected && a_decoded == b_decoded)
    }
    pub fn verify_views(
        &self,
        source: StreamView<'_>,
        result: StreamView<'_>,
    ) -> Result<VerificationReport, ConversionError> {
        let _scope = self.scope(super::measurement::Phase::Verification);
        let native = self.native_verify_views(source, result)?;
        if self.validation_depth == super::ValidationDepth::Native {
            return Ok(native);
        }
        // Native sample/configuration/window checks are authoritative for encoded
        // preservation. Decoding adds a separate proof without treating legal
        // container framing differences as changes to the encoded content.
        let source_profile = self.native_profile(source.metadata, source.index)?;
        let source_window = super::finite::window(source.metadata, source.index)?;
        let result_window = super::finite::window(result.metadata, result.index)?;
        let decoded_content_equal = if let (Some(a), Some(b)) = (source_window, result_window) {
            if a.audio && b.audio {
                self.verify_finite_audio(source, result, a, b, &source_profile)?
            } else {
                self.hash_stream_keys(source.input, source.index, false, source.key)?
                    == self.hash_stream_keys(result.input, result.index, false, result.key)?
            }
        } else {
            self.hash_stream_keys(source.input, source.index, false, source.key)?
                == self.hash_stream_keys(result.input, result.index, false, result.key)?
        };
        let mut report = VerificationReport::new(
            self.validation_depth,
            VerificationScope::Comparison,
            native.checks.clone(),
            DecodeEvidence::ContentCompared {
                equal: decoded_content_equal,
            },
        );
        report.require_native(&native)?;
        Ok(report)
    }
}
