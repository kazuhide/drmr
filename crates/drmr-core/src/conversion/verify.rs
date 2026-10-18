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
    key: Option<&'a ContentKey>,
}
impl<'a> StreamView<'a> {
    pub fn new(input: &'a Path, index: usize, key: Option<&'a ContentKey>) -> Self {
        Self {
            input,
            metadata: input,
            index,
            key,
        }
    }
    pub fn with_metadata(mut self, metadata: &'a Path) -> Self {
        self.metadata = metadata;
        self
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
    pub fn verify_views(
        &self,
        source: StreamView<'_>,
        result: StreamView<'_>,
    ) -> Result<VerificationReport, ConversionError> {
        let source_profile = self.profile_stream(source.input, source.index, source.key)?;
        let result_profile = self.profile_stream(result.input, result.index, result.key)?;
        let source_metadata = if source.metadata == source.input {
            source_profile.clone()
        } else {
            self.profile_stream(source.metadata, source.index, source.key)?
        };
        let result_metadata = if result.metadata == result.input {
            result_profile.clone()
        } else {
            self.profile_stream(result.metadata, result.index, result.key)?
        };
        let stream_metadata_equal = source_metadata == result_metadata;
        let packet_timing_equal = source_profile["time_base"] == result_profile["time_base"]
            && self.packets_stream(source.input, source.index, source.key)?
                == self.packets_stream(result.input, result.index, result.key)?
            && super::progressive::presentation_windows_equal(source.input, result.input)?;
        let packet_payloads_equal =
            self.hash_stream(source.input, source.index, true, source.key)?
                == self.hash_stream(result.input, result.index, true, result.key)?;
        let decoded_content_equal =
            self.hash_stream(source.input, source.index, false, source.key)?
                == self.hash_stream(result.input, result.index, false, result.key)?;
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
