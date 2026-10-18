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
    let prefix = &prefix[..count];
    let text = prefix.strip_prefix(b"\xef\xbb\xbf").unwrap_or(prefix);
    let text = text.trim_ascii_start();
    if text.starts_with(b"#EXTM3U") {
        manifests::inspect_hls(path, &read_manifest(path)?)
    } else if text.starts_with(b"<") {
        manifests::inspect_dash(&read_manifest(path)?)
    } else if prefix.get(4..8).is_some_and(|kind| {
        matches!(
            kind,
            b"ftyp" | b"moov" | b"styp" | b"free" | b"wide" | b"mdat" | b"sidx"
        )
    }) {
        container::inspect_mp4(path)
    } else {
        Err(InspectionError("unrecognized-input"))
    }
}
