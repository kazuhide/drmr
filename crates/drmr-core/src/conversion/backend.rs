//! Backend command construction. Retained records contain neither raw arguments nor tool output.
use super::{ContentKey, ConversionError, InputKeys, OutputKey};
use crate::{
    inspection::TargetProtection,
    toolrun::{RunOptions, ToolRunner},
};
use std::{fmt::Write as _, path::Path, sync::Arc, time::Duration};
use zeroize::Zeroizing;

pub(super) struct Backend {
    pub runner: Arc<ToolRunner>,
    pub packager: String,
    pub ffmpeg: String,
    pub ffprobe: String,
    pub timeout: Duration,
}
impl Backend {
    pub fn run(
        &self,
        argv: &[String],
        tool: &str,
        secrets: &[&str],
    ) -> Result<Vec<u8>, ConversionError> {
        let mut effective = Zeroizing::new(argv.to_vec());
        if matches!(tool, "ffmpeg" | "ffprobe") {
            let input = effective
                .iter()
                .position(|value| value == "-i")
                .map_or_else(|| effective.len() - 1, |index| index + 1);
            if effective.get(input).is_some_and(|value| {
                Path::new(value)
                    .extension()
                    .is_some_and(|extension| extension == "m3u8")
            }) {
                let at = if tool == "ffmpeg" { input - 1 } else { input };
                effective.splice(at..at, ["-allowed_extensions".into(), "ALL".into()]);
            }
        }
        let shown = vec![tool.to_string(), "[arguments redacted]".into()];
        let options = RunOptions {
            argv_redacted: Some(&shown),
            secrets,
            timeout: Some(self.timeout),
            max_output_chars: 0,
            max_stdout_bytes: if tool == "ffprobe"
                && effective.iter().any(|arg| arg == "-show_packets")
            {
                32 * 1024 * 1024
            } else {
                RunOptions::default().max_stdout_bytes
            },
            log_stdout: false,
            ..RunOptions::default()
        };
        let completed = self
            .runner
            .run_tool(&effective, tool, &options)
            .map_err(|_| ConversionError("media-tool-unavailable-or-timeout"))?;
        if completed.returncode != Some(0) {
            return Err(ConversionError("media-tool-failed"));
        }
        Ok(completed.stdout)
    }
    pub fn package(
        &self,
        input: &Path,
        output: &Path,
        decryption: Option<&InputKeys>,
        encryption: Option<(&OutputKey, TargetProtection)>,
    ) -> Result<(), ConversionError> {
        self.package_stream(input, 0, output, decryption, encryption)
    }
    pub fn package_stream(
        &self,
        input: &Path,
        index: usize,
        output: &Path,
        decryption: Option<&InputKeys>,
        encryption: Option<(&OutputKey, TargetProtection)>,
    ) -> Result<(), ConversionError> {
        super::audio::require_supported_audio(input, index)?;
        let input = super::files::tool_path(input)?;
        let output = super::files::tool_path(output)?;
        if input.contains([',', '\n', '\r']) || output.contains([',', '\n', '\r']) {
            return Err(ConversionError("unsupported-path"));
        }
        let mut argv = Zeroizing::new(vec![
            self.packager.clone(),
            "--mp4_reset_initial_composition_offset_to_zero=false".into(),
            format!("input={input},stream={index},output={output}"),
            "--clear_lead".into(),
            "0".into(),
        ]);
        let material: Vec<&ContentKey> = if let Some(keys) = decryption {
            argv.push("--enable_raw_key_decryption".into());
            keys.keys.iter().collect()
        } else if let Some((key, target)) = encryption {
            argv.extend([
                "--enable_raw_key_encryption".into(),
                "--protection_scheme".into(),
                target.as_str().into(),
            ]);
            vec![&key.0]
        } else {
            Vec::new()
        };
        let key_hex: Vec<_> = material.iter().map(|key| key.key_hex()).collect();
        if !material.is_empty() {
            let mut value = Zeroizing::new(String::new());
            for (index, key) in material.iter().enumerate() {
                if index > 0 {
                    value.push(',');
                }
                write!(
                    value,
                    "label={}:key_id={}:key=",
                    if encryption.is_some() {
                        String::new()
                    } else {
                        index.to_string()
                    },
                    key.id_hex()
                )
                .expect("writing a string cannot fail");
                value.push_str(&key_hex[index]);
            }
            argv.push("--keys".into());
            argv.push(std::mem::take(&mut *value));
        }
        let secrets: Vec<_> = key_hex.iter().map(|value| value.as_str()).collect();
        self.run(&argv, "packager", &secrets)?;
        super::private_file_mode(Path::new(&output))?;
        Ok(())
    }
    pub fn package_hls(&self, input: &Path, output: &Path) -> Result<(), ConversionError> {
        super::audio::require_supported_audio(input, 0)?;
        let input = super::files::tool_path(input)?;
        let output = super::files::tool_path(output)?;
        if input.contains(',') || output.contains(',') {
            return Err(ConversionError("unsupported-path"));
        }
        let argv = vec![
            self.packager.clone(),
            "--mp4_reset_initial_composition_offset_to_zero=false".into(),
            format!(
                "input={input},stream=0,init_segment={output}/init.mp4,segment_template={output}/seg_$Number$.m4s,playlist_name=media.m3u8"
            ),
            "--segment_duration".into(),
            "2".into(),
            "--hls_playlist_type".into(),
            "VOD".into(),
            "--hls_master_playlist_output".into(),
            format!("{output}/master.m3u8"),
        ];
        self.run(&argv, "packager", &[])?;
        Ok(())
    }
    pub fn profile(&self, path: &Path) -> Result<serde_json::Value, ConversionError> {
        self.profile_stream(path, 0)
    }
    pub fn profile_stream(
        &self,
        path: &Path,
        index: usize,
    ) -> Result<serde_json::Value, ConversionError> {
        let argv = vec![
            self.ffprobe.clone(),
            "-v".into(),
            "error".into(),
            "-protocol_whitelist".into(),
            "file,crypto".into(),
            "-select_streams".into(),
            index.to_string(),
            "-count_packets".into(),
            "-show_entries".into(),
            concat!(
                "stream=index,codec_name,codec_type,time_base,nb_read_packets,",
                "sample_rate,channels,channel_layout,width,height,pix_fmt,sample_aspect_ratio,",
                "color_range,color_space,color_transfer,color_primaries,chroma_location,field_order,",
                "bits_per_raw_sample,extradata_hash:stream_tags=language:",
                "stream_side_data=side_data_type,displaymatrix,rotation"
            ).into(),
            "-show_data_hash".into(),
            "sha256".into(),
            "-of".into(),
            "json".into(),
            super::files::tool_path(path)?,
        ];
        let bytes = self.run(&argv, "ffprobe", &[])?;
        if bytes.len() > 1024 * 1024 {
            return Err(ConversionError("probe-result-size-limit"));
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| ConversionError("invalid-media-probe"))?;
        let streams = value
            .get("streams")
            .and_then(serde_json::Value::as_array)
            .ok_or(ConversionError("invalid-media-probe"))?;
        if streams.len() != 1 {
            return Err(ConversionError("single-track-mp4-required"));
        }
        let stream = &streams[0];
        if stream["index"].as_u64() != Some(index as u64) {
            return Err(ConversionError("stream-selector-mismatch"));
        }
        if !matches!(stream["codec_type"].as_str(), Some("audio" | "video"))
            || !stream["nb_read_packets"]
                .as_str()
                .is_some_and(|count| count.parse::<u64>().is_ok_and(|value| value > 0))
        {
            return Err(ConversionError("media-packets-missing"));
        }
        let mut profile = serde_json::Map::new();
        for name in ["codec_name", "codec_type", "time_base", "nb_read_packets"] {
            profile.insert(
                name.into(),
                stream
                    .get(name)
                    .cloned()
                    .ok_or(ConversionError("stream-metadata-unavailable"))?,
            );
        }
        for name in [
            "sample_rate",
            "channels",
            "channel_layout",
            "width",
            "height",
            "pix_fmt",
            "sample_aspect_ratio",
            "color_range",
            "color_space",
            "color_transfer",
            "color_primaries",
            "chroma_location",
            "field_order",
            "bits_per_raw_sample",
            "extradata_hash",
        ] {
            profile.insert(name.into(), stream.get(name).cloned().unwrap_or_default());
        }
        let display_matrices: Vec<_> = stream["side_data_list"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|data| data.get("displaymatrix").is_some() || data.get("rotation").is_some())
            .cloned()
            .collect();
        profile.insert("display_matrices".into(), display_matrices.into());
        profile.insert(
            "language".into(),
            stream["tags"]["language"].as_str().unwrap_or("und").into(),
        );
        Ok(serde_json::Value::Object(profile))
    }
    pub fn packets_stream(
        &self,
        path: &Path,
        index: usize,
    ) -> Result<Vec<(i64, i64, i64)>, ConversionError> {
        // The demuxer itself bounds the emitted packet count; an incomplete presentation fails.
        const MAX_PACKETS: usize = 100_000;
        let profile = self.profile_stream(path, index)?;
        let expected = profile["nb_read_packets"]
            .as_str()
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|n| *n <= MAX_PACKETS)
            .ok_or(ConversionError("packet-evidence-count-limit"))?;
        let argv = vec![
            self.ffprobe.clone(),
            "-v".into(),
            "error".into(),
            "-protocol_whitelist".into(),
            "file,crypto".into(),
            "-select_streams".into(),
            index.to_string(),
            "-read_intervals".into(),
            format!("%+#{}", MAX_PACKETS + 1),
            "-show_packets".into(),
            "-show_entries".into(),
            "packet=stream_index,pts,dts,duration".into(),
            "-of".into(),
            "json".into(),
            super::files::tool_path(path)?,
        ];
        let bytes = self.run(&argv, "ffprobe", &[])?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(ConversionError("packet-evidence-size-limit"));
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| ConversionError("invalid-packet-evidence"))?;
        let packets = value["packets"]
            .as_array()
            .ok_or(ConversionError("invalid-packet-evidence"))?;
        if packets.is_empty() || packets.len() > MAX_PACKETS || packets.len() != expected {
            return Err(ConversionError("packet-evidence-count-limit"));
        }
        packets
            .iter()
            .map(|packet| {
                if packet["stream_index"].as_u64() != Some(index as u64) {
                    return Err(ConversionError("single-track-mp4-required"));
                }
                let pts = packet["pts"]
                    .as_i64()
                    .ok_or(ConversionError("packet-timing-unavailable"))?;
                let dts = packet["dts"]
                    .as_i64()
                    .ok_or(ConversionError("packet-timing-unavailable"))?;
                let duration = packet["duration"]
                    .as_i64()
                    .filter(|value| *value > 0)
                    .ok_or(ConversionError("packet-timing-unavailable"))?;
                Ok((pts, dts, duration))
            })
            .collect()
    }
    pub fn hash_stream(
        &self,
        path: &Path,
        index: usize,
        copy: bool,
        key: Option<&ContentKey>,
    ) -> Result<String, ConversionError> {
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
            super::files::tool_path(path)?,
            "-map".into(),
            format!("0:{index}"),
        ];
        let secret = key.map(ContentKey::key_hex);
        if let Some(secret) = &secret {
            let input_position = argv
                .iter()
                .position(|value| value == "-i")
                .expect("input option exists");
            argv.splice(
                input_position..input_position,
                ["-decryption_key".into(), secret.to_string()],
            );
        }
        let argv = Zeroizing::new(argv);
        let mut argv = argv;
        if copy {
            argv.extend(["-c".into(), "copy".into()]);
        } else {
            argv.extend([
                "-threads".into(),
                "1".into(),
                "-fps_mode".into(),
                "passthrough".into(),
            ]);
        }
        argv.extend([
            "-f".into(),
            "hash".into(),
            "-hash".into(),
            "sha256".into(),
            "-".into(),
        ]);
        let secrets: Vec<_> = secret.iter().map(|value| value.as_str()).collect();
        let bytes = self.run(&argv, "ffmpeg", &secrets)?;
        let value = std::str::from_utf8(&bytes)
            .map_err(|_| ConversionError("invalid-verification-result"))?
            .trim();
        let digest = value
            .strip_prefix("SHA256=")
            .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or(ConversionError("invalid-verification-result"))?;
        Ok(digest.to_ascii_lowercase())
    }
}
