//! Independent full decoded-content and compressed-payload comparisons.
use super::{ConversionError, backend::Backend};
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
/// The metadata check covers stream summaries, not every packet timestamp.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct VerificationChecks {
    pub packet_payloads_equal: bool,
    pub stream_metadata_equal: bool,
    pub decoded_content_equal: bool,
}
impl Backend {
    pub fn verify_clear(
        &self,
        source: &Path,
        result: &Path,
    ) -> Result<VerificationReport, ConversionError> {
        let packet_payloads_equal = self.hash(source, true)? == self.hash(result, true)?;
        let decoded_content_equal = self.hash(source, false)? == self.hash(result, false)?;
        let stream_metadata_equal = self.profile(source)? == self.profile(result)?;
        let passed = packet_payloads_equal && decoded_content_equal && stream_metadata_equal;
        Ok(VerificationReport {
            schema_version: 1,
            passed,
            media_verified: passed,
            checks: VerificationChecks {
                packet_payloads_equal,
                stream_metadata_equal,
                decoded_content_equal,
            },
            license_playback_verified: false,
        })
    }
}
