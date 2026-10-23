//! Bounded, read-only metadata inspection and non-executing conversion plans.
//!
//! Manifest declarations are reported separately from container observations. Neither
//! proves that every media sample is valid or that a DRM license will play on a device.

mod container;
mod details;
mod manifests;
mod planning;
mod processing;

pub use details::{
    Cipher, DescriptionEvidence, EncryptionUnit, FactStatus, HlsKeyMethod, InspectionDetails,
    PatternDescription, PeriodCount, ProtectionDescription, SampleCoverage,
};
pub use planning::{ConversionPlan, OutputFormat, TargetProtection, plan};
pub use processing::{
    ActualProcessing, CandidateRoute, InputPreparation, PlannedProcessing, PlanningStatus,
    ProcessingRouteId, RequiredTool,
};

use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::Read;
use std::path::Path;

/// Maximum size of one textual manifest inspected in memory.
pub const MAX_MANIFEST_BYTES: u64 = 4 * 1024 * 1024;

/// Presentation format or media container.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputFormat {
    Mp4,
    Dash,
    Hls,
}

impl InputFormat {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Dash => "dash",
            Self::Hls => "hls",
        }
    }
}

/// Observed or declared protection. Unknown is never interpreted as clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protection {
    Clear,
    Cenc,
    Cbcs,
    HlsAes128,
    Cens,
    Cbc1,
    Mixed,
    Unknown,
}

impl Protection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Cenc => "cenc",
            Self::Cbcs => "cbcs",
            Self::HlsAes128 => "hls-aes128",
            Self::Cens => "cens",
            Self::Cbc1 => "cbc1",
            Self::Mixed => "mixed",
            Self::Unknown => "unknown",
        }
    }

    pub fn aggregate(values: impl IntoIterator<Item = Self>) -> Self {
        let values: std::collections::BTreeSet<_> = values.into_iter().collect();
        if values.len() == 1 {
            *values.first().expect("one value")
        } else if values.is_empty() {
            Self::Unknown
        } else {
            Self::Mixed
        }
    }
}

/// What the inspector actually read; full media verification is a separate operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Evidence {
    Manifest,
    ContainerMetadata,
}

/// Media role, independent of protection and DRM signaling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrackKind {
    Video,
    Audio,
    Text,
    Unknown,
}
impl TrackKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Text => "text",
            Self::Unknown => "unknown",
        }
    }
}

/// Counts of encrypted and skipped 16-byte blocks, never IV or key bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pattern {
    pub encrypted_blocks: u8,
    pub clear_blocks: u8,
}

/// One track or sample description. No key URI, license URL, IV, or PSSH payload is kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Track {
    pub id: String,
    pub kind: TrackKind,
    pub codec: Option<String>,
    pub protection: Protection,
    pub key_ids: Vec<String>,
    pub pattern: Option<Pattern>,
}

/// A bounded explanation token rather than untrusted input text or paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub code: String,
}

impl Finding {
    pub fn new(code: &'static str) -> Self {
        Self { code: code.into() }
    }
}

/// Versioned metadata report shared by local MP4, DASH, and HLS inspection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inspection {
    pub schema_version: u32,
    pub format: InputFormat,
    pub evidence: Evidence,
    pub protection: Protection,
    pub tracks: Vec<Track>,
    pub drm_systems: Vec<String>,
    pub findings: Vec<Finding>,
    pub media_verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<InspectionDetails>,
}

impl Inspection {
    pub fn new(
        format: InputFormat,
        evidence: Evidence,
        tracks: Vec<Track>,
        mut drm_systems: Vec<String>,
        findings: Vec<Finding>,
    ) -> Self {
        drm_systems.sort();
        drm_systems.dedup();
        Self {
            schema_version: 1,
            format,
            evidence,
            protection: Protection::aggregate(tracks.iter().map(|track| track.protection)),
            tracks,
            drm_systems,
            findings,
            media_verified: false,
            details: None,
        }
    }

    pub(super) fn with_details(mut self, periods: PeriodCount) -> Self {
        self.details = Some(InspectionDetails {
            track_protection: self
                .tracks
                .iter()
                .map(|track| ProtectionDescription::track(track, self.evidence))
                .collect(),
            periods,
        });
        self
    }
}

/// Classified failures never carry a source path, URL, or media contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InspectionError(pub &'static str);

/// Read a bounded local manifest; this never follows references or writes files.
pub(crate) fn read_manifest(path: &Path) -> Result<Vec<u8>, InspectionError> {
    let metadata = std::fs::metadata(path).map_err(|_| InspectionError("input-unreadable"))?;
    if !metadata.is_file() {
        return Err(InspectionError("input-not-regular-file"));
    }
    let file = File::open(path).map_err(|_| InspectionError("input-unreadable"))?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| InspectionError("input-unreadable"))?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err(InspectionError("manifest-size-limit"));
    }
    Ok(bytes)
}

/// Hint from at most 64 leading bytes; ordinary parsers still validate the complete source.
/// XML-like text can contain incidental MP4 box names. XML-forbidden controls
/// in the binary size field instead keep its MP4 interpretation; later payload
/// bytes cannot change this choice. Streaming callers wait for eight bytes before
/// choosing a format-specific body budget.
pub(crate) fn format_hint(prefix: &[u8]) -> Option<InputFormat> {
    let prefix = &prefix[..prefix.len().min(64)];
    let text = prefix
        .strip_prefix(b"\xef\xbb\xbf")
        .unwrap_or(prefix)
        .trim_ascii_start();
    let size_field = &prefix[..prefix.len().min(4)];
    if text.starts_with(b"<")
        && !size_field
            .iter()
            .any(|byte| *byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r'))
    {
        return Some(InputFormat::Dash);
    }
    if prefix.get(4..8).is_some_and(|kind| {
        matches!(
            kind,
            b"ftyp" | b"moov" | b"styp" | b"free" | b"wide" | b"mdat" | b"sidx"
        )
    }) {
        return Some(InputFormat::Mp4);
    }
    if text.starts_with(b"#EXTM3U") {
        Some(InputFormat::Hls)
    } else if text.starts_with(b"<") {
        Some(InputFormat::Dash)
    } else {
        None
    }
}

/// Detect and inspect a local file by its contents, without keys or external processes.
pub fn inspect(path: &Path) -> Result<Inspection, InspectionError> {
    if path.to_string_lossy().contains("://") {
        return Err(InspectionError("remote-input-requires-policy"));
    }
    let metadata = std::fs::metadata(path).map_err(|_| InspectionError("input-unreadable"))?;
    if !metadata.is_file() {
        return Err(InspectionError("input-not-regular-file"));
    }
    let mut file = File::open(path).map_err(|_| InspectionError("input-unreadable"))?;
    let mut prefix = [0_u8; 64];
    let count = file
        .read(&mut prefix)
        .map_err(|_| InspectionError("input-unreadable"))?;
    match format_hint(&prefix[..count]) {
        Some(InputFormat::Mp4) => container::inspect_mp4(path),
        Some(InputFormat::Hls) => manifests::inspect_hls(path, &read_manifest(path)?),
        Some(InputFormat::Dash) => manifests::inspect_dash(&read_manifest(path)?),
        None => Err(InspectionError("unrecognized-input")),
    }
}

/// Parse only retained metadata bytes; these helpers never acquire referenced media.
pub(crate) fn inspect_dash_metadata(bytes: &[u8]) -> Result<Inspection, InspectionError> {
    manifests::inspect_dash(bytes)
}
pub(crate) fn inspect_mp4_metadata(bytes: &[u8]) -> Result<Inspection, InspectionError> {
    container::inspect_metadata(bytes)
}
/// Master references are media playlists; only the entrypoint may be a master.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum HlsMetadataRole {
    Root,
    Media,
}
pub(crate) fn inspect_hls_metadata(
    bytes: &[u8],
    role: HlsMetadataRole,
    child: &mut dyn FnMut(&str) -> Result<Inspection, InspectionError>,
) -> Result<Inspection, InspectionError> {
    manifests::inspect_hls_metadata(bytes, role, child)
}

#[cfg(test)]
mod format_tests {
    use super::{InputFormat, format_hint};

    #[test]
    fn binary_mp4_box_types_precede_text_like_size_bytes() {
        for kind in [
            b"ftyp", b"moov", b"styp", b"free", b"wide", b"mdat", b"sidx",
        ] {
            for first in [0, b'#', b'<'] {
                let bytes = [[first, 0, 0, 8].as_slice(), kind].concat();
                assert_eq!(format_hint(&bytes), Some(InputFormat::Mp4));
            }
        }
        assert_eq!(format_hint(b"\0\0\0\x08junk"), None);
    }

    #[test]
    fn xml_comments_and_qualified_names_are_not_mp4_box_headers() {
        for kind in [
            b"ftyp", b"moov", b"styp", b"free", b"wide", b"mdat", b"sidx",
        ] {
            let comment = [b"<!--".as_slice(), kind, b" information-->\n<MPD/>"].concat();
            assert_eq!(format_hint(&comment), Some(InputFormat::Dash));
            let name = std::str::from_utf8(kind).unwrap();
            let qualified =
                format!("<abc{name}:MPD xmlns:abc{name}='urn:mpeg:dash:schema:mpd:2011'/>");
            assert_eq!(format_hint(qualified.as_bytes()), Some(InputFormat::Dash));
        }
    }

    #[test]
    fn xml_like_headers_do_not_change_hint_when_later_payload_is_invalid_xml() {
        let header = b"<!--wide";
        let body = [header.as_slice(), b"\0 invalid XML payload"].concat();
        assert_eq!(format_hint(header), Some(InputFormat::Dash));
        assert_eq!(format_hint(&body), Some(InputFormat::Dash));
        assert!(super::manifests::inspect_dash(&body).is_err());
    }

    #[test]
    fn tiny_text_and_bom_prefixes_remain_valid_without_reading_past_the_bound() {
        assert_eq!(format_hint(b"#EXTM3U"), Some(InputFormat::Hls));
        assert_eq!(format_hint(b"<MPD/>"), Some(InputFormat::Dash));
        assert_eq!(
            format_hint(b"\xef\xbb\xbf \n#EXTM3U"),
            Some(InputFormat::Hls)
        );
        assert_eq!(
            format_hint(b"\xef\xbb\xbf \n<MPD/>"),
            Some(InputFormat::Dash)
        );
        assert_eq!(format_hint(b"\xef\xbb"), None);
        assert_eq!(
            format_hint(&[vec![b' '; 64], b"<MPD/>".to_vec()].concat()),
            None
        );
    }
}
