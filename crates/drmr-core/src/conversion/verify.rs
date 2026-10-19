//! Independent full decoded-content and compressed-payload comparisons.
use super::{ContentKey, ConversionError, backend::Backend};
use serde::Serialize;
use std::path::Path;

/// Full decode and packet-payload evidence. This does not prove a license server or device playback.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerificationReport {
    pub schema_version: u32,
    pub passed: bool,
    pub media_verified: bool,
    pub checks: VerificationChecks,
    pub license_playback_verified: bool,
}
/// An executed check's outcome, kept distinct from an unchecked metadata claim.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckOutcome {
    Pass,
    Fail,
}
impl CheckOutcome {
    const fn from_equal(equal: bool) -> Self {
        if equal { Self::Pass } else { Self::Fail }
    }
    pub const fn is_pass(self) -> bool {
        matches!(self, Self::Pass)
    }
    pub(super) fn include(&mut self, other: Self) {
        if other == Self::Fail {
            *self = Self::Fail;
        }
    }
}
/// Packet timing compares every presentation timestamp, decode timestamp, and duration.
/// The qualified progressive profile also compares exact presentation windows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerificationChecks {
    pub stream_metadata: CheckOutcome,
    pub packet_payloads_equal: bool,
    pub packet_timing_equal: bool,
    pub decoded_content_equal: bool,
}
/// Independent demux/decode input and its byte-preserving local initialization view.
/// HLS demuxers omit some initialization metadata, so it is read from the assembled MP4.
#[derive(Clone, Copy)]
pub(super) struct StreamView<'a> {
    input: &'a Path,
    metadata: &'a Path,
    index: usize,
    key: Option<super::keys::ProbeKeys<'a>>,
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
    pub fn with_original_keys(self, all: bool, keys: &'a super::InputKeys) -> Self {
        if all { self.with_all_keys(keys) } else { self }
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
        let source_profile = self.profile_stream_keys(source.input, source.index, source.key)?;
        let result_profile = self.profile_stream_keys(result.input, result.index, result.key)?;
        let source_metadata = if source.metadata == source.input {
            source_profile.clone()
        } else {
            self.profile_stream_keys(source.metadata, source.index, source.key)?
        };
        let result_metadata = if result.metadata == result.input {
            result_profile.clone()
        } else {
            self.profile_stream_keys(result.metadata, result.index, result.key)?
        };
        for (view, profile) in [(source, &source_profile), (result, &result_profile)] {
            if matches!(view.key, Some(super::keys::ProbeKeys::All(_))) {
                let counts = super::cenc::sample_counts(&super::audio::movie(view.input)?)?;
                let actual = profile["nb_read_packets"]
                    .as_str()
                    .and_then(|n| n.parse::<u32>().ok());
                if actual != counts.get(view.index).copied() {
                    return Err(ConversionError("progressive-cenc-profile-not-supported"));
                }
            }
        }
        let stream_metadata_equal = source_metadata == result_metadata;
        let source_window = super::finite::window(source.metadata, source.index)?;
        let result_window = super::finite::window(result.metadata, result.index)?;
        let windows_equal = match (source_window, result_window) {
            (Some(a), Some(b)) => {
                a.equivalent(b)
                    && super::progressive::playback_controls_equal(
                        source.metadata,
                        source.index,
                        result.metadata,
                        result.index,
                    )?
            }
            (None, None) => true,
            _ => false,
        };
        let packet_timing_equal = windows_equal
            && source_profile["time_base"] == result_profile["time_base"]
            && self.packets_stream_keys(source.input, source.index, source.key)?
                == self.packets_stream_keys(result.input, result.index, result.key)?
            && super::progressive::presentation_windows_equal(source.metadata, result.metadata)?;
        let packet_payloads_equal =
            self.hash_stream_keys(source.input, source.index, true, source.key)?
                == self.hash_stream_keys(result.input, result.index, true, result.key)?;
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
        let passed = stream_metadata_equal
            && packet_payloads_equal
            && decoded_content_equal
            && packet_timing_equal;
        Ok(VerificationReport {
            schema_version: 1,
            passed,
            media_verified: passed,
            checks: VerificationChecks {
                stream_metadata: CheckOutcome::from_equal(stream_metadata_equal),
                packet_payloads_equal,
                packet_timing_equal,
                decoded_content_equal,
            },
            license_playback_verified: false,
        })
    }
}
