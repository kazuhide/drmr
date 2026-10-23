//! Hash every decoded byte while retaining the hash muxer's timestamp checks.
use super::{Backend, ConversionError, RunOptions, tool_error};
use crate::conversion::keys::KeyedArguments;

impl Backend {
    pub(super) fn hash_decoded_stream(
        &self,
        mut keyed: KeyedArguments,
        index: usize,
    ) -> Result<String, ConversionError> {
        // These are the hash muxer's default encoders. Do not select a pixel
        // format, sample rate, channel layout, or different audio precision.
        keyed.argv.extend([
            "-threads".into(),
            "1".into(),
            "-fps_mode".into(),
            "passthrough".into(),
            "-thread_queue_size".into(),
            "1".into(),
            "-c:v".into(),
            "rawvideo".into(),
            "-c:a".into(),
            "pcm_s16le".into(),
            "-f".into(),
            "data".into(),
            "pipe:1".into(),
            "-map".into(),
            format!("0:{index}"),
            "-threads".into(),
            "1".into(),
            "-fps_mode".into(),
            "passthrough".into(),
            "-thread_queue_size".into(),
            "1".into(),
            "-c:v".into(),
            "rawvideo".into(),
            "-c:a".into(),
            "pcm_s16le".into(),
            "-f".into(),
            "hash".into(),
            "-hash".into(),
            "adler32".into(),
            "pipe:2".into(),
        ]);
        let secrets = keyed.secrets();
        let shown = ["ffmpeg".into(), "[arguments redacted]".into()];
        let options = RunOptions {
            stop: Some(&self.stop),
            argv_redacted: Some(&shown),
            secrets: &secrets,
            timeout: Some(self.timeout),
            // Retain the previous admission reservation for child queues;
            // streamed stdout itself uses only the runner's fixed read buffer.
            max_stdout_bytes: crate::toolrun::DEFAULT_STDOUT_BYTES,
            max_output_chars: 0,
            log_stdout: false,
            ..RunOptions::default()
        };
        self.check_cancelled()?;
        let effective = Self::effective_argv(&keyed.argv, "ffmpeg");
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        let completed = self
            .runner
            .run_tool_streamed(&effective, "ffmpeg", &options, |bytes| {
                hash.update(bytes);
                Ok(())
            })
            .map_err(|error| tool_error(&error))?;
        self.check_cancelled()?;
        if completed.returncode != Some(0) {
            return Err(ConversionError("media-tool-failed"));
        }
        // Adler-32 is only a completion marker for the unchanged muxer checks.
        // The content proof is the complete SHA-256 of stdout, never this marker.
        if !completed.stderr.split(|&byte| byte == b'\n').any(|line| {
            line.strip_suffix(b"\r")
                .unwrap_or(line)
                .strip_prefix(b"adler32=")
                .is_some_and(|value| value.len() == 8 && value.iter().all(u8::is_ascii_hexdigit))
        }) {
            return Err(ConversionError("invalid-verification-result"));
        }
        Ok(hex::encode(hash.finish().as_ref()))
    }
}
