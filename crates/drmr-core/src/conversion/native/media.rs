//! Evidence is published only after the original file's complete guarded scan.
use super::super::{ConversionError, InputKeys, backend::Backend, native_media, ts};
use super::samples::{SampleEvidence, Samples};
use std::{collections::BTreeMap, path::Path};

pub(in crate::conversion) struct Track {
    pub descriptor: native_media::TrackDescriptor,
    pub samples: SampleEvidence,
    pub config: Option<ts::CanonicalConfig>,
}
pub(in crate::conversion) struct Media {
    pub movie: native_media::Movie,
    pub tracks: Vec<Track>,
}

fn presentation_offset(
    track: &native_media::TrackDescriptor,
) -> Result<super::samples::Ratio, ConversionError> {
    let offset = match track.edits.as_slice() {
        [] => 0,
        [edit] if edit.media_time >= 0 && edit.rate_integer == 1 && edit.rate_fraction == 0 => {
            edit.media_time
        }
        _ => return Err(ConversionError("native-presentation-window-not-supported")),
    };
    super::samples::Ratio::ticks(offset, track.timescale)
}

fn canonical(
    track: &native_media::TrackDescriptor,
) -> Result<Option<(ts::CanonicalConfig, u8)>, ConversionError> {
    match &track.decoder_config_kind {
        b"avcC" => {
            let (configuration, width) = ts::avc::Configuration::from_avcc(&track.decoder_config)?;
            let metadata = configuration.metadata();
            if track.sample_entry_metadata.width.map(u32::from) != Some(metadata.width)
                || track.sample_entry_metadata.height.map(u32::from) != Some(metadata.height)
            {
                return Err(ConversionError("native-video-dimensions-mismatch"));
            }
            let (sps, pps) = configuration.parameter_sets();
            Ok(Some((
                ts::CanonicalConfig::Video {
                    sps: sps.to_vec(),
                    pps: pps.to_vec(),
                },
                width,
            )))
        }
        b"esds" => {
            // Only the AAC-LC framing used by transport streams needs a common
            // cross-container digest. Other MP4 codecs keep their raw samples.
            let config = native_media::AacConfiguration::parse(&track.decoder_config)?;
            if config.core_object_type == 2
                && !config.sbr
                && config.frame_samples == 1024
                && matches!(config.sample_rate, 44_100 | 48_000)
                && (1..=7).contains(&config.channel_configuration)
            {
                Ok(Some((
                    ts::CanonicalConfig::Audio {
                        channels: config.channel_configuration,
                        sample_rate: config.sample_rate,
                    },
                    4,
                )))
            } else {
                Ok(None)
            }
        }
        _ => Err(ConversionError("native-codec-profile-not-supported")),
    }
}

pub(super) fn accumulator(
    descriptor: &native_media::TrackDescriptor,
) -> Result<(Samples, Option<ts::CanonicalConfig>), ConversionError> {
    let canonical = canonical(descriptor)?;
    let config = canonical.as_ref().map(|(config, _)| config.clone());
    let mut samples = Samples::new(descriptor.timescale, descriptor.sample_count, canonical)?
        .with_presentation_offset(presentation_offset(descriptor)?);
    if descriptor.decoder_config_kind == *b"esds" {
        let aac = native_media::AacConfiguration::parse(&descriptor.decoder_config)?;
        samples = samples.with_audio_clock(aac.frame_samples, aac.sample_rate)?;
    }
    Ok((samples, config))
}

impl Backend {
    pub(in crate::conversion) fn native_payload_digest(
        &self,
        input: &Path,
        index: usize,
        keys: Option<super::super::keys::ProbeKeys<'_>>,
    ) -> Result<String, ConversionError> {
        let media = self.native_media(input, keys)?;
        let track = media
            .tracks
            .get(index)
            .ok_or(ConversionError("stream-selector-mismatch"))?;
        Ok(hex::encode(
            track
                .samples
                .canonical_payload
                .unwrap_or(track.samples.raw_payload),
        ))
    }
    pub(in crate::conversion) fn native_audio_evidence(
        &self,
        view: ts::View<'_>,
    ) -> Result<super::super::audio_presentation::AudioEvidence, ConversionError> {
        if view.transport {
            let native = view
                .native
                .ok_or(ConversionError("ts-original-evidence-required"))?;
            native.validate()?;
            let timing = native.timing()?;
            let ts::CanonicalConfig::Audio { sample_rate, .. } = native.config else {
                return Err(super::super::audio_presentation::MISMATCH);
            };
            return Ok(super::super::audio_presentation::AudioEvidence {
                sample_rate,
                coded_frames: timing.count,
                raw_first_pts: timing.first_pts,
                raw_coded_end: timing.end_pts,
            });
        }
        let media = self.native_media(view.metadata, view.probe_keys())?;
        let track = media
            .tracks
            .get(view.index)
            .ok_or(ConversionError("stream-selector-mismatch"))?;
        let Some(ts::CanonicalConfig::Audio { sample_rate, .. }) = track.config else {
            return Err(super::super::audio_presentation::MISMATCH);
        };
        Ok(super::super::audio_presentation::AudioEvidence {
            sample_rate,
            coded_frames: track.samples.count,
            raw_first_pts: track
                .samples
                .first_pts
                .transport_ticks()?
                .ok_or(super::super::audio_presentation::MISMATCH)?,
            raw_coded_end: track
                .samples
                .end_pts
                .transport_ticks()?
                .ok_or(super::super::audio_presentation::MISMATCH)?,
        })
    }
    pub(in crate::conversion::native) fn native_media(
        &self,
        input: &Path,
        keys: Option<super::super::keys::ProbeKeys<'_>>,
    ) -> Result<Media, ConversionError> {
        let _permit = self
            .runner
            .native_permit(
                crate::resources::Claim {
                    cpu_threads: 1,
                    memory_bytes: 128 * 1024 * 1024,
                },
                &self.stop,
                self.timeout,
            )
            .map_err(|error| match error {
                crate::resources::ReservationError::Cancelled => ConversionError("cancelled"),
                crate::resources::ReservationError::Timeout => {
                    ConversionError("media-tool-timeout")
                }
                crate::resources::ReservationError::Insufficient => {
                    ConversionError("resource-budget")
                }
            })?;
        let owned;
        let empty = InputKeys::default();
        let keys = match keys {
            Some(super::super::keys::ProbeKeys::All(keys)) => keys,
            Some(super::super::keys::ProbeKeys::One(key)) => {
                owned = InputKeys::new(vec![super::super::ContentKey {
                    id: key.id,
                    bytes: key.bytes.clone(),
                }])?;
                &owned
            }
            None => &empty,
        };
        let mut pending = BTreeMap::<
            usize,
            (
                native_media::TrackDescriptor,
                Samples,
                Option<ts::CanonicalConfig>,
            ),
        >::new();
        let movie = native_media::scan(input, keys, &self.stop, &mut |descriptor, info, bytes| {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                pending.entry(descriptor.index)
            {
                let (samples, config) = accumulator(descriptor)?;
                entry.insert((descriptor.clone(), samples, config));
            }
            let (seen, samples, _) = pending
                .get_mut(&descriptor.index)
                .ok_or(ConversionError("native-sample-evidence-invalid"))?;
            if seen != descriptor {
                return Err(ConversionError("native-track-changed"));
            }
            samples.push(info, bytes)
        })?;
        if pending.len() != movie.tracks.len() {
            return Err(ConversionError("native-sample-evidence-incomplete"));
        }
        let mut tracks = Vec::with_capacity(pending.len());
        for (index, (descriptor, samples, config)) in pending {
            if descriptor.index != index || movie.tracks.get(index) != Some(&descriptor) {
                return Err(ConversionError("native-track-changed"));
            }
            let samples = samples.finish()?;
            if descriptor.protection.scheme == native_media::Scheme::Clear
                && samples.protected_samples != 0
            {
                return Err(ConversionError("native-sample-protection-mismatch"));
            }
            tracks.push(Track {
                descriptor,
                samples,
                config,
            });
        }
        self.check_cancelled()?;
        let media = Media { movie, tracks };
        super::duration_headers::validate(&media)?;
        Ok(media)
    }
}
