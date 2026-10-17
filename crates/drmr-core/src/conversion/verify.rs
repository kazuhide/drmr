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
/// Packet timing compares every presentation timestamp, decode timestamp, and duration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerificationChecks {
    pub packet_payloads_equal: bool,
    pub packet_timing_equal: bool,
    pub decoded_content_equal: bool,
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
        let packet_timing_equal = self.profile_stream(source, source_index)?
            == self.profile_stream(result, result_index)?
            && self.packets_stream(source, source_index)?
                == self.packets_stream(result, result_index)?;
        let packet_payloads_equal = self.hash_stream(source, source_index, true, source_key)?
            == self.hash_stream(result, result_index, true, result_key)?;
        let decoded_content_equal = self.hash_stream(source, source_index, false, source_key)?
            == self.hash_stream(result, result_index, false, result_key)?;
        let passed = packet_payloads_equal && decoded_content_equal && packet_timing_equal;
        Ok(VerificationReport {
            schema_version: 1,
            passed,
            media_verified: passed,
            checks: VerificationChecks {
                packet_payloads_equal,
                packet_timing_equal,
                decoded_content_equal,
            },
            license_playback_verified: false,
        })
    }
}
