//! Inspect downloaded media through byte signatures and ffprobe.
//!
//! Validate initialization data, supported audio, and the first video packet's keyframe
//! status. Tool execution uses the secure runner.

use super::fetch::{LOCAL_CHUNK, open_exclusive, read_full};
use super::pypath::with_suffix;
use super::{Escaped, Failure, SourceError};
use crate::toolrun::{RunOptions, ToolRunner};
use serde_json::{Map, Value};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

/// One stream as `ffprobe -show_streams -of json` prints it (`index`, `codec_type`,
/// `codec_name`, `profile`, `width`, `height`).
pub type ProbeStream = Map<String, Value>;

/// `_probe(path, init_path)`: the streams of a landed segment, probed with its playlist's
/// initialization segment when there is one. Tests stand in for it as the tests
/// dependency injection `source::_probe`.
pub trait Prober: Send + Sync {
    /// The streams, or the classification of why they are not admitted.
    fn probe(&self, path: &Path, init_path: Option<&Path>) -> Result<Vec<ProbeStream>, Failure>;
}

impl<F> Prober for F
where
    F: Fn(&Path, Option<&Path>) -> Result<Vec<ProbeStream>, Failure> + Send + Sync,
{
    fn probe(&self, path: &Path, init_path: Option<&Path>) -> Result<Vec<ProbeStream>, Failure> {
        self(path, init_path)
    }
}

/// The detected container of a media segment (`_SUFFIX`'s keys).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Container {
    /// Fragmented MP4 (an `EXT-X-MAP` was in effect).
    Fmp4,
    /// `WebVTT` text.
    Webvtt,
    /// MPEG-2 transport stream.
    Ts,
}

impl Container {
    /// The container's name in the record (`fmp4`, `webvtt`, `ts`).
    pub fn as_str(self) -> &'static str {
        match self {
            Container::Fmp4 => "fmp4",
            Container::Webvtt => "webvtt",
            Container::Ts => "ts",
        }
    }

    /// The frozen file's suffix (`_SUFFIX`).
    pub fn suffix(self) -> &'static str {
        match self {
            Container::Fmp4 => ".m4s",
            Container::Webvtt => ".vtt",
            Container::Ts => ".ts",
        }
    }
}

/// `_detect_container(head, has_init)`: from a segment's first bytes (the whole segment when
/// it is shorter than 512 bytes): a box a fragment starts with after an `EXT-X-MAP`, else
/// `WEBVTT`, else MPEG-TS sync bytes 188 bytes apart.
pub fn detect_container(head: &[u8], has_init: bool) -> Result<Container, SourceError> {
    let unsupported = || SourceError::new("unsupported-container");
    if has_init {
        let fragment_box = head.len() >= 8
            && [&b"moof"[..], b"styp", b"sidx", b"emsg", b"prft", b"free"].contains(&&head[4..8]);
        return if fragment_box {
            Ok(Container::Fmp4)
        } else {
            Err(unsupported())
        };
    }
    if head.starts_with(b"WEBVTT") {
        return Ok(Container::Webvtt);
    }
    if head.first() == Some(&0x47) && (head.len() < 376 || head[188] == 0x47) {
        return Ok(Container::Ts);
    }
    Err(unsupported())
}

/// `_check_init(path)`: an initialization segment starts with `ftyp` or `moov`.
pub fn check_init(path: &Path) -> Result<(), SourceError> {
    let head = File::open(path)
        .and_then(|mut file| read_full(&mut file, 8))
        .map_err(|_| SourceError::new("source-read-failed"))?;
    if head.len() < 8 || !(&head[4..8] == b"ftyp" || &head[4..8] == b"moov") {
        return Err(SourceError::new("unsupported-container"));
    }
    Ok(())
}

/// `_probe` with the real `ffprobe`: both invocations limited to one decoder thread
/// (`-threads 1`), so the probe pool, not ffprobe, decides how many cores are busy;
/// 30 s each; recorded by the tool runner with their output.
pub struct FfprobeProber<'r> {
    runner: &'r ToolRunner,
    program: String,
}

impl std::fmt::Debug for FfprobeProber<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfprobeProber")
            .field("program", &self.program)
            .finish_non_exhaustive()
    }
}

impl<'r> FfprobeProber<'r> {
    /// `ffprobe` from `PATH`.
    pub fn new(runner: &'r ToolRunner) -> Self {
        Self::with_program(runner, "ffprobe")
    }

    /// Another `ffprobe` binary (the tests' stand-in); the argv is otherwise the same.
    pub fn with_program(runner: &'r ToolRunner, program: &str) -> Self {
        FfprobeProber {
            runner,
            program: program.to_string(),
        }
    }

    /// `["ffprobe", "-v", "error", "-threads", "1", "-show_streams", "-show_entries",
    /// "stream=index,codec_type,codec_name,profile,width,height", "-of", "json", path]`.
    pub fn streams_argv(&self, path: &Path) -> Vec<String> {
        [
            self.program.as_str(),
            "-v",
            "error",
            "-threads",
            "1",
            "-show_streams",
            "-show_entries",
            "stream=index,codec_type,codec_name,profile,width,height",
            "-of",
            "json",
        ]
        .iter()
        .map(ToString::to_string)
        .chain([path.to_string_lossy().into_owned()])
        .collect()
    }

    /// `["ffprobe", "-v", "error", "-threads", "1", "-select_streams", "v:0", "-read_intervals",
    /// "%+#1", "-show_packets", "-show_entries", "packet=flags", "-of", "json", path]`.
    pub fn packets_argv(&self, path: &Path) -> Vec<String> {
        [
            self.program.as_str(),
            "-v",
            "error",
            "-threads",
            "1",
            "-select_streams",
            "v:0",
            "-read_intervals",
            "%+#1",
            "-show_packets",
            "-show_entries",
            "packet=flags",
            "-of",
            "json",
        ]
        .iter()
        .map(ToString::to_string)
        .chain([path.to_string_lossy().into_owned()])
        .collect()
    }

    fn run(&self, argv: &[String]) -> Result<crate::toolrun::Completed, Failure> {
        let options = RunOptions {
            text: false,
            timeout: Some(Duration::from_secs(30)),
            ..RunOptions::default()
        };
        // The runner turns a timeout or a spawn failure into a ToolError.
        self.runner
            .run_tool(argv, "ffprobe", &options)
            .map_err(|_| Failure::source("media-probe-failed"))
    }

    /// Both ffprobe passes over `probe_path`.
    fn inspect(&self, probe_path: &Path) -> Result<Vec<ProbeStream>, Failure> {
        let completed = self.run(&self.streams_argv(probe_path))?;
        if completed.returncode != Some(0) {
            return Err(Failure::source("media-probe-failed"));
        }
        let streams = json_list(&completed.stdout, "streams")?;
        if streams.is_empty() {
            return Err(Failure::source("media-probe-failed"));
        }
        if streams.iter().any(|stream| {
            stream.get("codec_name") == Some(&Value::from("aac"))
                && stream.get("profile") != Some(&Value::from("LC"))
        }) {
            return Err(Failure::source("unsupported-aac-profile"));
        }
        if streams
            .iter()
            .any(|stream| stream.get("codec_type") == Some(&Value::from("video")))
        {
            let packet = self.run(&self.packets_argv(probe_path))?;
            let packets = if packet.returncode == Some(0) {
                json_list(&packet.stdout, "packets")?
            } else {
                Vec::new()
            };
            let key_frame = match packets.first().and_then(|first| first.get("flags")) {
                None => false,
                Some(Value::String(flags)) => flags.contains('K'),
                Some(Value::Array(flags)) => flags.contains(&Value::from("K")),
                Some(Value::Object(flags)) => flags.contains_key("K"),
                Some(_) => return Err(Failure::Escaped(Escaped::new("TypeError"))),
            };
            if !key_frame {
                return Err(Failure::source("non-independent-video"));
            }
        }
        Ok(streams)
    }
}

/// `json.loads(output).get(key, [])` as a list of objects: an empty (or false) value is an
/// empty list; output that is not JSON is `media-probe-failed` (Python's `ValueError`); a
/// top-level value without `.get`, or items that are not objects, escape as Python's
/// `AttributeError` (a number where a list belongs, its `TypeError`).
fn json_list(output: &[u8], key: &str) -> Result<Vec<ProbeStream>, Failure> {
    let value: Value =
        serde_json::from_slice(output).map_err(|_| Failure::source("media-probe-failed"))?;
    let Value::Object(map) = value else {
        return Err(Failure::Escaped(Escaped::new("AttributeError")));
    };
    match map.get(key) {
        None | Some(Value::Null | Value::Bool(false)) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Object(stream) => Ok(stream.clone()),
                _ => Err(Failure::Escaped(Escaped::new("AttributeError"))),
            })
            .collect(),
        Some(Value::String(text)) if text.is_empty() => Ok(Vec::new()),
        Some(Value::Object(object)) if object.is_empty() => Ok(Vec::new()),
        Some(Value::Number(number)) if number.as_f64() == Some(0.0) => Ok(Vec::new()),
        Some(Value::String(_) | Value::Object(_)) => {
            Err(Failure::Escaped(Escaped::new("AttributeError")))
        }
        Some(_) => Err(Failure::Escaped(Escaped::new("TypeError"))),
    }
}

impl Prober for FfprobeProber<'_> {
    fn probe(&self, path: &Path, init_path: Option<&Path>) -> Result<Vec<ProbeStream>, Failure> {
        let head = File::open(path)
            .and_then(|mut file| read_full(&mut file, 16))
            .map_err(|_| Failure::source("media-probe-failed"))?;
        if head.starts_with(b"WEBVTT") {
            // A subtitle segment: small, validated whole.
            let text =
                std::fs::read(path).map_err(|_| Failure::source("unsupported-subtitle-profile"))?;
            let text = text.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&text);
            std::str::from_utf8(text)
                .map_err(|_| Failure::source("unsupported-subtitle-profile"))?;
            let mut stream = ProbeStream::new();
            stream.insert("index".into(), 0.into());
            stream.insert("codec_type".into(), "subtitle".into());
            stream.insert("codec_name".into(), "webvtt".into());
            return Ok(vec![stream]);
        }
        let Some(init_path) = init_path else {
            return self.inspect(path);
        };
        // An initialization segment is small; media segments are never read whole.
        let init =
            std::fs::read(init_path).map_err(|_| Failure::Escaped(Escaped::new("OSError")))?;
        // Encrypted sample entries are forbidden even when the playlist has no KEY tag.
        if [&b"encv"[..], b"enca", b"senc"]
            .iter()
            .any(|token| init.windows(4).any(|window| window == *token))
        {
            return Err(Failure::source("encrypted-source-unsupported"));
        }
        let temporary = with_suffix(path, ".probe.mp4");
        copy_with_init(&temporary, &init, path).map_err(Failure::Source)?;
        let result = self.inspect(&temporary);
        let _ = std::fs::remove_file(&temporary);
        result
    }
}

/// The probe's temporary copy: the initialization segment, then the segment streamed in
/// 1 MiB chunks (`shutil.copyfileobj`).
fn copy_with_init(temporary: &Path, init: &[u8], segment: &Path) -> Result<(), SourceError> {
    let mut out = open_exclusive(temporary)?;
    let mut copy = || -> io::Result<()> {
        let mut source = File::open(segment)?;
        out.write_all(init)?;
        let mut buffer = vec![0; LOCAL_CHUNK];
        loop {
            let count = source.read(&mut buffer)?;
            if count == 0 {
                return Ok(());
            }
            out.write_all(&buffer[..count])?;
        }
    };
    copy().map_err(|_| SourceError::new("snapshot-write-failed"))
}
