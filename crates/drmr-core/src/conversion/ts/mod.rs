//! A deliberately narrow MPEG-TS input profile with independent original-media evidence.
mod admission;
mod canonical;
mod frames;
mod native;
mod projection;
mod sample_aes;
use super::{
    CheckOutcome, ContentKey, ConversionError, VerificationChecks, VerificationReport,
    backend::{Backend, tool_error},
};
use crate::{inspection::TrackKind, toolrun::RunOptions};
pub(super) use admission::Admission;
pub(super) use canonical::Native as NativeEvidence;
pub(super) use native::NativeScanner;
pub(super) use projection::Projector;
use std::{collections::BTreeMap, path::Path, sync::Arc};
use zeroize::Zeroizing;

#[derive(Clone, Copy)]
pub(super) struct View<'a> {
    pub input: &'a Path,
    pub metadata: &'a Path,
    pub index: usize,
    pub key: Option<&'a ContentKey>,
    pub transport: bool,
    pub native: Option<&'a NativeEvidence>,
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
        let profile = self.profile_stream(view.input, view.index, view.key)?;
        let config = self.transport_config(view, &profile)?;
        Self::require_native_count(&profile, native.count)?;
        let units = self.access_units(view, &profile, config.clone())?;
        if !native.matches(&config, &units) {
            return Err(ConversionError("ts-original-evidence-mismatch"));
        }
        if let canonical::Config::Audio { channels } = config {
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
        let argv = vec![
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
        let keyed =
            super::keys::KeyedArguments::new(argv, view.key.map(super::keys::ProbeKeys::One));
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
    pub fn transport_streams(
        &self,
        path: &Path,
        admission: &Admission,
        native: &BTreeMap<u16, Arc<NativeEvidence>>,
    ) -> Result<Vec<(usize, TrackKind)>, ConversionError> {
        admission.finish()?;
        // Discovery is evidence-gated; the caller still binds every actual source
        // packet and decoded sample before demuxing or normalizing the track.
        require_audio_source_evidence(admission, native)?;
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
    pub fn transport_durations(
        &self,
        segments: &[(std::path::PathBuf, String)],
        index: usize,
    ) -> Result<(), ConversionError> {
        for (path, declared) in segments {
            let view = View {
                input: path,
                metadata: path,
                index,
                key: None,
                transport: true,
                native: None,
            };
            let profile = self.profile_stream(path, index, None)?;
            let config = self.transport_config(view, &profile)?;
            let units = self.access_units(view, &profile, config)?;
            check_duration(
                declared,
                units
                    .end_pts
                    .checked_sub(units.first_pts)
                    .ok_or(ConversionError("ts-segment-duration-mismatch"))?,
            )?;
        }
        Ok(())
    }
    pub fn demux_transport(
        &self,
        input: &Path,
        kind: TrackKind,
        output: &Path,
    ) -> Result<(), ConversionError> {
        let input = super::files::tool_path(input)?;
        let output = super::files::tool_path(output)?;
        if input.contains([',', '\n', '\r']) || output.contains([',', '\n', '\r']) {
            return Err(ConversionError("unsupported-path"));
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
                if profile["sample_rate"] != "48000" {
                    return Err(ConversionError("ts-aac-sample-rate-not-supported"));
                }
                let channels = match profile["channels"].as_u64() {
                    Some(n @ 1..=6) => u8::try_from(n)
                        .map_err(|_| ConversionError("ts-codec-configuration-not-supported"))?,
                    Some(8) => 7,
                    _ => return Err(ConversionError("ts-codec-configuration-not-supported")),
                };
                Ok(canonical::Config::Audio { channels })
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
                let bytes = self.run(&argv, "ffprobe", &[])?;
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
        let expected = profile["nb_read_packets"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .ok_or(ConversionError("media-packets-missing"))?;
        let mut argv = Zeroizing::new(vec![
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
        ]);
        let secret = view.key.map(ContentKey::key_hex);
        if let Some(key) = &secret {
            argv.extend(["-decryption_key".into(), key.to_string()]);
        }
        argv.push(super::files::tool_path(view.input)?);
        let effective = Backend::effective_argv(&argv, "ffprobe");
        let shown = ["ffprobe".into(), "[arguments redacted]".into()];
        let secrets: Vec<_> = secret.iter().map(|s| s.as_str()).collect();
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
        let mut parser = canonical::Parser::new(view.index, expected, view.transport, config);
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
        let mut argv = Zeroizing::new(vec![
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
        ]);
        let secret = view.key.map(ContentKey::key_hex);
        if let Some(key) = &secret {
            argv.extend(["-decryption_key".into(), key.to_string()]);
        }
        argv.push(super::files::tool_path(view.input)?);
        let effective = Self::effective_argv(&argv, "ffprobe");
        let shown = ["ffprobe".into(), "[arguments redacted]".into()];
        let secrets: Vec<_> = secret.iter().map(|s| s.as_str()).collect();
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
        let mut a = self.profile_stream(source.input, source.index, source.key)?;
        let mut b = self.profile_stream(result.metadata, result.index, result.key)?;
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
            if let canonical::Config::Audio { channels } = config_a {
                self.native_audio_samples(source, channels, native.count)?;
                self.native_audio_samples(result, channels, native.count)?;
            }
        }
        // Codec configuration has its own exact, format-aware check above. All other fields remain exact.
        a.as_object_mut()
            .ok_or(ConversionError("invalid-media-probe"))?
            .remove("extradata_hash");
        b.as_object_mut()
            .ok_or(ConversionError("invalid-media-probe"))?
            .remove("extradata_hash");
        if matches!(config_a, canonical::Config::Video { .. }) {
            let source_frames = self.progressive_transport_frames(source, &a)?;
            let result_frames = self.progressive_transport_frames(result, &b)?;
            source_frames.resolve_field_order(&mut a);
            result_frames.resolve_field_order(&mut b);
        }
        let metadata = a == b && config_a == config_b;
        let timing = self.packets_stream(source.input, source.index, source.key)?
            == self.packets_stream(result.input, result.index, result.key)?;
        let payload = units_a.payload_equal(&units_b);
        let decoded = self.hash_stream(source.input, source.index, false, source.key)?
            == self.hash_stream(result.input, result.index, false, result.key)?;
        let passed = metadata && timing && payload && decoded;

        Ok(VerificationReport {
            schema_version: 1,
            passed,
            media_verified: passed,
            license_playback_verified: false,
            checks: VerificationChecks {
                stream_metadata: if metadata {
                    CheckOutcome::Pass
                } else {
                    CheckOutcome::Fail
                },
                packet_timing_equal: timing,
                packet_payloads_equal: payload,
                decoded_content_equal: decoded,
            },
        })
    }
}

fn check_duration(declared: &str, ticks: i64) -> Result<(), ConversionError> {
    let bad = ConversionError("ts-segment-duration-mismatch");
    let (whole, part) = declared.split_once('.').unwrap_or((declared, ""));
    if whole.is_empty()
        || part.len() > 9
        || !whole
            .bytes()
            .chain(part.bytes())
            .all(|b| b.is_ascii_digit())
        || ticks <= 0
    {
        return Err(bad);
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
    let measured = (u128::try_from(ticks).map_err(|_| bad)? * scale + 45000) / 90000;
    if measured != value {
        return Err(bad);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_uses_only_the_declared_decimal_precision() {
        for (text, ticks) in [("2.000000", 180_000), ("2.042", 183_750), ("2", 180_001)] {
            check_duration(text, ticks).unwrap();
        }
        for (text, ticks) in [
            ("2.000000", 180_001),
            ("2.041", 183_750),
            ("2.0000000000", 180_000),
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
}
