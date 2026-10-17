//! Bounded, credential-safe admission and freezing of clear VOD HLS sources.
//!
//! Source addresses and original playlists stay in private snapshot files. Public
//! records use allowlists, and errors expose classification tokens only. Resource IDs
//! are assigned in playlist order before concurrent downloads start. Separate bounded
//! pools handle fetching and inspection; one lock protects the byte budget.
//!
//! On failure, new work stops, active work drains, and the earliest submitted failure
//! is reimplemented before the partial snapshot is removed. Each network attempt uses a
//! validated pinned address, bounded socket operations, and verified TLS.

mod analyze;
mod fetch;
pub mod fsum;
pub mod http;
pub mod netaddr;
pub mod platform;
mod playlist;
mod policy;
mod probe;
pub mod pypath;
pub mod pytext;
pub mod pyurl;
mod verify;

pub use crate::m3u8::SourceError;
pub use analyze::{AnalyzeOptions, Direct, FetchHooks, analyze_hls, analyze_hls_with};
pub use fetch::{ByteRange, Fetched, Fetcher};
pub use playlist::{COMMON, MASTER, MEDIA, PlaylistKind, lines, positive_int};
pub use policy::{FetchPolicy, MAX_CONNECTIONS, MAX_PROBE_WORKERS, SourceRequest};
pub use probe::{Container, FfprobeProber, ProbeStream, Prober, check_init, detect_container};
pub use verify::{read_analysis, verify_source_snapshot};

use crate::concurrency::UnitError;
use pyurl::{urljoin, urlsplit};
use serde_json::{Map, Value};
use std::fmt;
use std::path::{Path, PathBuf};

/// An unclassified source error, preserved by class name: the CLI
/// reports `source-config-invalid` (exit 2) for an `OSError`, `ValueError` or `TypeError` and
/// `source-infrastructure-error` (exit 2) for any other class (`the analysis command`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Escaped {
    /// The Python class: `OSError`, `ValueError`, `TypeError`, `AttributeError`, `KeyError`,
    /// `OverflowError`, `RuntimeError` (a symlink loop in `Path.resolve`) or `Cancelled`.
    pub class: &'static str,
}

impl Escaped {
    /// An escaping exception of `class`.
    pub fn new(class: &'static str) -> Self {
        Escaped { class }
    }

    /// Whether the CLI's `except (ValueError, TypeError, OSError)` catches it.
    pub fn is_config_class(self) -> bool {
        matches!(self.class, "OSError" | "ValueError" | "TypeError")
    }
}

impl fmt::Display for Escaped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.class)
    }
}

/// How an analysis, a fetch or a unit of one failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    /// A classification token.
    #[error("{0}")]
    Source(SourceError),
    /// The unit stopped because the analysis is stopping (`concurrency.Cancelled`).
    #[error("cancelled")]
    Cancelled,
    /// An unclassified exception escaping as it is.
    #[error("{0}")]
    Escaped(Escaped),
}

impl Failure {
    /// The token, when this is a [`SourceError`].
    pub fn token(&self) -> Option<&str> {
        match self {
            Failure::Source(error) => Some(error.token()),
            _ => None,
        }
    }

    pub(crate) fn source(name: &str) -> Failure {
        Failure::Source(SourceError::new(name))
    }
}

impl From<SourceError> for Failure {
    fn from(error: SourceError) -> Self {
        Failure::Source(error)
    }
}

impl UnitError for Failure {
    fn cancelled() -> Self {
        Failure::Cancelled
    }

    fn is_cancelled(&self) -> bool {
        matches!(self, Failure::Cancelled)
    }
}

/// `_origin(url)`: `scheme://host[:port]` with the host in lower case (an IPv6 address in
/// brackets) and the scheme's default port omitted; a URL without a host, with credentials or
/// a fragment, or that does not parse, is `source-not-allowed`.
pub fn origin(url: &str) -> Result<String, SourceError> {
    let refused = || SourceError::new("source-not-allowed");
    let parts = urlsplit(url, "").map_err(|_| refused())?;
    let Some(host) = parts.hostname() else {
        return Err(refused());
    };
    let has = |value: Option<&str>| value.is_some_and(|text| !text.is_empty());
    if has(parts.username()) || has(parts.password()) || !parts.fragment.is_empty() {
        return Err(refused());
    }
    let default_port = if parts.scheme == "https" { 443 } else { 80 };
    let port = match parts.port().map_err(|_| refused())? {
        Some(port) if port != 0 => port,
        _ => default_port,
    };
    let host = host.to_lowercase();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let suffix = if port == default_port {
        String::new()
    } else {
        format!(":{port}")
    };
    Ok(format!("{}://{host}{suffix}", parts.scheme))
}

/// `_child_url(base, reference)`: `urljoin`, refusing control characters in the reference
/// and a remote playlist's reference to a `file:` URL (a remotely controlled playlist never
/// gains the operator's local-file capability). A URL the parser rejects is Python's
/// `ValueError`, escaping as it is.
pub fn child_url(base: &str, reference: &str) -> Result<String, Failure> {
    if reference.chars().any(|c| u32::from(c) < 32) {
        return Err(Failure::source("source-not-allowed"));
    }
    let value_error = |_| Failure::Escaped(Escaped::new("ValueError"));
    let result = urljoin(base, reference).map_err(value_error)?;
    let base_scheme = urlsplit(base, "").map_err(value_error)?.scheme;
    if (base_scheme == "https" || base_scheme == "http")
        && urlsplit(&result, "").map_err(value_error)?.scheme == "file"
    {
        return Err(Failure::source("source-not-allowed"));
    }
    Ok(result)
}

/// One frozen media segment (`SourceSegment`). `Debug` does not show the path.
#[derive(Clone, PartialEq)]
pub struct SourceSegment {
    /// Position in the playlist, from 0.
    pub index: i64,
    /// The frozen file.
    pub path: PathBuf,
    /// The `EXTINF` duration.
    pub duration: f64,
    /// SHA-256 of the file.
    pub sha256: String,
    /// The bytes of the source resource this segment is.
    pub byte_range: Option<ByteRange>,
}

impl fmt::Debug for SourceSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceSegment")
            .field("index", &self.index)
            .field("duration", &self.duration)
            .field("sha256", &self.sha256)
            .field("byte_range", &self.byte_range)
            .finish_non_exhaustive()
    }
}

/// One admitted track (`SourceTrack`). `Debug` does not show paths.
#[derive(Clone, PartialEq)]
pub struct SourceTrack {
    /// `v000`, `a000`, `t000`, … in census order.
    pub id: String,
    /// `video`, `audio` or `text`.
    pub content_type: String,
    /// `ts`, `fmp4`, `webvtt` or `imsc`.
    pub container: String,
    /// `h264`, `hevc`, `aac`, `webvtt` or `ttml`.
    pub codec: String,
    /// The segments in playlist order.
    pub segments: Vec<SourceSegment>,
    /// The frozen initialization segment.
    pub init_path: Option<PathBuf>,
    /// SHA-256 of the initialization segment.
    pub init_sha256: Option<String>,
    /// `LANGUAGE` (`und`).
    pub language: String,
    /// `CHARACTERISTICS` (`main`).
    pub role: String,
    /// The rendition group.
    pub group_id: String,
    /// `NAME`.
    pub name: String,
    /// Width from the probe.
    pub width: Option<u64>,
    /// Height from the probe.
    pub height: Option<u64>,
    /// The variant's `BANDWIDTH`.
    pub bandwidth: Option<u64>,
    /// The variant's `CODECS`.
    pub codecs: String,
    /// The stream's index in its segments.
    pub stream_index: i64,
    /// The frozen media playlist's id.
    pub playlist_id: String,
    /// Whether video and audio share the segments.
    pub muxed: bool,
    /// The variant's `AUDIO` group.
    pub audio_group: String,
    /// The variant's `SUBTITLES` group.
    pub subtitle_group: String,
    /// The codec profile from the probe.
    pub profile: String,
    /// `DEFAULT=YES`.
    pub is_default: bool,
    /// `AUTOSELECT=YES` (implied by `DEFAULT=YES`).
    pub autoselect: bool,
}

impl fmt::Debug for SourceTrack {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceTrack")
            .field("id", &self.id)
            .field("content_type", &self.content_type)
            .field("container", &self.container)
            .field("codec", &self.codec)
            .field("segments", &self.segments.len())
            .finish_non_exhaustive()
    }
}

fn optional<T: Into<Value>>(value: Option<T>) -> Value {
    value.map_or(Value::Null, Into::into)
}

impl SourceTrack {
    /// `duration`: `math.fsum` of the segment durations. Tracks come from `analyze_hls`
    /// (bounded durations) or `read_analysis` (which refuses durations whose sum overflows),
    /// so the sum is always finite.
    pub fn duration(&self) -> f64 {
        fsum::fsum(self.segments.iter().map(|segment| segment.duration)).unwrap_or(f64::NAN)
    }

    /// `to_public_dict`: the allowlisted public fields, in field insertion order.
    pub fn to_public_dict(&self) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert("id".into(), self.id.clone().into());
        out.insert("content_type".into(), self.content_type.clone().into());
        out.insert("container".into(), self.container.clone().into());
        out.insert("codec".into(), self.codec.clone().into());
        out.insert("codecs".into(), self.codecs.clone().into());
        out.insert("language".into(), self.language.clone().into());
        out.insert("role".into(), self.role.clone().into());
        out.insert("group_id".into(), self.group_id.clone().into());
        out.insert("name".into(), self.name.clone().into());
        out.insert("width".into(), optional(self.width));
        out.insert("height".into(), optional(self.height));
        out.insert("bandwidth".into(), optional(self.bandwidth));
        out.insert("segment_count".into(), self.segments.len().into());
        out.insert("duration".into(), self.duration().into());
        out.insert("playlist_id".into(), self.playlist_id.clone().into());
        out.insert("muxed".into(), self.muxed.into());
        out.insert("audio_group".into(), self.audio_group.clone().into());
        out.insert("subtitle_group".into(), self.subtitle_group.clone().into());
        out.insert("profile".into(), self.profile.clone().into());
        out.insert("is_default".into(), self.is_default.into());
        out.insert("autoselect".into(), self.autoselect.into());
        out
    }

    /// `_track_private(track, directory)`: the public fields plus `stream_index`, `init_path`,
    /// `init_sha256` and the segments, paths relative to `directory`. `None` when a path is
    /// not under `directory` (Python's `ValueError`).
    pub fn to_private_dict(&self, directory: &Path) -> Option<Map<String, Value>> {
        let mut out = self.to_public_dict();
        out.insert("stream_index".into(), self.stream_index.into());
        let init = match &self.init_path {
            Some(path) => Value::String(pypath::relative_posix(path, directory)?),
            None => Value::Null,
        };
        out.insert("init_path".into(), init);
        out.insert("init_sha256".into(), optional(self.init_sha256.clone()));
        let mut segments = Vec::with_capacity(self.segments.len());
        for segment in &self.segments {
            let mut row = Map::new();
            row.insert("index".into(), segment.index.into());
            row.insert(
                "path".into(),
                pypath::relative_posix(&segment.path, directory)?.into(),
            );
            row.insert("duration".into(), segment.duration.into());
            row.insert("sha256".into(), segment.sha256.clone().into());
            row.insert(
                "byte_range".into(),
                segment.byte_range.map_or(Value::Null, |range| {
                    Value::Array(vec![range.offset.into(), range.length.into()])
                }),
            );
            segments.push(Value::Object(row));
        }
        out.insert("segments".into(), Value::Array(segments));
        Some(out)
    }
}

/// An admitted, frozen source (`SourceAnalysis`). `Debug` does not show the snapshot path.
#[derive(Clone, PartialEq)]
pub struct SourceAnalysis {
    /// `"1"`.
    pub schema_version: String,
    /// `"hls-clear"`.
    pub source_kind: String,
    /// `hls-` and the first 24 hex digits of the digest (as recorded).
    pub analysis_id: String,
    /// The asset id.
    pub asset_id: String,
    /// Always `true` for a returned analysis.
    pub admitted: bool,
    /// The tracks in census order.
    pub tracks: Vec<SourceTrack>,
    /// Always empty for a returned analysis.
    pub findings: Vec<Value>,
    /// `output/<asset_id>/clear/source/analysis.json`.
    pub snapshot_path: PathBuf,
    /// SHA-256 of the canonical private record.
    pub snapshot_digest: String,
}

impl fmt::Debug for SourceAnalysis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceAnalysis")
            .field("analysis_id", &self.analysis_id)
            .field("asset_id", &self.asset_id)
            .field("admitted", &self.admitted)
            .field("tracks", &self.tracks)
            .field("snapshot_digest", &self.snapshot_digest)
            .finish_non_exhaustive()
    }
}

impl SourceAnalysis {
    /// `output_dir`: `output/<asset_id>/` (the snapshot path's third parent).
    pub fn output_dir(&self) -> Option<&Path> {
        self.snapshot_path.ancestors().nth(3)
    }

    /// `to_public_dict`: the first seven fields and the digest, in field insertion order.
    pub fn to_public_dict(&self) -> Map<String, Value> {
        let mut out = Map::new();
        out.insert("schema_version".into(), self.schema_version.clone().into());
        out.insert("source_kind".into(), self.source_kind.clone().into());
        out.insert("analysis_id".into(), self.analysis_id.clone().into());
        out.insert("asset_id".into(), self.asset_id.clone().into());
        out.insert("admitted".into(), self.admitted.into());
        out.insert(
            "snapshot_digest".into(),
            self.snapshot_digest.clone().into(),
        );
        out.insert(
            "tracks".into(),
            Value::Array(
                self.tracks
                    .iter()
                    .map(|track| Value::Object(track.to_public_dict()))
                    .collect(),
            ),
        );
        out.insert("findings".into(), Value::Array(self.findings.clone()));
        out
    }
}
