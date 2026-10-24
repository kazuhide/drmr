//! A supported clock family is separate from exact source-to-output preservation.
use crate::{
    conversion::{ConversionError, native_media, ts},
    inspection::TrackKind,
};

const BAD: ConversionError = ConversionError("native-clock-profile-mismatch");

/// Private context for single-track static HLS/DASH audio, or its transport-derived copies.
/// This is never an assertion of authenticity or a timestamp-comparison tolerance.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::conversion) struct ManifestAacClock {
    channels: u8,
}

impl ManifestAacClock {
    /// Call only after resolving a supported static HLS/DASH resource graph.
    pub(in crate::conversion) fn qualify_manifest(
        movie: &native_media::Movie,
    ) -> Result<Option<Self>, ConversionError> {
        let [track] = movie.tracks.as_slice() else {
            return Ok(None);
        };
        if !movie.fragmented {
            return Ok(None);
        }
        Self::qualify_track(track)
    }

    pub(in crate::conversion) fn from_transport(
        source: &ts::NativeEvidence,
    ) -> Result<Option<Self>, ConversionError> {
        source.validate()?;
        source.evidence()?;
        source.timing()?;
        match source.config {
            ts::CanonicalConfig::Audio {
                channels,
                sample_rate: 44_100,
            } => Ok(Some(Self { channels })),
            _ => Ok(None),
        }
    }

    fn qualify_track(
        track: &native_media::TrackDescriptor,
    ) -> Result<Option<Self>, ConversionError> {
        if track.index != 0
            || track.kind != TrackKind::Audio
            || track.codec != *b"mp4a"
            || track.decoder_config_kind != *b"esds"
            || track.timescale != 90_000
        {
            return Ok(None);
        }
        let aac = native_media::AacConfiguration::parse(&track.decoder_config)?;
        Ok((aac.core_object_type == 2
            && !aac.sbr
            && aac.frame_samples == 1024
            && aac.sample_rate == 44_100
            && (1..=7).contains(&aac.channel_configuration))
        .then_some(Self {
            channels: aac.channel_configuration,
        }))
    }

    pub(super) fn check_track(
        self,
        track: &native_media::TrackDescriptor,
    ) -> Result<(), ConversionError> {
        if Self::qualify_track(track)? != Some(self) {
            return Err(BAD);
        }
        Ok(())
    }

    pub(super) fn check_movie(self, movie: &native_media::Movie) -> Result<(), ConversionError> {
        if Self::qualify_manifest(movie)? != Some(self) {
            return Err(BAD);
        }
        Ok(())
    }
}
