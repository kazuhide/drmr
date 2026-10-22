//! Media probing, remuxing, timing, and decoded-content verification.
//!
//! The `MediaEngine` interface supports subprocess and native implementations.
//! Raw key files are private, secret values are hidden from debug output and wiped
//! on drop, and key-bearing tool calls use the secure runner.

mod files;
pub(crate) use files::{create_output_file, write_output_file};
mod parse;
mod playlist;
pub mod python;
mod subprocess;
mod timing;

pub use crate::mp4::{pssh_box_present, split_fragmented_mp4};
pub use files::{codec_from_init, concatenate, write_raw_key_file};
pub use parse::{
    packets_from_probe, parse_fragment_boundary, parse_init_probe, parse_tenc_constant_iv_hex,
    payload_hashes_from_probe, payloads_from_probe, segment_timing_from_probe,
};
pub use playlist::{
    extract_hls_iv_hex_values, localize_hls_master_http, localize_hls_media_playlist,
    localize_hls_media_playlist_http,
};
pub use subprocess::{
    FFMPEG, FFPROBE, FRAMEMD5_MAX_OUTPUT_CHARS, MEDIA_MAX_OUTPUT_CHARS, MP4DUMP,
    PROBE_MAX_OUTPUT_CHARS, Subprocess,
};
pub use timing::{default_timing_tolerance, same_timing};

use num_rational::BigRational;
use serde_json::Value;
use std::fmt;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// `media::HlsMediaError`: a classification token (`media-tool-failed`,
/// `unsupported-timebase`, …), never a value from the media.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct HlsMediaError(pub String);

impl HlsMediaError {
    /// An `HlsMediaError` with this token.
    pub fn new(token: &str) -> Self {
        HlsMediaError(token.to_string())
    }

    /// The token.
    pub fn token(&self) -> &str {
        &self.0
    }
}

/// A failure of a media helper, by the Python exception class the helper raises.
///
/// `Display` is Python's `str(exception)` where the callers print it (`RuntimeError` messages go
/// into the reports as `FAIL: <message>`).
#[derive(Debug, thiserror::Error)]
pub enum MediaError {
    /// `HlsMediaError(token)` (a `RuntimeError` subclass).
    #[error("{0}")]
    Hls(#[from] HlsMediaError),
    /// A `RuntimeError` with its message (`media_inspect`'s tool failures).
    #[error("{0}")]
    Runtime(String),
    /// `toolrun.ToolError` (a `RuntimeError` subclass): `<tool>: timeout|unavailable`.
    #[error("{0}")]
    Tool(#[from] crate::toolrun::ToolError),
    /// A `ValueError`: malformed tool output (`json.JSONDecodeError`, `int` / `float` /
    /// `Fraction` of a bad literal, a bad bracketed byte array, text that is not UTF-8).
    #[error("{0}")]
    Value(String),
    /// A `KeyError`: a field the parse requires is missing (`'streams'`).
    #[error("{0}")]
    Key(String),
    /// An `OSError`: a file could not be read or written.
    #[error("{0}")]
    Os(std::io::Error),
    /// An exception of another class (`TypeError`, `AttributeError`, `IndexError`,
    /// `ZeroDivisionError`, `OverflowError`, `re.error`), named by `class`. The pipelines'
    /// handlers let these through; a caller that does catch one (the console's
    /// `except (…, TypeError, …)`) matches [`MediaError::python_class`].
    #[error("{message}")]
    Uncaught {
        /// The Python class name.
        class: &'static str,
        /// The classified error message.
        message: String,
    },
}

impl MediaError {
    /// A [`MediaError::Uncaught`] of Python class `class`.
    pub fn uncaught(class: &'static str, message: String) -> Self {
        MediaError::Uncaught { class, message }
    }

    /// The Python exception class.
    pub fn python_class(&self) -> &'static str {
        match self {
            MediaError::Hls(_) => "HlsMediaError",
            MediaError::Runtime(_) => "RuntimeError",
            MediaError::Tool(_) => "ToolError",
            MediaError::Value(_) => "ValueError",
            MediaError::Key(_) => "KeyError",
            MediaError::Os(_) => "OSError",
            MediaError::Uncaught { class, .. } => class,
        }
    }

    /// Whether Python's `except RuntimeError` catches it (`HlsMediaError`, `ToolError` and
    /// `RuntimeError` itself).
    pub fn is_runtime_error(&self) -> bool {
        matches!(
            self,
            MediaError::Hls(_) | MediaError::Runtime(_) | MediaError::Tool(_)
        )
    }

    /// The `HlsMediaError` token, when it is one.
    pub fn hls_token(&self) -> Option<&str> {
        match self {
            MediaError::Hls(error) => Some(error.token()),
            _ => None,
        }
    }
}

impl From<std::io::Error> for MediaError {
    fn from(error: std::io::Error) -> Self {
        MediaError::Os(error)
    }
}

/// Hex text that is key material — the CBCS constant IV read from a `tenc` box or an
/// `#EXT-X-KEY` line. It is compared, never printed: `Debug` shows its length only, and the
/// text is wiped on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct HexSecret(Zeroizing<String>);

impl HexSecret {
    /// Wrap hex text.
    pub fn new(text: String) -> Self {
        HexSecret(Zeroizing::new(text))
    }

    /// The hex text, for a comparison or a write the caller keeps private.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for HexSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HexSecret(<{} hex characters>)", self.0.len())
    }
}

impl PartialEq<str> for HexSecret {
    fn eq(&self, other: &str) -> bool {
        self.expose() == other
    }
}

impl PartialEq<&str> for HexSecret {
    fn eq(&self, other: &&str) -> bool {
        self.expose() == *other
    }
}

/// `media::PacketInfo`: one stream's packets as ffprobe reports them, in the stream's
/// timescale (`1/time_base`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacketInfo {
    /// The denominator of the stream's `time_base` (whose numerator is 1).
    pub timescale: i64,
    /// Presentation timestamps, in packet order.
    pub pts: Vec<i64>,
    /// Decode timestamps, in packet order.
    pub dts: Vec<i64>,
    /// Durations, in packet order.
    pub durations: Vec<i64>,
    /// The stream's `avg_frame_rate` as ffprobe prints it (`24/1`, `0/0`), or `""`.
    pub frame_rate: String,
}

/// `media::probe_segment_timing`'s `{"duration_s", "packet_count"}`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SegmentTiming {
    /// ffprobe's `format.duration` (NaN when absent).
    pub duration_s: f64,
    /// The number of packets ffprobe listed.
    pub packet_count: usize,
}

/// `media::InitProbe`: the `schm` / `tenc` truth of an init segment (
/// init-probe). Public signaling plus the constant IV, which is a [`HexSecret`]; `Debug` prints
/// which fields are present, never the IV.
///
/// mp4dump prints `scheme_type` as a string and `default_Per_Sample_IV_Size` as an integer; a
/// value of another JSON type (which mp4dump never prints) keeps Python's comparisons: a
/// non-string scheme type as its JSON text (never equal to a 4CC), a per-sample size of
/// `false` / `0.0` as 0, any other non-integer as absent (never equal to 0).
#[derive(Clone, PartialEq, Eq, Default)]
pub struct InitProbe {
    /// The first `schm` box's `scheme_type` (`cenc`, `cbcs`), `None` when unencrypted.
    pub scheme_type: Option<String>,
    /// The first `tenc` box's constant IV (cbcs), `None` for per-sample IVs.
    pub constant_iv_hex: Option<HexSecret>,
    /// The first `tenc` box's `default_KID` (public).
    pub default_kid_hex: Option<String>,
    /// The first `tenc` box's `default_Per_Sample_IV_Size` (0 for cbcs).
    pub per_sample_iv_size: Option<i64>,
}

impl fmt::Debug for InitProbe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InitProbe")
            .field("scheme_type", &self.scheme_type)
            .field("constant_iv_hex", &self.constant_iv_hex)
            .field(
                "default_kid_hex",
                &self.default_kid_hex.as_ref().map(String::len),
            )
            .field("per_sample_iv_size", &self.per_sample_iv_size)
            .finish()
    }
}

/// `media::parse_fragment_boundary`'s `{"base_media_decode_time", "sample_count"}`: the
/// first `tfdt` and `trun` values as mp4dump printed them (JSON values, compared with Python's
/// `==`, so `281 == 281.0` as in the boundary check).
#[derive(Clone, Debug)]
pub struct FragmentBoundary {
    /// The first `tfdt` box's `base media decode time`.
    pub base_media_decode_time: Value,
    /// The first `trun` box's `sample count`.
    pub sample_count: Value,
}

impl FragmentBoundary {
    /// A boundary from integers (the native engine's box reader).
    pub fn new(base_media_decode_time: u64, sample_count: u32) -> Self {
        FragmentBoundary {
            base_media_decode_time: Value::from(base_media_decode_time),
            sample_count: Value::from(sample_count),
        }
    }
}

impl PartialEq for FragmentBoundary {
    fn eq(&self, other: &Self) -> bool {
        python::py_eq(&self.base_media_decode_time, &other.base_media_decode_time)
            && python::py_eq(&self.sample_count, &other.sample_count)
    }
}

/// Media operations used by source inspection and packaging.
/// Subprocess and native implementations share the same interface and error categories.
pub trait MediaEngine: Send + Sync {
    /// `media::probe_packets`: one stream's packet timing.
    fn probe_packets(&self, path: &Path, stream_index: i64) -> Result<PacketInfo, MediaError>;

    /// `media::packet_payloads`: one stream's packet payloads (never logged).
    fn packet_payloads(&self, path: &Path, stream_index: i64) -> Result<Vec<Vec<u8>>, MediaError>;

    /// The SHA-256 payload digests ffprobe reports per packet of one stream (the
    /// `-show_data_hash sha256` call of `subtitle payload verification`), as printed (`SHA256:<hex>`).
    // `subtitle payload verification`'s local `payloads(location, stream)` runs this ffprobe call
    // through `run_media`; native
    // engine can answer it. Compare the lists with `==` (ffprobe prints strings).
    fn packet_payload_hashes(
        &self,
        path: &Path,
        stream_index: i64,
    ) -> Result<Vec<Value>, MediaError>;

    /// `media::normalize_segment`: re-mux one stream with the transport `offset` (seconds).
    fn normalize_segment(
        &self,
        path: &Path,
        destination: &Path,
        stream_index: i64,
        offset: &BigRational,
        content_type: &str,
    ) -> Result<PathBuf, MediaError>;

    /// `media::decoded_digest`: SHA-256 hex of the decoded stream (PCM for audio, frame
    /// sizes and hashes otherwise).
    fn decoded_digest(
        &self,
        path: &Path,
        content_type: &str,
        stream_index: i64,
    ) -> Result<String, MediaError>;

    /// The mp4dump `--format json` box tree of a file (`initialization segment validation`'s
    /// `run_media(["mp4dump", …])`, bytes mode).
    // Initialization validation reads the tenc pattern from this tree (the first
    // `tenc`'s `default_crypt_byte_block` / `default_skip_byte_block`, 0 when absent);
    // `mp4::tenc_pattern` gives the same pair from the bytes for the native engine.
    fn box_tree(&self, path: &Path) -> Result<Value, MediaError>;

    /// `media::compute_framemd5`: ffmpeg's framemd5 report text.
    fn compute_framemd5(&self, path: &Path) -> Result<String, MediaError>;

    /// `media::probe_segment_timing`.
    fn probe_segment_timing(&self, path: &Path) -> Result<SegmentTiming, MediaError>;

    /// `media::read_tenc_constant_iv_hex`.
    fn read_tenc_constant_iv_hex(
        &self,
        init_segment_path: &Path,
    ) -> Result<Option<HexSecret>, MediaError>;

    /// `media::probe_init_segment`.
    fn probe_init_segment(&self, init_segment_path: &Path) -> Result<InitProbe, MediaError>;

    /// `media::read_fragment_boundary`.
    fn read_fragment_boundary(
        &self,
        segment_path: &Path,
    ) -> Result<Option<FragmentBoundary>, MediaError>;

    /// `media::decode_media_to_pcm`: canonical PCM (`s16le`, stereo, 48 kHz).
    fn decode_media_to_pcm(
        &self,
        input_path: &Path,
        output_path: &Path,
    ) -> Result<PathBuf, MediaError>;

    /// `media::decode_hls_media_playlist` (`mode` `pcm` or `framemd5`; anything else is
    /// Python's `ValueError`).
    fn decode_hls_media_playlist(
        &self,
        localized_playlist_path: &Path,
        output_path: &Path,
        mode: &str,
    ) -> Result<PathBuf, MediaError>;

    /// `media::measure_segment_durations_s`: each fragment's duration in seconds — the
    /// `tfdt` differences, and the last one from ffprobe's duration of `init || last segment`.
    fn measure_segment_durations_s(
        &self,
        init_path: &Path,
        segment_paths: &[PathBuf],
        timescale: i64,
    ) -> Result<Vec<f64>, MediaError> {
        timing::measure_segment_durations_s(self, init_path, segment_paths, timescale)
    }

    /// Alias for [`MediaEngine::compute_framemd5`].
    fn framemd5(&self, path: &Path) -> Result<String, MediaError> {
        self.compute_framemd5(path)
    }

    /// Alias for [`MediaEngine::decode_media_to_pcm`].
    fn decode_pcm(&self, input_path: &Path, output_path: &Path) -> Result<PathBuf, MediaError> {
        self.decode_media_to_pcm(input_path, output_path)
    }

    /// Alias for [`MediaEngine::measure_segment_durations_s`].
    fn measure_durations(
        &self,
        init_path: &Path,
        segment_paths: &[PathBuf],
        timescale: i64,
    ) -> Result<Vec<f64>, MediaError> {
        self.measure_segment_durations_s(init_path, segment_paths, timescale)
    }
}
