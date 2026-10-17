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
        let shown = vec![tool.to_string(), "[arguments redacted]".into()];
        let options = RunOptions {
            argv_redacted: Some(&shown),
            secrets,
            timeout: Some(self.timeout),
            max_output_chars: 0,
            log_stdout: false,
            ..RunOptions::default()
        };
        let completed = self
            .runner
            .run_tool(argv, tool, &options)
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
        let input = input.to_str().ok_or(ConversionError("unsupported-path"))?;
        let output = output.to_str().ok_or(ConversionError("unsupported-path"))?;
        if input.contains([',', '\n', '\r']) || output.contains([',', '\n', '\r']) {
            return Err(ConversionError("unsupported-path"));
        }
        let mut argv = Zeroizing::new(vec![
            self.packager.clone(),
            format!("input={input},stream=0,output={output}"),
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
        super::private_file_mode(Path::new(output))?;
        Ok(())
    }
    pub fn profile(&self, path: &Path) -> Result<serde_json::Value, ConversionError> {
        let argv = vec![
            self.ffprobe.clone(),
            "-v".into(),
            "error".into(),
            "-protocol_whitelist".into(),
            "file".into(),
            "-count_packets".into(),
            "-show_entries".into(),
            "stream=index,codec_name,codec_type,time_base,start_pts,duration_ts,nb_read_packets"
                .into(),
            "-of".into(),
            "json".into(),
            path.to_str()
                .ok_or(ConversionError("unsupported-path"))?
                .into(),
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
        if !matches!(stream["codec_type"].as_str(), Some("audio" | "video"))
            || !stream["nb_read_packets"]
                .as_str()
                .is_some_and(|count| count.parse::<u64>().is_ok_and(|value| value > 0))
        {
            return Err(ConversionError("media-packets-missing"));
        }
        let mut profile = serde_json::Map::new();
        for name in [
            "index",
            "codec_name",
            "codec_type",
            "time_base",
            "start_pts",
            "duration_ts",
            "nb_read_packets",
        ] {
            profile.insert(
                name.into(),
                stream
                    .get(name)
                    .cloned()
                    .ok_or(ConversionError("stream-metadata-unavailable"))?,
            );
        }
        Ok(serde_json::Value::Object(profile))
    }
    pub fn packets(&self, path: &Path) -> Result<Vec<(i64, i64, i64)>, ConversionError> {
        // The demuxer itself bounds the emitted packet count; an incomplete presentation fails.
        const MAX_PACKETS: usize = 100_000;
        let argv = vec![
            self.ffprobe.clone(),
            "-v".into(),
            "error".into(),
            "-protocol_whitelist".into(),
            "file".into(),
            "-read_intervals".into(),
            format!("%+#{}", MAX_PACKETS + 1),
            "-show_packets".into(),
            "-show_entries".into(),
            "packet=stream_index,pts,dts,duration".into(),
            "-of".into(),
            "json".into(),
            path.to_str()
                .ok_or(ConversionError("unsupported-path"))?
                .into(),
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
        if packets.is_empty() || packets.len() > MAX_PACKETS {
            return Err(ConversionError("packet-evidence-count-limit"));
        }
        packets
            .iter()
            .map(|packet| {
                if packet["stream_index"].as_u64() != Some(0) {
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
    pub fn hash(
        &self,
        path: &Path,
        copy: bool,
        key: Option<&ContentKey>,
    ) -> Result<String, ConversionError> {
        let mut argv = vec![
            self.ffmpeg.clone(),
            "-v".into(),
            "error".into(),
            "-nostdin".into(),
            "-xerror".into(),
            "-threads".into(),
            "1".into(),
            "-protocol_whitelist".into(),
            "file".into(),
            "-i".into(),
            path.to_str()
                .ok_or(ConversionError("unsupported-path"))?
                .into(),
            "-map".into(),
            "0:0".into(),
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
