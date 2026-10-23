//! Complete evidence for one fixed transport source, without comparing it with itself.
use super::{
    Backend, CheckOutcome, ConversionError, NativeEvidence, VerificationChecks, VerificationReport,
    View, canonical,
};
use std::path::Path;

impl Backend {
    pub(in crate::conversion) fn validate_transport_source(
        &self,
        input: &Path,
        index: usize,
        native: &NativeEvidence,
    ) -> Result<VerificationReport, ConversionError> {
        let _scope = self.scope(super::super::measurement::Phase::Verification);
        let view = View {
            input,
            metadata: input,
            index,
            key: None,
            transport: true,
            native: Some(native),
            unedited: false,
        };
        let mut profile = self.profile_stream_options(input, index, None, false)?;
        let config = self.transport_config(view, &profile)?;
        let units = self.access_units(view, &profile, config.clone())?;
        Self::require_native_count(&profile, native.count)?;
        if !native.matches(&config, &units) {
            return Err(ConversionError("ts-original-evidence-mismatch"));
        }
        if let canonical::Config::Audio { channels, .. } = &config {
            self.native_audio_samples(view, *channels, native.count)?;
        }
        if matches!(config, canonical::Config::Video { .. }) {
            self.progressive_transport_frames(view, &profile)?
                .resolve_field_order(&mut profile);
        }
        if !matches!(
            config,
            canonical::Config::Audio {
                sample_rate: 44_100,
                ..
            }
        ) {
            self.packets_stream_options(input, index, None, false)?;
        }
        // Access-unit evidence retains exact 44.1 kHz clocks; the decoded proof
        // still reads the complete source, including every terminal sample.
        self.hash_stream_options(input, index, false, None, false)?;
        Ok(VerificationReport {
            schema_version: 1,
            passed: true,
            media_verified: true,
            license_playback_verified: false,
            native_manifest: None,
            checks: VerificationChecks {
                stream_metadata: CheckOutcome::Pass,
                packet_timing_equal: true,
                packet_payloads_equal: true,
                decoded_content_equal: true,
            },
        })
    }
}
