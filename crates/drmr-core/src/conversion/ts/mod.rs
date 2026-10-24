//! A deliberately narrow MPEG-TS input profile with independent original-media evidence.
mod aac_mp4;
mod admission;
pub(in crate::conversion) mod avc;
pub(in crate::conversion) mod canonical;
mod clock;
mod compatibility;
mod frames;
mod native;
mod preparation;
mod projection;
mod sample_aes;
mod source_admission;
use super::{
    CheckOutcome, ContentKey, ConversionError, VerificationChecks, VerificationReport,
    backend::{Backend, tool_error},
};
use crate::{inspection::TrackKind, toolrun::RunOptions};
pub(super) use admission::Admission;
pub(in crate::conversion) use canonical::Config as CanonicalConfig;
pub(super) use canonical::Native as NativeEvidence;
pub(super) use compatibility::demux_view;
pub(super) use native::NativeScanner;
pub(super) use projection::Projector;
use std::{collections::BTreeMap, path::Path, sync::Arc};

pub(super) fn requires_native_audio_mp4(native: Option<&NativeEvidence>) -> bool {
    native.is_some_and(|anchor| {
        matches!(
            anchor.config,
            canonical::Config::Audio {
                sample_rate: 44_100,
                ..
            }
        )
    })
}

#[derive(Clone, Copy)]
pub(super) struct View<'a> {
    pub input: &'a Path,
    pub metadata: &'a Path,
    pub index: usize,
    pub key: Option<&'a ContentKey>,
    pub all_keys: Option<&'a super::InputKeys>,
    pub transport: bool,
    pub native: Option<&'a NativeEvidence>,
    pub unedited: bool,
}
impl<'a> View<'a> {
    pub fn with_all_keys(mut self, keys: &'a super::InputKeys) -> Self {
        self.all_keys = Some(keys);
        self
    }
    pub(in crate::conversion) fn probe_keys(self) -> Option<super::keys::ProbeKeys<'a>> {
        if !self.transport
            && let Some(keys) = self.all_keys
        {
            return Some(super::keys::ProbeKeys::All(keys));
        }
        self.key.map(super::keys::ProbeKeys::One)
    }
}
fn require_audio_source_evidence(
    admission: &Admission,
    native: &BTreeMap<u16, Arc<NativeEvidence>>,
) -> Result<(), ConversionError> {
    if admission.tracks.len() == 1 && admission.tracks[0].kind == TrackKind::Audio {
        let id = admission.tracks[0].pid;
        if native.get(&id).is_none_or(|anchor| {
            anchor.count == 0 || !matches!(anchor.config, canonical::Config::Audio { .. })
        }) {
            return Err(ConversionError("ts-audio-only-timing-proof-not-supported"));
        }
    }
    Ok(())
}
impl Backend {
    pub fn confirm_native_transport(&self, view: View<'_>) -> Result<(), ConversionError> {
        let native = view
            .native
            .ok_or(ConversionError("ts-original-evidence-required"))?;
        self.check_cancelled()?;
        // Localization retains and guards the original resources. This anchor
        // precedes normalization and remains authoritative at every depth.
        native.validate()?;
        native.metadata()?;
        let timing = native.timing()?;
        let evidence = native.evidence()?;
        if evidence.count != timing.count
            || evidence.count != native.count
            || evidence.first_pts != timing.first_pts
            || evidence.end_pts != timing.end_pts
        {
            return Err(ConversionError("ts-original-evidence-mismatch"));
        }
        if self.validation_depth == super::ValidationDepth::Native {
            return self.check_cancelled();
        }
        let profile = self.profile_stream_keys_options(
            view.input,
            view.index,
            view.probe_keys(),
            view.unedited,
        )?;
        let config = self.transport_config(view, &profile)?;
        Self::require_native_count(&profile, native.count)?;
        let units = self.access_units(view, &profile, config.clone())?;
        if !native.matches(&config, &units) {
            return Err(ConversionError("ts-original-evidence-mismatch"));
        }
        if let canonical::Config::Audio { channels, .. } = config {
            self.native_audio_samples(view, channels, native.count)?;
        }
        Ok(())
    }
    fn require_native_count(
        profile: &serde_json::Value,
        expected: u64,
    ) -> Result<(), ConversionError> {
        if expected == 0
            || profile["nb_read_packets"]
                .as_str()
                .and_then(|s| s.parse::<u64>().ok())
                != Some(expected)
        {
            return Err(ConversionError("ts-original-access-unit-count-mismatch"));
        }
        Ok(())
    }
    fn native_audio_samples(
        &self,
        view: View<'_>,
        channels: u8,
        frames: u64,
    ) -> Result<(), ConversionError> {
        let channels = if channels == 7 { 8 } else { channels };
        let expected = frames
            .checked_mul(1024)
            .and_then(|n| n.checked_mul(u64::from(channels)))
            .and_then(|n| n.checked_mul(4))
            .ok_or(ConversionError("ts-decoded-sample-count-mismatch"))?;
        let mut argv = vec![
            self.ffmpeg.clone(),
            "-v".into(),
            "error".into(),
            "-nostdin".into(),
            "-filter_threads".into(),
            "1".into(),
            "-filter_complex_threads".into(),
            "1".into(),
            "-xerror".into(),
            "-threads".into(),
            "1".into(),
            "-protocol_whitelist".into(),
            "file,crypto".into(),
            "-i".into(),
            super::files::tool_path(view.input)?,
            "-map".into(),
            format!("0:{}", view.index),
            "-threads".into(),
            "1".into(),
            "-c:a".into(),
            "pcm_s32le".into(),
            "-f".into(),
            "s32le".into(),
            "-".into(),
        ];
        if view.unedited {
            argv.splice(1..1, ["-ignore_editlist".into(), "1".into()]);
        }
        let keyed = super::keys::KeyedArguments::new(argv, view.probe_keys());
        let secrets = keyed.secrets();
        let shown = ["ffmpeg".into(), "[arguments redacted]".into()];
        let options = RunOptions {
            stop: Some(&self.stop),
            argv_redacted: Some(&shown),
            secrets: &secrets,
            timeout: Some(self.timeout),
            max_stdout_bytes: 64 * 1024,
            max_output_chars: 0,
            log_stdout: false,
            ..RunOptions::default()
        };
        let effective = Self::effective_argv(&keyed.argv, "ffmpeg");
        let mut count = 0_u64;
        let completed = self
            .runner
            .run_tool_streamed(&effective, "ffmpeg", &options, |chunk| {
                count = count
                    .checked_add(u64::try_from(chunk.len()).map_err(|_| ())?)
                    .ok_or(())?;
                if count > expected { Err(()) } else { Ok(()) }
            })
            .map_err(|error| {
                if error.kind == "stream-invalid" {
                    ConversionError("ts-decoded-sample-count-mismatch")
                } else {
                    tool_error(&error)
                }
            })?;
        self.check_cancelled()?;
        if completed.returncode != Some(0) {
            return Err(ConversionError("media-tool-failed"));
        }
        if count != expected {
            return Err(ConversionError("ts-decoded-sample-count-mismatch"));
        }
        Ok(())
    }
    fn native_transport_streams(
        &self,
        admission: &Admission,
        native: &BTreeMap<u16, Arc<NativeEvidence>>,
    ) -> Result<Vec<(usize, TrackKind)>, ConversionError> {
        self.check_cancelled()?;
        if native.len() != admission.tracks.len() {
            return Err(ConversionError("ts-stream-count-mismatch"));
        }
        let mut streams = Vec::new();
        for (index, track) in admission.tracks.iter().enumerate() {
            self.check_cancelled()?;
            let anchor = native
                .get(&track.pid)
                .ok_or(ConversionError("ts-original-evidence-required"))?;
            anchor.validate()?;
            let metadata = anchor.metadata()?;
            let timing = anchor.timing()?;
            let evidence = anchor.evidence()?;
            let kind_matches = matches!(
                (track.kind, metadata),
                (TrackKind::Video, canonical::TrackMetadata::Video(_))
                    | (TrackKind::Audio, canonical::TrackMetadata::Audio { .. })
            );
            if !kind_matches || timing.count != evidence.count || anchor.count != evidence.count {
                return Err(ConversionError("invalid-ts-streams"));
            }
            streams.push((index, track.kind));
        }
        Ok(streams)
    }
    pub fn transport_streams(
        &self,
        path: &Path,
        admission: &Admission,
        native: &BTreeMap<u16, Arc<NativeEvidence>>,
    ) -> Result<Vec<(usize, TrackKind)>, ConversionError> {
        admission.finish()?;
        // Discovery requires original access-unit evidence. Complete decode is
        // an additional requested validation depth.
        require_audio_source_evidence(admission, native)?;
        if self.validation_depth == super::ValidationDepth::Native {
            return self.native_transport_streams(admission, native);
        }
        let argv = vec![
            self.ffprobe.clone(),
            "-v".into(),
            "error".into(),
            "-protocol_whitelist".into(),
            "file,crypto".into(),
            "-show_entries".into(),
            "stream=index,id,codec_name,codec_type".into(),
            "-of".into(),
            "json".into(),
            super::files::tool_path(path)?,
        ];
        let bytes = self.run(&argv, "ffprobe", &[])?;
        if bytes.len() > 65536 {
            return Err(ConversionError("ts-stream-count-mismatch"));
        }
        let data: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| ConversionError("invalid-ts-streams"))?;
        let streams = data["streams"]
            .as_array()
            .ok_or(ConversionError("invalid-ts-streams"))?;
        if streams.len() != admission.tracks.len() {
            return Err(ConversionError("ts-stream-count-mismatch"));
        }
        let mut result = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (index, stream) in streams.iter().enumerate() {
            let id = stream["id"]
                .as_str()
                .and_then(|s| s.strip_prefix("0x"))
                .and_then(|s| u16::from_str_radix(s, 16).ok())
                .ok_or(ConversionError("invalid-ts-streams"))?;
            let track = admission
                .tracks
                .iter()
                .find(|t| t.pid == id)
                .ok_or(ConversionError("ts-stream-count-mismatch"))?;
            if !seen.insert(id)
                || stream["index"].as_u64() != Some(index as u64)
                || !match track.kind {
                    TrackKind::Video => {
                        stream["codec_type"] == "video" && stream["codec_name"] == "h264"
                    }
                    TrackKind::Audio => {
                        stream["codec_type"] == "audio" && stream["codec_name"] == "aac"
                    }
                    _ => false,
                }
            {
                return Err(ConversionError("invalid-ts-streams"));
            }
            result.push((index, track.kind));
        }
        Ok(result)
    }
    pub(super) fn audio_evidence(
        &self,
        view: View<'_>,
    ) -> Result<super::audio_presentation::AudioEvidence, ConversionError> {
        if self.validation_depth == super::ValidationDepth::Native {
            return self.native_audio_evidence(view);
        }
        let profile = self.profile_stream_keys_options(
            view.input,
            view.index,
            view.probe_keys(),
            view.unedited,
        )?;
        let config = self.transport_config(view, &profile)?;
        let canonical::Config::Audio { sample_rate, .. } = config else {
            return Err(super::audio_presentation::MISMATCH);
        };
        let units = self.access_units(view, &profile, config)?;
        Ok(super::audio_presentation::AudioEvidence {
            sample_rate,
            coded_frames: units.count,
            raw_first_pts: units.first_pts,
            raw_coded_end: units.end_pts,
        })
    }
    pub fn transport_durations(
        &self,
        segments: &[(std::path::PathBuf, String)],
        index: usize,
        maximum_workers: Option<usize>,
    ) -> Result<Option<super::audio_presentation::AudioPresentation>, ConversionError> {
        let bad = ConversionError("ts-segment-duration-mismatch");
        let mut previous: Option<(&str, canonical::Evidence, u32)> = None;
        let mut whole: Option<super::audio_presentation::AudioEvidence> = None;
        let batch = preparation::Batch::new(self, segments.len(), maximum_workers)?;
        for segments in segments.chunks(preparation::BATCH) {
            let evidence = batch.collect(segments.len(), |ordinal| {
                let path = &segments[ordinal].0;
                let view = View {
                    input: path,
                    metadata: path,
                    index,
                    key: None,
                    all_keys: None,
                    transport: true,
                    native: None,
                    unedited: false,
                };
                let profile = self.profile_stream(path, index, None)?;
                let config = self.transport_config(view, &profile)?;
                let units = self.access_units(view, &profile, config.clone())?;
                let rate = match config {
                    canonical::Config::Audio { sample_rate, .. } => Some(sample_rate),
                    canonical::Config::Video { .. } => None,
                };
                Ok((units, rate))
            })?;
            for ((_, declared), (units, rate)) in segments.iter().zip(evidence) {
                if let Some((old, observed, rate)) = previous.take() {
                    let boundary = check_duration(
                        old,
                        units.first_pts.checked_sub(observed.first_pts).ok_or(bad)?,
                    );
                    if boundary.is_err()
                        && check_duration(
                            old,
                            observed
                                .end_pts
                                .checked_sub(observed.first_pts)
                                .ok_or(bad)?,
                        )
                        .is_err()
                    {
                        super::audio_presentation::check_sample_duration(
                            old,
                            observed.count.checked_mul(1024).ok_or(bad)?,
                            rate,
                        )?;
                    }
                }
                if let Some(sample_rate) = rate {
                    if let Some(all) = &mut whole {
                        if all.sample_rate != sample_rate {
                            return Err(bad);
                        }
                        all.coded_frames = all.coded_frames.checked_add(units.count).ok_or(bad)?;
                        all.raw_coded_end = units.end_pts;
                    } else {
                        whole = Some(super::audio_presentation::AudioEvidence {
                            sample_rate,
                            coded_frames: units.count,
                            raw_first_pts: units.first_pts,
                            raw_coded_end: units.end_pts,
                        });
                    }
                    previous = Some((declared, units, sample_rate));
                } else {
                    check_duration(
                        declared,
                        units.end_pts.checked_sub(units.first_pts).ok_or(bad)?,
                    )?;
                }
            }
        }
        if let Some((declared, units, rate)) = previous {
            let ticks = units.end_pts.checked_sub(units.first_pts).ok_or(bad)?;
            let full = if rate == 44_100 {
                check_final_audio_duration(declared, ticks, units.count)
            } else {
                check_duration(declared, ticks).or_else(|_| {
                    super::audio_presentation::check_sample_duration(
                        declared,
                        units.count.checked_mul(1024).ok_or(bad)?,
                        rate,
                    )
                })
            };
            if full.is_err() {
                let presentation = super::audio_presentation::AudioPresentation::from_final(
                    whole.ok_or(bad)?,
                    units.count,
                    declared,
                )?;
                batch.finish()?;
                return Ok(Some(presentation));
            }
        }
        batch.finish()?;
        Ok(None)
    }
    pub fn demux_transport(
        &self,
        input: &Path,
        kind: TrackKind,
        output: &Path,
        native: Option<&NativeEvidence>,
    ) -> Result<(), ConversionError> {
        let input = super::files::tool_path(input)?;
        let output = super::files::tool_path(output)?;
        if input.contains([',', '\n', '\r']) || output.contains([',', '\n', '\r']) {
            return Err(ConversionError("unsupported-path"));
        }
        if kind == TrackKind::Audio && requires_native_audio_mp4(native) {
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
                    crate::resources::ReservationError::Insufficient => {
                        ConversionError("resource-budget")
                    }
                    crate::resources::ReservationError::Cancelled => ConversionError("cancelled"),
                    crate::resources::ReservationError::Timeout => {
                        ConversionError("media-tool-timeout")
                    }
                })?;
            aac_mp4::write(
                Path::new(&input),
                Path::new(&output),
                native.ok_or(ConversionError("ts-original-evidence-required"))?,
                &self.stop,
            )?;
            return self.check_cancelled();
        }
        let kind = match kind {
            TrackKind::Video => "video",
            TrackKind::Audio => "audio",
            _ => return Err(ConversionError("ts-stream-type-not-supported")),
        };
        self.run(
            &[
                self.packager.clone(),
                "--mp4_reset_initial_composition_offset_to_zero=false".into(),
                format!("input={input},stream={kind},output={output}"),
                "--clear_lead".into(),
                "0".into(),
            ],
            "packager",
            &[],
        )?;
        super::private_file_mode(Path::new(&output))
    }
    fn transport_config(
        &self,
        view: View<'_>,
        profile: &serde_json::Value,
    ) -> Result<canonical::Config, ConversionError> {
        if profile["time_base"] != "1/90000" {
            return Err(ConversionError("ts-clock-timescale-not-supported"));
        }
        match profile["codec_name"].as_str() {
            Some("aac") if view.transport => {
                let sample_rate = match profile["sample_rate"].as_str() {
                    Some("44100") => 44_100,
                    Some("48000") => 48_000,
                    _ => return Err(ConversionError("ts-aac-sample-rate-not-supported")),
                };
                let channels = match profile["channels"].as_u64() {
                    Some(n @ 1..=6) => u8::try_from(n)
                        .map_err(|_| ConversionError("ts-codec-configuration-not-supported"))?,
                    Some(8) => 7,
                    _ => return Err(ConversionError("ts-codec-configuration-not-supported")),
                };
                Ok(canonical::Config::Audio {
                    channels,
                    sample_rate,
                })
            }
            Some("h264" | "aac") => {
                let argv = vec![
                    self.ffprobe.clone(),
                    "-v".into(),
                    "error".into(),
                    "-protocol_whitelist".into(),
                    "file,crypto".into(),
                    "-select_streams".into(),
                    view.index.to_string(),
                    "-show_entries".into(),
                    "stream=extradata".into(),
                    "-show_data".into(),
                    "-of".into(),
                    "json".into(),
                    super::files::tool_path(view.metadata)?,
                ];
                let keyed = super::keys::KeyedArguments::new(argv, view.probe_keys());
                let secrets = keyed.secrets();
                let bytes = self.run(&keyed.argv, "ffprobe", &secrets)?;
                if bytes.len() > 1024 * 1024 {
                    return Err(ConversionError("ts-codec-configuration-not-supported"));
                }
                let json: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|_| ConversionError("invalid-ts-access-unit-evidence"))?;
                let streams = json["streams"]
                    .as_array()
                    .filter(|s| s.len() == 1)
                    .ok_or(ConversionError("invalid-ts-access-unit-evidence"))?;
                let data = streams[0]["extradata"]
                    .as_str()
                    .ok_or(ConversionError("ts-codec-configuration-not-supported"))?;
                let bytes = canonical::dump(data.as_bytes(), false)?;
                if profile["codec_name"] == "h264" {
                    canonical::Config::video(&bytes, view.transport)
                } else {
                    canonical::Config::audio(&bytes)
                }
            }
            _ => Err(ConversionError("ts-codec-configuration-not-supported")),
        }
    }
    fn access_units(
        &self,
        view: View<'_>,
        profile: &serde_json::Value,
        config: canonical::Config,
    ) -> Result<canonical::Evidence, ConversionError> {
        let nal_length_size =
            if !view.transport && matches!(config, canonical::Config::Video { .. }) {
                profile["nal_length_size"]
                    .as_str()
                    .and_then(|value| value.parse::<u8>().ok())
                    .filter(|width| matches!(width, 1 | 2 | 4))
                    .ok_or(ConversionError("ts-codec-configuration-not-supported"))?
            } else {
                0
            };
        let expected = profile["nb_read_packets"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(ConversionError("media-packets-missing"))?;
        let mut argv = vec![
            self.ffprobe.clone(),
            "-v".into(),
            "error".into(),
            "-protocol_whitelist".into(),
            "file,crypto".into(),
            "-select_streams".into(),
            view.index.to_string(),
            "-show_packets".into(),
            "-show_data".into(),
            "-show_entries".into(),
            "packet=stream_index,pts,dts,duration,data:packet_side_data=".into(),
            "-of".into(),
            "compact=p=1:nk=0".into(),
        ];
        if view.unedited {
            argv.splice(1..1, ["-ignore_editlist".into(), "1".into()]);
        }
        argv.push(super::files::tool_path(view.input)?);
        let keyed = super::keys::KeyedArguments::new(argv, view.probe_keys());
        let effective = Backend::effective_argv(&keyed.argv, "ffprobe");
        let shown = ["ffprobe".into(), "[arguments redacted]".into()];
        let secrets = keyed.secrets();
        let options = RunOptions {
            stop: Some(&self.stop),
            argv_redacted: Some(&shown),
            secrets: &secrets,
            timeout: Some(self.timeout),
            max_stdout_bytes: canonical::MEMORY,
            max_output_chars: 0,
            log_stdout: false,
            ..RunOptions::default()
        };
        let mut parser = canonical::Parser::new(
            view.index,
            expected,
            view.transport,
            config,
            nal_length_size,
        );
        let completed = self
            .runner
            .run_tool_streamed(&effective, "ffprobe", &options, |chunk| parser.push(chunk))
            .map_err(|error| {
                if error.kind == "stream-invalid" {
                    parser
                        .error()
                        .unwrap_or(ConversionError("invalid-ts-access-unit-evidence"))
                } else {
                    tool_error(&error)
                }
            })?;
        self.check_cancelled()?;
        if completed.returncode != Some(0) {
            return Err(ConversionError("media-tool-failed"));
        }
        parser.finish()
    }

    fn progressive_transport_frames(
        &self,
        view: View<'_>,
        profile: &serde_json::Value,
    ) -> Result<frames::Evidence, ConversionError> {
        let expected = profile["nb_read_packets"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(ConversionError("media-packets-missing"))?;
        let mut argv = vec![
            self.ffprobe.clone(),
            "-v".into(),
            "error".into(),
            "-protocol_whitelist".into(),
            "file,crypto".into(),
            "-select_streams".into(),
            view.index.to_string(),
            "-show_frames".into(),
            "-show_entries".into(),
            "frame=interlaced_frame,top_field_first:frame_side_data=".into(),
            "-of".into(),
            "compact=p=1:nk=0".into(),
        ];
        argv.push(super::files::tool_path(view.input)?);
        let keyed = super::keys::KeyedArguments::new(argv, view.probe_keys());
        let effective = Self::effective_argv(&keyed.argv, "ffprobe");
        let shown = ["ffprobe".into(), "[arguments redacted]".into()];
        let secrets = keyed.secrets();
        let options = RunOptions {
            stop: Some(&self.stop),
            argv_redacted: Some(&shown),
            secrets: &secrets,
            timeout: Some(self.timeout),
            max_stdout_bytes: 16 * 1024,
            max_output_chars: 0,
            log_stdout: false,
            ..RunOptions::default()
        };
        let mut parser = frames::Parser::new(expected);
        let completed = self
            .runner
            .run_tool_streamed(&effective, "ffprobe", &options, |chunk| parser.push(chunk))
            .map_err(|error| {
                if error.kind == "stream-invalid" {
                    ConversionError("ts-progressive-frame-evidence-required")
                } else {
                    tool_error(&error)
                }
            })?;
        self.check_cancelled()?;
        if completed.returncode != Some(0) {
            return Err(ConversionError("media-tool-failed"));
        }
        parser.finish()
    }
    pub fn verify_transport(
        &self,
        source: View<'_>,
        result: View<'_>,
    ) -> Result<VerificationReport, ConversionError> {
        let _scope = self.scope(super::measurement::Phase::Verification);
        if self.validation_depth == super::ValidationDepth::Native {
            return self.native_verify_transport(source, result);
        }
        let mut a = self.profile_stream_keys_options(
            source.input,
            source.index,
            source.probe_keys(),
            source.unedited,
        )?;
        let mut b = self.profile_stream_keys_options(
            result.metadata,
            result.index,
            result.probe_keys(),
            result.unedited,
        )?;
        let config_a = self.transport_config(source, &a)?;
        let config_b = self.transport_config(result, &b)?;
        let units_a = self.access_units(source, &a, config_a.clone())?;
        let units_b = self.access_units(result, &b, config_b.clone())?;
        if let Some(native) = source.native {
            Self::require_native_count(&a, native.count)?;
            Self::require_native_count(&b, native.count)?;
            if !native.matches(&config_a, &units_a) {
                return Err(ConversionError("ts-original-evidence-mismatch"));
            }
            if let canonical::Config::Audio { channels, .. } = config_a {
                self.native_audio_samples(source, channels, native.count)?;
                self.native_audio_samples(result, channels, native.count)?;
            }
        }
        // Codec configuration has its own exact, format-aware check above. All other fields remain exact.
        for profile in [&mut a, &mut b] {
            let profile = profile
                .as_object_mut()
                .ok_or(ConversionError("invalid-media-probe"))?;
            profile.remove("extradata_hash");
            profile.remove("nal_length_size");
        }
        if matches!(config_a, canonical::Config::Video { .. }) {
            let source_frames = self.progressive_transport_frames(source, &a)?;
            let result_frames = self.progressive_transport_frames(result, &b)?;
            source_frames.resolve_field_order(&mut a);
            result_frames.resolve_field_order(&mut b);
        }
        let metadata = a == b && config_a == config_b;
        let timing = if matches!(
            config_a,
            canonical::Config::Audio {
                sample_rate: 44_100,
                ..
            }
        ) {
            // TS probes report a nominal, independently rounded AAC duration.
            // MP4 stores the actual next-DTS difference. Compare every original
            // absolute PTS/DTS and the final end, without a time tolerance.
            units_a.timing_equal(&units_b)
        } else {
            self.packets_stream_keys_options(
                source.input,
                source.index,
                source.probe_keys(),
                source.unedited,
            )? == self.packets_stream_keys_options(
                result.input,
                result.index,
                result.probe_keys(),
                result.unedited,
            )?
        };
        let payload = units_a.payload_equal(&units_b);
        let decoded = self.hash_stream_keys_options(
            source.input,
            source.index,
            false,
            source.probe_keys(),
            source.unedited,
        )? == self.hash_stream_keys_options(
            result.input,
            result.index,
            false,
            result.probe_keys(),
            result.unedited,
        )?;
        Ok(VerificationReport::new(
            self.validation_depth,
            super::VerificationScope::Comparison,
            VerificationChecks {
                stream_metadata: if metadata {
                    CheckOutcome::Pass
                } else {
                    CheckOutcome::Fail
                },
                packet_timing_equal: timing,
                packet_payloads_equal: payload,
            },
            super::DecodeEvidence::ContentCompared { equal: decoded },
        ))
    }
}

pub(in crate::conversion) fn check_duration(
    declared: &str,
    ticks: i64,
) -> Result<(), ConversionError> {
    let ticks =
        u128::try_from(ticks).map_err(|_| ConversionError("ts-segment-duration-mismatch"))?;
    check_duration_ratio(declared, ticks, 90_000)
}

pub(in crate::conversion) fn check_final_audio_duration(
    declared: &str,
    ticks: i64,
    frames: u64,
) -> Result<(), ConversionError> {
    // A final 44.1 kHz frame has no following PES timestamp. A publisher can
    // express its nominal duration in truncated or nearest 90 kHz ticks, or
    // express the complete AAC sample span directly. Match only those exact
    // representations at the precision actually written in EXTINF.
    if check_duration(declared, ticks).is_ok()
        || check_duration(
            declared,
            ticks
                .checked_add(1)
                .ok_or(ConversionError("ts-segment-duration-mismatch"))?,
        )
        .is_ok()
    {
        return Ok(());
    }
    super::audio_presentation::check_sample_duration(
        declared,
        frames
            .checked_mul(1024)
            .ok_or(ConversionError("ts-segment-duration-mismatch"))?,
        44_100,
    )
}

pub(super) fn check_duration_ratio(
    declared: &str,
    numerator: u128,
    denominator: u32,
) -> Result<(), ConversionError> {
    let bad = ConversionError("ts-segment-duration-mismatch");
    let (whole, part) = declared.split_once('.').unwrap_or((declared, ""));
    if whole.is_empty()
        || declared.len() > 64
        || !whole
            .bytes()
            .chain(part.bytes())
            .all(|b| b.is_ascii_digit())
        || numerator == 0
        || denominator == 0
    {
        return Err(bad);
    }
    if part.len() > 9 {
        // Long decimals commonly serialize a binary64 segment duration. Require
        // exact binary64 identity with native ticks, without a time tolerance.
        // The bounded integer conversion is exact and keeps each ULP far below
        // a microsecond; media clocks themselves remain integer evidence.
        let numerator = u32::try_from(numerator).map_err(|_| bad)?;
        let observed = declared.parse::<f64>().map_err(|_| bad)?;
        let expected = f64::from(numerator) / f64::from(denominator);
        return if observed.to_bits() == expected.to_bits() {
            Ok(())
        } else {
            Err(bad)
        };
    }
    let scale = 10_u128.pow(u32::try_from(part.len()).map_err(|_| bad)?);
    let fraction = if part.is_empty() {
        0
    } else {
        part.parse::<u128>().map_err(|_| bad)?
    };
    let value = whole
        .parse::<u128>()
        .ok()
        .and_then(|n| n.checked_mul(scale))
        .and_then(|n| n.checked_add(fraction))
        .ok_or(bad)?;
    // Round only to the exact precision declared by EXTINF, with no fixed time tolerance.
    let denominator = u128::from(denominator);
    let measured = numerator
        .checked_mul(scale)
        .and_then(|value| value.checked_add(denominator / 2))
        .ok_or(bad)?
        / denominator;
    if measured != value {
        return Err(bad);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_keep_every_mp4_key_without_changing_original_transport_key_selection() {
        use super::super::keys::ProbeKeys;
        let keys = super::super::InputKeys::new(vec![
            ContentKey::from_hex(
                "000102030405060708090a0b0c0d0e0f",
                "101112131415161718191a1b1c1d1e1f",
            )
            .unwrap(),
            ContentKey::from_hex(
                "303132333435363738393a3b3c3d3e3f",
                "202122232425262728292a2b2c2d2e2f",
            )
            .unwrap(),
        ])
        .unwrap();
        let dictionary = &keys;
        let selected = &keys.keys[0];
        let view = View {
            input: Path::new("fixture.mp4"),
            metadata: Path::new("fixture.mp4"),
            index: 0,
            key: Some(selected),
            all_keys: None,
            transport: false,
            native: None,
            unedited: true,
        };
        assert!(
            matches!(view.probe_keys(), Some(ProbeKeys::One(key)) if std::ptr::eq(key, selected))
        );
        let all = view.with_all_keys(dictionary);
        assert!(
            matches!(all.probe_keys(), Some(ProbeKeys::All(value)) if std::ptr::eq(value, dictionary))
        );
        assert!(std::ptr::eq(all.key.unwrap(), selected));
        let transport = View {
            transport: true,
            ..all
        };
        assert!(
            matches!(transport.probe_keys(), Some(ProbeKeys::One(key)) if std::ptr::eq(key, selected))
        );
        let clear_transport = View {
            key: None,
            ..transport
        };
        assert!(clear_transport.probe_keys().is_none());
        let unselected = View { key: None, ..all };
        assert!(
            matches!(unselected.probe_keys(), Some(ProbeKeys::All(value)) if std::ptr::eq(value, dictionary))
        );
    }

    #[test]
    fn fractional_aac_duration_uses_the_complete_sample_span() {
        check_duration_ratio("0.023219954648526078", 1024, 44_100).unwrap();
        for (text, frames) in [("0.023220", 1_u64), ("1.996916", 86), ("5.967528", 257)] {
            check_duration_ratio(text, u128::from(frames) * 1024, 44_100).unwrap();
        }
        for text in ["0.023211", "0.023220", "0.023222"] {
            check_final_audio_duration(text, 2089, 1).unwrap();
        }
        for text in ["0.023200", "0.023233", "0.046440"] {
            assert!(check_final_audio_duration(text, 2089, 1).is_err());
        }
        for text in ["1.996917", "1.996915", "2.000000", "NaN", "0"] {
            assert!(check_duration_ratio(text, 86 * 1024, 44_100).is_err());
        }
    }

    #[test]
    fn duration_uses_only_the_declared_decimal_precision() {
        for (text, ticks) in [("2.000000", 180_000), ("2.042", 183_750), ("2", 180_001)] {
            check_duration(text, ticks).unwrap();
        }
        for (text, ticks) in [
            ("2.000000", 180_001),
            ("2.041", 183_750),
            ("NaN", 180_000),
            ("-2", 180_000),
            ("2", 0),
        ] {
            assert_eq!(
                check_duration(text, ticks).unwrap_err().0,
                "ts-segment-duration-mismatch"
            );
        }
    }
    #[test]
    fn long_decimal_durations_require_the_exact_binary64_value_of_the_original_clock() {
        for text in [
            "5.994666666666666",
            "5.9946666666666664",
            "5.99466666666666637",
        ] {
            check_duration(text, 539_520).unwrap();
        }
        check_duration("2.0000000000", 180_000).unwrap();
        check_duration("2.000000000000000000", 180_000).unwrap();
        assert!(check_duration(&format!("2.{}", "0".repeat(63)), 180_000).is_err());
        for (text, ticks) in [
            ("5.994666666666667", 539_520),
            ("5.994667666666666", 539_520),
            ("5.994666666666666", 539_521),
            ("2.0000000001", 180_000),
            ("2.00000000000000000junk", 180_000),
            ("2.0000000000000000e0", 180_000),
            ("NaN", 180_000),
            ("inf", 180_000),
            (
                "340282366920938463463374607431768211455.00000000000000000",
                180_000,
            ),
            ("100000000000000000000.00000000000000000", 180_000),
            ("50000.00000000000000000", 4_500_000_000),
        ] {
            assert_eq!(
                check_duration(text, ticks).unwrap_err().0,
                "ts-segment-duration-mismatch",
                "accepted {text}"
            );
        }
    }
}
