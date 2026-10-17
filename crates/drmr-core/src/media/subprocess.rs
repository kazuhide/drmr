//! Subprocess implementation of media probing, remuxing, and decoding.
//!
//! `FFmpeg`, ffprobe, and mp4dump run through the secure tool runner. Binary payloads
//! consumed as data are kept out of logs; recorded text is bounded and scrubbed.
//! Child stdin is closed, and these operations have no per-call timeout.

use super::parse::{
    json_loads_bytes, json_loads_str, packets_from_probe, parse_fragment_boundary,
    parse_init_probe, parse_tenc_constant_iv_hex, payload_hashes_from_probe, payloads_from_probe,
    segment_timing_from_probe,
};
use super::python::{self, path_str};
use super::{
    FragmentBoundary, HexSecret, HlsMediaError, InitProbe, MediaEngine, MediaError, PacketInfo,
    SegmentTiming,
};
use crate::canonical::python_float_repr;
use crate::toolrun::{RunOptions, ToolRunner, universal_newlines};
use num_rational::BigRational;
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::fmt;
use std::io::{BufRead as _, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Instant;

/// ffmpeg's default name (`media_inspect`'s `ffmpeg_bin`, `hls_media`'s literal).
pub const FFMPEG: &str = "ffmpeg";
/// ffprobe's default name.
pub const FFPROBE: &str = "ffprobe";
/// mp4dump's (Bento4) default name.
pub const MP4DUMP: &str = "mp4dump";
/// `run_media`'s bound on each output in its tool record (`64 * 1024`).
pub const MEDIA_MAX_OUTPUT_CHARS: usize = 64 * 1024;
/// `compute_framemd5`'s bound (`16 * 1024`): the report is a comparison input, not reading.
pub const FRAMEMD5_MAX_OUTPUT_CHARS: usize = 16 * 1024;
/// The bound of `media_inspect`'s ffprobe and mp4dump reads (`64 * 1024`).
pub const PROBE_MAX_OUTPUT_CHARS: usize = 64 * 1024;
/// `_PCM_ARGS`: the canonical audio of the identity compares.
const PCM_ARGS: [&str; 7] = ["-vn", "-f", "s16le", "-ac", "2", "-ar", "48000"];
/// `decoded_digest` reads the audio PCM in chunks of this size.
const CHUNK: usize = 1024 * 1024;

/// The subprocess engine: a tool runner and the tool binaries (Python's `ffmpeg_bin`,
/// `ffprobe_bin`, `mp4dump_bin`; `hls_media` writes the default names literally, which these
/// default to).
pub struct Subprocess {
    runner: Arc<ToolRunner>,
    /// The ffmpeg binary.
    pub ffmpeg_bin: String,
    /// The ffprobe binary.
    pub ffprobe_bin: String,
    /// The mp4dump binary.
    pub mp4dump_bin: String,
}

impl fmt::Debug for Subprocess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Subprocess")
            .field("ffmpeg_bin", &self.ffmpeg_bin)
            .field("ffprobe_bin", &self.ffprobe_bin)
            .field("mp4dump_bin", &self.mp4dump_bin)
            .finish_non_exhaustive()
    }
}

/// A text-mode run's result, decoded as Python's `text=True` decodes it.
struct TextOutput {
    returncode: Option<i32>,
    stdout: String,
    stderr: String,
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

/// Python's text mode: strict UTF-8 (a `UnicodeDecodeError` — a `ValueError` — otherwise),
/// universal newlines.
fn decode_text(bytes: &[u8]) -> Result<String, MediaError> {
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(universal_newlines(text)),
        Err(error) => {
            let position = error.valid_up_to();
            Err(MediaError::Value(format!(
                "'utf-8' codec can't decode byte 0x{:02x} in position {position}",
                bytes[position]
            )))
        }
    }
}

impl Subprocess {
    /// The engine with the default binaries.
    pub fn new(runner: Arc<ToolRunner>) -> Self {
        Subprocess {
            runner,
            ffmpeg_bin: FFMPEG.to_string(),
            ffprobe_bin: FFPROBE.to_string(),
            mp4dump_bin: MP4DUMP.to_string(),
        }
    }

    /// The tool runner.
    pub fn runner(&self) -> &Arc<ToolRunner> {
        &self.runner
    }

    /// `media::run_media`: run `argv` in bytes mode (records bounded to 64 Ki characters per
    /// stream, stdout kept out of the record when `log_stdout` is false), named after the
    /// basename of `argv[0]`; a tool that cannot run is `media-tool-unavailable`, a non-zero
    /// exit `media-tool-failed` (its stderr is never echoed: media metadata is untrusted).
    pub fn run_media(&self, argv: &[String], log_stdout: bool) -> Result<Vec<u8>, MediaError> {
        let first = argv.first().ok_or_else(|| {
            MediaError::uncaught("IndexError", "list index out of range".to_string())
        })?;
        let tool = first.rsplit('/').next().unwrap_or_default();
        let options = RunOptions {
            text: false,
            max_output_chars: MEDIA_MAX_OUTPUT_CHARS,
            log_stdout,
            ..RunOptions::default()
        };
        let result = self
            .runner
            .run_tool(argv, tool, &options)
            .map_err(|_| HlsMediaError::new("media-tool-unavailable"))?;
        if result.returncode != Some(0) {
            return Err(HlsMediaError::new("media-tool-failed").into());
        }
        Ok(result.stdout)
    }

    /// `toolrun.run_tool(argv, tool=…, max_output_chars=…)` in text mode (no timeout).
    fn run_text(
        &self,
        argv: &[String],
        tool: &str,
        max_output_chars: usize,
    ) -> Result<TextOutput, MediaError> {
        let options = RunOptions {
            max_output_chars,
            ..RunOptions::default()
        };
        let completed = self.runner.run_tool(argv, tool, &options)?;
        Ok(TextOutput {
            returncode: completed.returncode,
            stdout: decode_text(&completed.stdout)?,
            stderr: decode_text(&completed.stderr)?,
        })
    }

    /// A `RuntimeError` whose message may quote a tool's stderr: scrubbed of the job's secrets
    /// (The classified error message is not; it never carries one in practice — the scrub only guards).
    fn failure(&self, message: &str) -> MediaError {
        MediaError::Runtime(self.runner.secrets().scrub(message))
    }

    /// `mp4dump --format json <path>` in text mode, as `media_inspect` reads it.
    fn mp4dump_json(&self, path: &Path) -> Result<Value, MediaError> {
        let shown = path_str(path)?;
        let argv = strings(&[&self.mp4dump_bin, "--format", "json", &shown]);
        let output = self.run_text(&argv, MP4DUMP, PROBE_MAX_OUTPUT_CHARS)?;
        if output.returncode != Some(0) {
            return Err(self.failure(&format!(
                "mp4dump failed for {shown}: {}",
                python::strip(&output.stderr)
            )));
        }
        json_loads_str(&output.stdout)
    }

    /// The `ffmpeg` decode of `media_inspect`'s PCM / SAMPLE-AES helpers (text mode, default
    /// bounds); `Some(code)` is the exit status.
    fn run_ffmpeg_decode(&self, argv: &[String]) -> Result<Option<i32>, MediaError> {
        let output = self.run_text(argv, FFMPEG, crate::toolrun::DEFAULT_MAX_OUTPUT_CHARS)?;
        Ok(output.returncode)
    }
}

impl MediaEngine for Subprocess {
    fn probe_packets(&self, path: &Path, stream_index: i64) -> Result<PacketInfo, MediaError> {
        let argv = strings(&[
            &self.ffprobe_bin,
            "-v",
            "error",
            "-show_streams",
            "-show_packets",
            "-of",
            "json",
            &path_str(path)?,
        ]);
        packets_from_probe(
            &json_loads_bytes(&self.run_media(&argv, true)?)?,
            stream_index,
        )
    }

    fn packet_payloads(&self, path: &Path, stream_index: i64) -> Result<Vec<Vec<u8>>, MediaError> {
        // -show_data dumps the packet payloads (media payloads): consumed, never logged.
        let argv = strings(&[
            &self.ffprobe_bin,
            "-v",
            "error",
            "-show_packets",
            "-show_data",
            "-of",
            "json",
            &path_str(path)?,
        ]);
        payloads_from_probe(
            &json_loads_bytes(&self.run_media(&argv, false)?)?,
            stream_index,
        )
    }

    fn packet_payload_hashes(
        &self,
        path: &Path,
        stream_index: i64,
    ) -> Result<Vec<Value>, MediaError> {
        let argv = strings(&[
            &self.ffprobe_bin,
            "-v",
            "error",
            "-show_packets",
            "-show_data_hash",
            "sha256",
            "-of",
            "json",
            &path_str(path)?,
        ]);
        payload_hashes_from_probe(
            &json_loads_bytes(&self.run_media(&argv, true)?)?,
            stream_index,
        )
    }

    fn normalize_segment(
        &self,
        path: &Path,
        destination: &Path,
        stream_index: i64,
        offset: &BigRational,
        content_type: &str,
    ) -> Result<PathBuf, MediaError> {
        let offset_text = python_float_repr(python::fraction_to_f64(offset)?);
        let mut argv = strings(&[
            &self.ffmpeg_bin,
            "-v",
            "error",
            "-y",
            "-copyts",
            "-i",
            &path_str(path)?,
            "-map",
            &format!("0:{stream_index}"),
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
            "-output_ts_offset",
            &offset_text,
        ]);
        if content_type == "audio" || content_type == "text" {
            // ADTS cannot keep a shortened final AAC packet: an fMP4 with the source timescale.
            let scale = self.probe_packets(path, stream_index)?.timescale;
            argv.extend(strings(&[
                "-movie_timescale",
                &scale.to_string(),
                "-movflags",
                "+frag_keyframe+delay_moov+default_base_moof",
            ]));
            if content_type == "text" {
                argv.extend(strings(&["-frag_duration", "1"]));
            }
            argv.extend(strings(&["-f", "mp4"]));
        } else {
            argv.extend(strings(&[
                "-mpegts_copyts",
                "1",
                "-muxdelay",
                "0",
                "-muxpreload",
                "0",
                "-f",
                "mpegts",
            ]));
        }
        argv.push(path_str(destination)?);
        self.run_media(&argv, true)?;
        Ok(destination.to_path_buf())
    }

    fn decoded_digest(
        &self,
        path: &Path,
        content_type: &str,
        stream_index: i64,
    ) -> Result<String, MediaError> {
        // One decoder thread per call : the pool decides how many cores work.
        let mut argv = strings(&[
            &self.ffmpeg_bin,
            "-v",
            "error",
            "-threads",
            "1",
            "-i",
            &path_str(path)?,
            "-map",
            &format!("0:{stream_index}"),
        ]);
        let audio = content_type == "audio";
        if audio {
            // Channel count and sample rate preserved: no resampling, no folding.
            argv.extend(strings(&["-f", "s32le", "-acodec", "pcm_s32le", "-"]));
        } else {
            argv.extend(strings(&[
                "-fps_mode",
                "passthrough",
                "-f",
                "framemd5",
                "-",
            ]));
        }
        // Streamed: the output is the digest's input, not log text.
        let call_id = self.runner.note_command(&argv, FFMPEG, None);
        let started = Instant::now();
        let Ok((code, digest)) = stream_digest(&argv, audio) else {
            self.runner.note_result(
                FFMPEG,
                None,
                started.elapsed().as_secs_f64(),
                None,
                "could not run",
                Some(call_id),
            );
            return Err(HlsMediaError::new("media-tool-unavailable").into());
        };
        self.runner.note_result(
            FFMPEG,
            code,
            started.elapsed().as_secs_f64(),
            None,
            "",
            Some(call_id),
        );
        if code != Some(0) {
            return Err(HlsMediaError::new("media-decode-failed").into());
        }
        Ok(digest)
    }

    fn box_tree(&self, path: &Path) -> Result<Value, MediaError> {
        let argv = strings(&[&self.mp4dump_bin, "--format", "json", &path_str(path)?]);
        json_loads_bytes(&self.run_media(&argv, true)?)
    }

    fn compute_framemd5(&self, path: &Path) -> Result<String, MediaError> {
        // One line per frame: the log keeps a bounded head (the text is a comparison input).
        let shown = path_str(path)?;
        let argv = strings(&[
            &self.ffmpeg_bin,
            "-v",
            "error",
            "-i",
            &shown,
            "-f",
            "framemd5",
            "-",
        ]);
        let output = self.run_text(&argv, FFMPEG, FRAMEMD5_MAX_OUTPUT_CHARS)?;
        if output.returncode != Some(0) {
            return Err(self.failure(&format!(
                "ffmpeg framemd5 failed for {shown}: {}",
                python::strip(&output.stderr)
            )));
        }
        Ok(output.stdout)
    }

    fn probe_segment_timing(&self, path: &Path) -> Result<SegmentTiming, MediaError> {
        let shown = path_str(path)?;
        let argv = strings(&[
            &self.ffprobe_bin,
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-show_entries",
            "packet=pts_time",
            "-of",
            "json",
            &shown,
        ]);
        let output = self.run_text(&argv, FFPROBE, PROBE_MAX_OUTPUT_CHARS)?;
        if output.returncode != Some(0) {
            return Err(self.failure(&format!(
                "ffprobe failed for {shown}: {}",
                python::strip(&output.stderr)
            )));
        }
        segment_timing_from_probe(&json_loads_str(&output.stdout)?)
    }

    fn read_tenc_constant_iv_hex(
        &self,
        init_segment_path: &Path,
    ) -> Result<Option<HexSecret>, MediaError> {
        parse_tenc_constant_iv_hex(&self.mp4dump_json(init_segment_path)?)
    }

    fn probe_init_segment(&self, init_segment_path: &Path) -> Result<InitProbe, MediaError> {
        parse_init_probe(&self.mp4dump_json(init_segment_path)?)
    }

    fn read_fragment_boundary(
        &self,
        segment_path: &Path,
    ) -> Result<Option<FragmentBoundary>, MediaError> {
        Ok(parse_fragment_boundary(&self.mp4dump_json(segment_path)?))
    }

    fn decode_media_to_pcm(
        &self,
        input_path: &Path,
        output_path: &Path,
    ) -> Result<PathBuf, MediaError> {
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut argv = strings(&[
            &self.ffmpeg_bin,
            "-v",
            "error",
            "-y",
            "-i",
            &path_str(input_path)?,
        ]);
        argv.extend(strings(&PCM_ARGS));
        argv.push(path_str(output_path)?);
        if self.run_ffmpeg_decode(&argv)? != Some(0) {
            return Err(MediaError::Runtime(format!(
                "ffmpeg PCM decode failed for {}",
                python::path_name(input_path)?
            )));
        }
        Ok(output_path.to_path_buf())
    }

    fn decode_hls_media_playlist(
        &self,
        localized_playlist_path: &Path,
        output_path: &Path,
        mode: &str,
    ) -> Result<PathBuf, MediaError> {
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut argv = strings(&[
            &self.ffmpeg_bin,
            "-v",
            "error",
            "-y",
            "-allowed_extensions",
            "ALL",
            "-i",
            &path_str(localized_playlist_path)?,
        ]);
        match mode {
            "pcm" => argv.extend(strings(&PCM_ARGS)),
            "framemd5" => argv.extend(strings(&["-an", "-f", "framemd5"])),
            _ => {
                return Err(MediaError::Value(format!(
                    "unknown decode mode {} (expected 'pcm' | 'framemd5')",
                    python::repr_str(mode)
                )));
            }
        }
        argv.push(path_str(output_path)?);
        if self.run_ffmpeg_decode(&argv)? != Some(0) {
            // stderr may name the localized playlist or the key file's path, never the key.
            return Err(MediaError::Runtime(format!(
                "ffmpeg SAMPLE-AES decode ({mode}) failed for {}",
                python::path_name(localized_playlist_path)?
            )));
        }
        Ok(output_path.to_path_buf())
    }
}

/// `bytes.strip`: ASCII whitespace including `\v` (Rust's `trim_ascii` leaves `\v`).
fn strip_bytes(line: &[u8]) -> &[u8] {
    let space = |byte: &u8| matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let start = line
        .iter()
        .position(|byte| !space(byte))
        .unwrap_or(line.len());
    let end = line
        .iter()
        .rposition(|byte| !space(byte))
        .map_or(start, |last| last + 1);
    &line[start..end]
}

/// The digest input of one framemd5 line: its last two fields (`size, hash`) joined by `,`.
fn hash_frame_lines(stdout: impl Read, digest: &mut Sha256) -> std::io::Result<()> {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok(());
        }
        let stripped = strip_bytes(&line);
        if stripped.is_empty() || line.starts_with(b"#") {
            continue;
        }
        let fields: Vec<&[u8]> = stripped.split(|byte| *byte == b',').collect();
        let tail = fields[fields.len().saturating_sub(2)..].join(&b","[..]);
        digest.update(strip_bytes(&tail));
        digest.update(b"\n");
    }
}

fn hash_chunks(mut stdout: impl Read, digest: &mut Sha256) -> std::io::Result<()> {
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = stdout.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        digest.update(&buffer[..read]);
    }
}

/// A streamed reader that can stop even when a Unix descendant escapes its group.
struct DigestPipe<'a> {
    pipe: std::process::ChildStdout,
    stop: &'a std::sync::atomic::AtomicBool,
}

impl Read for DigestPipe<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        use std::sync::atomic::Ordering;
        loop {
            if self.stop.load(Ordering::Acquire) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "streamed output pipe remained open",
                ));
            }
            match self.pipe.read(buffer) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::park_timeout(std::time::Duration::from_millis(5));
                }
                result => return result,
            }
        }
    }
}

/// Hash stdout without retaining its payload. A decoder's exit gives inherited
/// pipe writers two seconds to close; errors kill the managed tree before joining
/// the reader or waiting for the child. Running decoders retain no per-call timeout.
fn stream_digest(argv: &[String], audio: bool) -> std::io::Result<(Option<i32>, String)> {
    use crate::native::{ManagedChild, in_asset_worker};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, TryRecvError};
    use std::time::Duration;
    let (program, arguments) = argv.split_first().expect("decoded_digest builds a command");
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = ManagedChild::spawn(command, !in_asset_worker())?;
    let stdout = child.stdout.take().expect("stdout is piped");
    #[cfg(unix)]
    {
        let flags = rustix::fs::fcntl_getfl(&stdout)?;
        rustix::fs::fcntl_setfl(&stdout, flags | rustix::fs::OFlags::NONBLOCK)?;
    }
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(1);
        let stop = &stop;
        let reader = std::thread::Builder::new()
            .name("decoded-digest".into())
            .spawn_scoped(scope, move || {
                let stdout = DigestPipe { pipe: stdout, stop };
                let mut digest = Sha256::new();
                let read = if audio {
                    hash_chunks(stdout, &mut digest)
                } else {
                    hash_frame_lines(stdout, &mut digest)
                };
                let _ = sender.send(read.map(|()| hex::encode(digest.finalize())));
            })?;
        let mut digest = None;
        let mut status = None;
        let mut pipe_deadline = None;
        let result = loop {
            if digest.is_none() {
                match receiver.try_recv() {
                    Ok(Ok(value)) => digest = Some(value),
                    Ok(Err(error)) => break Err(error),
                    Err(TryRecvError::Disconnected) => {
                        break Err(std::io::Error::other("streamed-output reader stopped"));
                    }
                    Err(TryRecvError::Empty) => {}
                }
            }
            if status.is_none() {
                match child.try_wait() {
                    Ok(value) => status = value,
                    Err(error) => break Err(error),
                }
                if status.is_some() {
                    pipe_deadline = Some(Instant::now() + Duration::from_secs(2));
                }
            }
            if let Some(status) = status
                && let Some(digest) = digest.take()
            {
                break Ok((exit_code(status), digest));
            }
            if pipe_deadline.is_some_and(|end| Instant::now() >= end) {
                break Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "streamed output pipe remained open after decoder exit",
                ));
            }
            std::thread::park_timeout(Duration::from_millis(5));
        };
        if result.is_err() {
            let _ = child.kill();
        }
        stop.store(true, Ordering::Release);
        let joined = reader
            .join()
            .map_err(|_| std::io::Error::other("streamed-output reader panicked"));
        joined?;
        result
    })
}

/// `Popen.wait`'s return code: the exit code, or the negative signal number.
#[cfg(unix)]
fn exit_code(status: ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|number| -number))
}

#[cfg(not(unix))]
fn exit_code(status: ExitStatus) -> Option<i32> {
    status.code()
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use std::time::Duration;

    #[cfg(unix)]
    #[test]
    fn an_escaped_descendant_cannot_retain_a_streamed_reader_after_the_close_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("escaped.pid");
        let argv = vec![
            "python3".to_string(), "-c".to_string(),
            "import pathlib,subprocess,sys; child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(8)'],start_new_session=True); pathlib.Path(sys.argv[1]).write_text(str(child.pid))".to_string(),
            pid_file.to_str().unwrap().to_string(),
        ];
        let started = Instant::now();
        let result = stream_digest(&argv, true);
        // Deliberately escaped Unix processes are outside the managed group.
        // The reader still has a close deadline; clean up this fixture explicitly.
        let pid = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid).unwrap(),
            rustix::process::Signal::KILL,
        );
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_descendant_cannot_keep_the_streamed_digest_pipe_open_after_decoder_exit() {
        let python = if cfg!(windows) { "python" } else { "python3" };
        let argv = strings(&[
            python,
            "-c",
            "import subprocess,sys; subprocess.Popen([sys.executable,'-c','import time;time.sleep(6)'])",
        ]);
        let started = Instant::now();
        let error = stream_digest(&argv, false).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
