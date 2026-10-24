//! Backend command construction. Retained records contain neither raw arguments nor tool output.

mod decoded_hash;

const RESOURCE_FLAGS: [&str; 2] = ["--single_threaded", "--io_cache_size=0"];
use super::{ContentKey, ConversionError, InputKeys, OutputKey};
use crate::{
    inspection::TargetProtection,
    toolrun::{RunOptions, ToolRunner},
};
use std::{fmt::Write as _, path::Path, sync::Arc, time::Duration};
use zeroize::Zeroizing;

#[derive(Clone)]
pub(super) struct Backend {
    pub stop: crate::concurrency::Stop,
    pub runner: Arc<ToolRunner>,
    pub packager: String,
    pub ffmpeg: String,
    pub ffprobe: String,
    pub timeout: Duration,
    pub validation_depth: super::ValidationDepth,
    pub measurement: Option<Arc<super::measurement::Measurements>>,
    pub activity: Option<Arc<dyn crate::events::Sink>>,
}
impl Backend {
    pub fn processing(
        &self,
        route: crate::inspection::ProcessingRouteId,
        preparation: crate::inspection::InputPreparation,
    ) -> crate::inspection::ActualProcessing {
        use crate::inspection::RequiredTool;
        let mut report = crate::inspection::ActualProcessing::new(route, preparation);
        if self.validation_depth == super::ValidationDepth::Decoded {
            report
                .required_tools
                .splice(0..0, [RequiredTool::Ffmpeg, RequiredTool::Ffprobe]);
            if route == crate::inspection::ProcessingRouteId::MpegTsSegments {
                report.required_tools.push(RequiredTool::ShakaPackager);
            }
        }
        report
    }

    pub fn task(&self, task: super::observation::Task) -> super::observation::Scope {
        super::observation::Scope::new(self.activity.as_ref(), task)
    }
    pub fn scope(&self, phase: super::measurement::Phase) -> super::measurement::Scope {
        self.measurement
            .as_ref()
            .map_or_else(super::measurement::Scope::default, |meter| {
                meter.scope(phase)
            })
            .observed(self.activity.as_ref(), phase)
    }
    pub fn completed_work_unit(&self, ordinal: usize, total: usize) {
        if let Some(sink) = &self.activity {
            sink.emit(serde_json::json!({"stage":"conversion", "event":"work-unit-completed", "ordinal":ordinal, "total":total}).as_object().unwrap());
        }
    }
    pub fn checkpoint(&self, roots: &[&super::files::Stage]) {
        if let Some(meter) = &self.measurement {
            meter.checkpoint(roots);
        }
    }
    pub fn check_cancelled(&self) -> Result<(), ConversionError> {
        if self.stop.is_set() {
            Err(ConversionError("cancelled"))
        } else {
            Ok(())
        }
    }
    pub(super) fn require_decode(&self) -> Result<(), ConversionError> {
        if self.validation_depth == super::ValidationDepth::Decoded {
            Ok(())
        } else {
            Err(ConversionError("decoder-tool-requires-decode"))
        }
    }
    pub fn run(
        &self,
        argv: &[String],
        tool: &str,
        secrets: &[&str],
    ) -> Result<Vec<u8>, ConversionError> {
        self.run_with_cleanup_receipt(argv, tool, secrets, &mut None)
    }
    pub fn run_with_cleanup_receipt(
        &self,
        argv: &[String],
        tool: &str,
        secrets: &[&str],
        receipt: &mut Option<crate::toolrun::CleanupReceipt>,
    ) -> Result<Vec<u8>, ConversionError> {
        self.check_cancelled()?;
        if matches!(tool, "ffmpeg" | "ffprobe") {
            self.require_decode()?;
        }
        let effective = Self::effective_argv(argv, tool);
        let shown = vec![tool.to_string(), "[arguments redacted]".into()];
        let options = RunOptions {
            stop: Some(&self.stop),
            argv_redacted: Some(&shown),
            secrets,
            timeout: Some(self.timeout),
            max_output_chars: 0,
            log_stdout: false,
            ..RunOptions::default()
        };
        let completed = self
            .runner
            .run_tool_with_cleanup_receipt(&effective, tool, &options, receipt)
            .map_err(|error| tool_error(&error))?;
        self.check_cancelled()?;
        if completed.returncode != Some(0) {
            return Err(ConversionError("media-tool-failed"));
        }
        Ok(completed.stdout)
    }
    pub(super) fn effective_argv(argv: &[String], tool: &str) -> Zeroizing<Vec<String>> {
        let mut effective = Zeroizing::new(argv.to_vec());
        if tool == "ffprobe" {
            effective.splice(1..1, ["-threads".into(), "1".into()]);
        }
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
        if tool == "packager" {
            effective.extend(RESOURCE_FLAGS.map(String::from));
        }
        effective
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
        if super::finite::window(input, index)?.is_none() {
            super::audio::require_supported_audio(input, index)?;
        }
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
        super::finite::restore(Path::new(&input), index, Path::new(&output), &self.stop)?;
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
        super::finite::restore(
            Path::new(&input),
            0,
            &Path::new(&output).join("init.mp4"),
            &self.stop,
        )?;
        Ok(())
    }
    pub fn profile(
        &self,
        path: &Path,
        key: Option<&ContentKey>,
    ) -> Result<serde_json::Value, ConversionError> {
        self.profile_stream(path, 0, key)
    }
    pub fn profile_stream(
        &self,
        path: &Path,
        index: usize,
        key: Option<&ContentKey>,
    ) -> Result<serde_json::Value, ConversionError> {
        self.profile_stream_keys(path, index, key.map(super::keys::ProbeKeys::One))
    }
    pub(super) fn profile_stream_options(
        &self,
        path: &Path,
        index: usize,
        key: Option<&ContentKey>,
        unedited: bool,
    ) -> Result<serde_json::Value, ConversionError> {
        self.profile_stream_keys_options(
            path,
            index,
            key.map(super::keys::ProbeKeys::One),
            unedited,
        )
    }
    pub(super) fn profile_stream_keys(
        &self,
        path: &Path,
        index: usize,
        key: Option<super::keys::ProbeKeys<'_>>,
    ) -> Result<serde_json::Value, ConversionError> {
        self.profile_stream_keys_options(path, index, key, false)
    }
    pub(super) fn profile_stream_keys_options(
        &self,
        path: &Path,
        index: usize,
        key: Option<super::keys::ProbeKeys<'_>>,
        unedited: bool,
    ) -> Result<serde_json::Value, ConversionError> {
        if self.validation_depth == super::ValidationDepth::Native {
            return self.native_profile(path, index);
        }
        let mut argv = vec![
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
                "bits_per_raw_sample,nal_length_size,extradata_hash:stream_tags=language:",
                "stream_side_data=side_data_type,displaymatrix,rotation"
            ).into(),
            "-show_data_hash".into(),
            "sha256".into(),
            "-of".into(),
            "json".into(),
            super::files::tool_path(path)?,
        ];
        if unedited {
            argv.splice(1..1, ["-ignore_editlist".into(), "1".into()]);
        }
        // Codec metadata can be inferred from packets, so encrypted inputs need their role key.
        let keyed = super::keys::KeyedArguments::new(argv, key);
        let argv = &keyed.argv;
        let secrets = keyed.secrets();
        let bytes = self.run(argv, "ffprobe", &secrets)?;
        if bytes.len() > 1024 * 1024 {
            return Err(ConversionError("probe-result-size-limit"));
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| ConversionError("invalid-media-probe"))?;
        self.read_profile(path, index, &value)
    }
    fn read_profile(
        &self,
        path: &Path,
        index: usize,
        value: &serde_json::Value,
    ) -> Result<serde_json::Value, ConversionError> {
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
            "nal_length_size",
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
        let profile = serde_json::Value::Object(profile);
        self.runner
            .register_workload(super::execution::observe_profile(&profile, path)?);
        Ok(profile)
    }
    pub fn packets_stream(
        &self,
        path: &Path,
        index: usize,
        key: Option<&ContentKey>,
    ) -> Result<super::packets::Evidence, ConversionError> {
        self.packets_stream_keys(path, index, key.map(super::keys::ProbeKeys::One))
    }
    pub(super) fn packets_stream_keys(
        &self,
        path: &Path,
        index: usize,
        key: Option<super::keys::ProbeKeys<'_>>,
    ) -> Result<super::packets::Evidence, ConversionError> {
        self.packets_stream_keys_options(path, index, key, false)
    }
    pub(super) fn packets_stream_options(
        &self,
        path: &Path,
        index: usize,
        key: Option<&ContentKey>,
        unedited: bool,
    ) -> Result<super::packets::Evidence, ConversionError> {
        if !unedited {
            return self.packets_stream(path, index, key);
        }
        self.packets_stream_keys_options(
            path,
            index,
            key.map(super::keys::ProbeKeys::One),
            unedited,
        )
    }
    pub(super) fn packets_stream_keys_options(
        &self,
        path: &Path,
        index: usize,
        key: Option<super::keys::ProbeKeys<'_>>,
        unedited: bool,
    ) -> Result<super::packets::Evidence, ConversionError> {
        let _task = self.task(super::observation::Task::PacketTiming);
        let profile = self.profile_stream_keys_options(path, index, key, unedited)?;
        let expected = profile["nb_read_packets"]
            .as_str()
            .and_then(|count| count.parse::<u64>().ok())
            .filter(|count| *count > 0)
            .ok_or(ConversionError("media-packets-missing"))?;
        let mut argv = vec![
            self.ffprobe.clone(),
            "-v".into(),
            "error".into(),
            "-protocol_whitelist".into(),
            "file,crypto".into(),
            "-select_streams".into(),
            index.to_string(),
            "-show_packets".into(),
            "-show_entries".into(),
            "packet=stream_index,pts,dts,duration:packet_side_data=".into(),
            "-of".into(),
            "compact=p=1:nk=0".into(),
            super::files::tool_path(path)?,
        ];
        if unedited {
            argv.splice(1..1, ["-ignore_editlist".into(), "1".into()]);
        }
        let keyed = super::keys::KeyedArguments::new(argv, key);
        let argv = &keyed.argv;
        let secrets = keyed.secrets();
        self.require_decode()?;
        let effective = Self::effective_argv(argv, "ffprobe");
        let shown = ["ffprobe".into(), "[arguments redacted]".into()];
        let options = RunOptions {
            stop: Some(&self.stop),
            secrets: &secrets,
            argv_redacted: Some(&shown),
            timeout: Some(self.timeout),
            max_output_chars: 0,
            // Charge the fixed record, hashing state and 8 KiB pipe read buffer.
            max_stdout_bytes: 16 * 1024,
            log_stdout: false,
            ..RunOptions::default()
        };
        self.check_cancelled()?;
        let mut parser = super::packets::Parser::new(index, expected);
        let completed = self
            .runner
            .run_tool_streamed(&effective, "ffprobe", &options, |chunk| parser.push(chunk));
        let completed = completed.map_err(|error| match error.kind {
            "stream-invalid" => parser
                .error()
                .unwrap_or(ConversionError("invalid-packet-evidence")),
            _ => tool_error(&error),
        })?;
        self.check_cancelled()?;
        if completed.returncode != Some(0) {
            return Err(ConversionError("media-tool-failed"));
        }
        parser.finish()
    }
    pub fn hash_stream(
        &self,
        path: &Path,
        index: usize,
        copy: bool,
        key: Option<&ContentKey>,
    ) -> Result<String, ConversionError> {
        self.hash_stream_keys(path, index, copy, key.map(super::keys::ProbeKeys::One))
    }
    pub(super) fn hash_stream_keys(
        &self,
        path: &Path,
        index: usize,
        copy: bool,
        key: Option<super::keys::ProbeKeys<'_>>,
    ) -> Result<String, ConversionError> {
        self.hash_stream_keys_options(path, index, copy, key, false)
    }
    pub(super) fn hash_stream_options(
        &self,
        path: &Path,
        index: usize,
        copy: bool,
        key: Option<&ContentKey>,
        unedited: bool,
    ) -> Result<String, ConversionError> {
        if !unedited {
            return self.hash_stream(path, index, copy, key);
        }
        self.hash_stream_keys_options(
            path,
            index,
            copy,
            key.map(super::keys::ProbeKeys::One),
            unedited,
        )
    }
    pub(super) fn hash_stream_keys_options(
        &self,
        path: &Path,
        index: usize,
        copy: bool,
        key: Option<super::keys::ProbeKeys<'_>>,
        unedited: bool,
    ) -> Result<String, ConversionError> {
        let _task = self.task(if copy {
            super::observation::Task::EncodedContent
        } else {
            super::observation::Task::DecodedContent
        });
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
        if unedited {
            argv.splice(1..1, ["-ignore_editlist".into(), "1".into()]);
        }
        let mut keyed = super::keys::KeyedArguments::new(argv, key);
        if !copy {
            return self.hash_decoded_stream(keyed, index);
        }
        let argv = &mut keyed.argv;
        argv.extend(["-c".into(), "copy".into()]);
        argv.extend([
            "-f".into(),
            "hash".into(),
            "-hash".into(),
            "sha256".into(),
            "-".into(),
        ]);
        let secrets = keyed.secrets();
        let bytes = self.run(&keyed.argv, "ffmpeg", &secrets)?;
        let value = std::str::from_utf8(&bytes)
            .map_err(|_| ConversionError("invalid-verification-result"))?
            .trim();
        let digest = value
            .strip_prefix("SHA256=")
            .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or(ConversionError("invalid-verification-result"))?;
        Ok(digest.to_ascii_lowercase())
    }

    pub(super) fn finite_audio_decode(
        &self,
        path: &Path,
        index: usize,
        key: Option<super::keys::ProbeKeys<'_>>,
        window: super::finite::Window,
        format: (u32, u32),
        complete: bool,
    ) -> Result<super::finite::Decoded, ConversionError> {
        let _task = self.task(super::observation::Task::DecodedContent);
        let mut argv = vec![
            self.ffmpeg.clone(),
            "-v".into(),
            "error".into(),
            "-nostdin".into(),
            "-xerror".into(),
            "-threads".into(),
            "1".into(),
            "-filter_threads".into(),
            "1".into(),
            "-protocol_whitelist".into(),
            "file,crypto".into(),
            "-i".into(),
            super::files::tool_path(path)?,
            "-map".into(),
            format!("0:{index}"),
            "-threads".into(),
            "1".into(),
            "-c:a".into(),
            "pcm_f32le".into(),
            "-f".into(),
            "f32le".into(),
            "-".into(),
        ];
        if complete {
            argv.splice(
                1..1,
                [
                    "-ignore_editlist".into(),
                    "1".into(),
                    "-flags2".into(),
                    "+skip_manual".into(),
                ],
            );
        }
        let keyed = super::keys::KeyedArguments::new(argv, key);
        let secrets = keyed.secrets();
        let shown = ["ffmpeg".into(), "[arguments redacted]".into()];
        let options = RunOptions {
            stop: Some(&self.stop),
            secrets: &secrets,
            argv_redacted: Some(&shown),
            timeout: Some(self.timeout),
            max_output_chars: 0,
            max_stdout_bytes: 16 * 1024,
            log_stdout: false,
            ..RunOptions::default()
        };
        self.check_cancelled()?;
        let mut pcm = super::finite::Pcm::new(window, format.0, format.1)?;
        self.require_decode()?;
        let complete = self
            .runner
            .run_tool_streamed(&keyed.argv, "ffmpeg", &options, |bytes| pcm.push(bytes))
            .map_err(|e| tool_error(&e))?;
        self.check_cancelled()?;
        if complete.returncode != Some(0) {
            return Err(ConversionError("media-tool-failed"));
        }
        pcm.finish()
    }
}

pub(super) fn tool_error(error: &crate::toolrun::ToolError) -> ConversionError {
    ConversionError(match error.kind {
        "cleanup-unconfirmed" => "media-cleanup-unconfirmed",
        "output-limit" => "media-output-limit",
        "resource-budget" => "resource-budget",
        "timeout" | "pipe-timeout" => "media-tool-timeout",
        "cancelled" => "cancelled",
        _ => "media-tool-unavailable-or-timeout",
    })
}

#[cfg(test)]
mod hash_tests;

#[cfg(test)]
mod hash_media_tests;

#[cfg(test)]
mod packet_tests {
    use super::*;
    use crate::events::{Record, Sink};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn native_depth_refuses_decoder_tools_before_process_launch() {
        let events = Arc::new(crate::events::Collector::default());
        let runner = Arc::new(ToolRunner::new(
            Arc::new(crate::toolrun::Secrets::new()),
            events.clone(),
        ));
        let converter = crate::conversion::Converter::new(runner, "missing", "missing");
        for tool in ["ffmpeg", "ffprobe"] {
            assert_eq!(
                converter
                    .backend
                    .run(&["missing".into()], tool, &[])
                    .unwrap_err(),
                ConversionError("decoder-tool-requires-decode")
            );
        }
        assert!(events.records().is_empty());
        assert!(converter.with_decode(true).backend.require_decode().is_ok());
    }

    #[test]
    fn actual_processing_tools_require_ffmpeg_only_for_complete_decoding() {
        use crate::inspection::{InputPreparation, ProcessingRouteId, RequiredTool};
        let runner = Arc::new(ToolRunner::new(
            Arc::new(crate::toolrun::Secrets::new()),
            Arc::new(crate::events::NoSink),
        ));
        let mut backend = crate::conversion::Converter::new(runner, "packager", "ffmpeg").backend;
        for (route, minimum) in [
            (ProcessingRouteId::ProgressiveMp4, vec![]),
            (
                ProcessingRouteId::FragmentedMp4,
                vec![RequiredTool::ShakaPackager],
            ),
            (
                ProcessingRouteId::DashPresentation,
                vec![RequiredTool::ShakaPackager],
            ),
            (
                ProcessingRouteId::MpegTsHlsTs,
                vec![RequiredTool::ShakaPackager],
            ),
        ] {
            backend.validation_depth = crate::conversion::ValidationDepth::Native;
            assert_eq!(
                backend
                    .processing(route, InputPreparation::CompleteFile)
                    .required_tools,
                minimum
            );
            backend.validation_depth = crate::conversion::ValidationDepth::Decoded;
            let mut decoded = vec![RequiredTool::Ffmpeg, RequiredTool::Ffprobe];
            decoded.extend(minimum);
            assert_eq!(
                backend
                    .processing(route, InputPreparation::CompleteFile)
                    .required_tools,
                decoded
            );
        }
    }

    struct AfterProfile {
        state: std::path::PathBuf,
        response: drmr_test_support::ToolSpec,
        swapped: AtomicBool,
    }
    impl Sink for AfterProfile {
        fn emit(&self, record: &Record) {
            if record["event"] == "tool-output" && !self.swapped.swap(true, Ordering::SeqCst) {
                self.response
                    .encode(std::fs::File::create(&self.state).unwrap())
                    .unwrap();
            }
        }
    }

    #[test]
    fn packet_proof_rejects_early_exit_truncated_records_and_count_changes() {
        for (payload, code, expected, token) in [
            (
                "packet|stream_index=0|pts=0|dts=0|duration=1\n",
                7,
                1,
                "media-tool-failed",
            ),
            (
                "packet|stream_index=0|pts=0|dts=0|duration=1",
                0,
                1,
                "invalid-packet-evidence",
            ),
            (
                "packet|stream_index=0|pts=0|dts=0|duration=1\n",
                0,
                2,
                "packet-evidence-count-limit",
            ),
            (
                "packet|stream_index=1|pts=0|dts=0|duration=1\n",
                0,
                1,
                "single-track-mp4-required",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let state = root.path().join("state");
            let input = root.path().join("input.mp4");
            std::fs::write(&input, b"fixture").unwrap();
            let profile = drmr_test_support::ToolSpec {
                stdout: serde_json::json!({"streams":[{
                    "index":0,"codec_type":"video","codec_name":"h264","time_base":"1/1",
                    "nb_read_packets":expected.to_string(),"width":16,"height":16
                }]})
                .to_string()
                .into_bytes(),
                ..Default::default()
            };
            profile
                .encode(std::fs::File::create(&state).unwrap())
                .unwrap();
            let tool = drmr_test_support::install(
                &root.path().join("probe"),
                &drmr_test_support::ToolSpec::default()
                    .with("status_file", state.to_str().unwrap()),
            )
            .unwrap();
            let runner = Arc::new(ToolRunner::new(
                Arc::new(crate::toolrun::Secrets::new()),
                Arc::new(AfterProfile {
                    state,
                    response: drmr_test_support::ToolSpec {
                        stdout: payload.as_bytes().to_vec(),
                        exit_code: code,
                        ..Default::default()
                    },
                    swapped: AtomicBool::new(false),
                }),
            ));
            let backend = Backend {
                stop: crate::concurrency::Stop::new(),
                runner,
                packager: String::new(),
                ffmpeg: String::new(),
                ffprobe: tool.to_str().unwrap().into(),
                timeout: Duration::from_secs(20),
                validation_depth: crate::conversion::ValidationDepth::Decoded,
                measurement: None,
                activity: None,
            };
            assert_eq!(
                backend.packets_stream(&input, 0, None).unwrap_err(),
                ConversionError(token)
            );
        }
    }
}
