//! Package clear HLS as a common CBCS presentation for three DRM systems.
//!
//! Sources remain immutable and manifests are published only after per-track checks pass.
//! Each source segment is remuxed, encrypted, and validated in isolation to preserve
//! boundaries. Shared bounded pools prepare and verify segments concurrently while
//! results and reports retain source-track order. Tool execution uses the secure runner.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::panic;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use num_bigint::BigInt;
use num_integer::Integer;
use num_rational::{BigRational, Ratio};
use num_traits::{Signed, ToPrimitive, Zero};
use regex::Regex;
use serde_json::{Value, json};

use crate::canonical::Dumps;
use crate::concurrency::{Pool, Unit, UnitError, default_workers, run_ordered};
use crate::config::KeyMaterial;
use crate::events::Record;
use crate::hls_cli;
pub use crate::hls_pipeline_deps::PipelineDeps;
use crate::manifest_gen::{
    OutputSegment, OutputTrack, PackagedPresentation, PrivateFairPlaySignaling,
};
use crate::manifest_support::fraction::Fraction;
use crate::manifest_support::py::PyErr;
use crate::media::python::{py_eq, true_divide, truthy};
use crate::media::{self, HexSecret, MediaError, PacketInfo};
use crate::packager;
use crate::pipeline_deps::EncryptCbcs;
use crate::playready::{self, PlayReadyObjects, WIDEVINE_SYSTEM_ID};
use crate::pycompat;
use crate::source::{Failure, SourceAnalysis, SourceSegment, SourceTrack, valid_track_id};

/// Python's `Fraction`: exact timeline arithmetic.
pub type Frac = Ratio<i128>;

/// `Fraction(n, d)`.
#[must_use]
pub fn frac(numerator: i128, denominator: i128) -> Frac {
    Frac::new(numerator, denominator)
}

/// `round(fraction)`: to the nearest integer, ties to even (`Fraction.__round__`).
#[must_use]
pub fn round_half_even(value: Frac) -> i128 {
    let (floor, remainder) = value.numer().div_mod_floor(value.denom());
    let twice = remainder * 2;
    if twice < *value.denom() || (twice == *value.denom() && floor.is_even()) {
        floor
    } else {
        floor + 1
    }
}

/// `Fraction("12.345")`: a decimal literal, exactly.
fn frac_from_decimal(text: &str) -> Option<Frac> {
    let (whole_part, fraction_part) = text.split_once('.').unwrap_or((text, ""));
    let digits = format!("{whole_part}{fraction_part}");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let numerator: i128 = digits.parse().ok()?;
    let denominator = 10_i128.checked_pow(u32::try_from(fraction_part.len()).ok()?)?;
    Some(frac(numerator, denominator))
}

/// A [`Frac`] as the manifest generator's [`Fraction`].
#[must_use]
pub fn to_fraction(value: Frac) -> Fraction {
    Fraction::new(BigInt::from(*value.numer()), BigInt::from(*value.denom()))
        .expect("a ratio's denominator is never zero")
}

/// A manifest [`Fraction`] as a [`Frac`]: every timeline this pipeline builds fits; one that
/// does not is Python's unbounded arithmetic outrunning the implementation (`OverflowError`).
fn from_fraction(value: &Fraction) -> Result<Frac, PipelineError> {
    match (value.numerator().to_i128(), value.denominator().to_i128()) {
        (Some(numerator), Some(denominator)) => Ok(frac(numerator, denominator)),
        _ => Err(PipelineError::Infrastructure(
            "OverflowError: a timeline beyond 128 bits".to_string(),
        )),
    }
}

/// `Fraction(2)`: the one common transport offset, as the media interface takes it.
fn transport_shift() -> BigRational {
    BigRational::from_integer(BigInt::from(2))
}

/// The three operator-supplied signaling values. `repr=False` on every field in Python: the
/// Widevine PSSH, the `LA_URL` and the `FairPlay` URI never print.
#[derive(Clone, PartialEq, Eq)]
pub struct DrmSignaling {
    pub widevine_pssh: Vec<u8>,
    pub playready_la_url: String,
    pub fairplay_uri: String,
}

impl fmt::Debug for DrmSignaling {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DrmSignaling()")
    }
}

/// The operator's choice among muxed audio renditions, bound to one analysis and snapshot.
///
/// The fields hold what the selection file holds, as Python's dataclass holds any object: an
/// id that is not the analysis's own string is stale, and an `audio_source_id` without Python
/// truth (`null`, `""`, `0`, …) is no choice.
#[derive(Clone, Debug, PartialEq)]
pub struct PackagingSelection {
    pub analysis_id: Value,
    pub snapshot_digest: Value,
    pub audio_source_id: Value,
}

impl PackagingSelection {
    /// A selection of text values (`None`: no audio choice).
    #[must_use]
    pub fn new(analysis_id: &str, snapshot_digest: &str, audio_source_id: Option<&str>) -> Self {
        PackagingSelection {
            analysis_id: Value::from(analysis_id),
            snapshot_digest: Value::from(snapshot_digest),
            audio_source_id: audio_source_id.map_or(Value::Null, Value::from),
        }
    }
}

/// One source track's outcome: `checks` is the plain list of the checks that passed, or the
/// failure token of a failed track; `segments` the per-segment correspondence rows.
#[derive(Clone, Debug, PartialEq)]
pub struct TrackResult {
    pub source_track_id: String,
    pub disposition: String,
    pub checks: Vec<String>,
    pub segments: Vec<Record>,
}

impl TrackResult {
    fn new(source_track_id: &str, disposition: &str, checks: &[&str]) -> Self {
        TrackResult {
            source_track_id: source_track_id.to_string(),
            disposition: disposition.to_string(),
            checks: checks.iter().map(ToString::to_string).collect(),
            segments: Vec::new(),
        }
    }

    #[must_use]
    pub fn to_public_dict(&self) -> Value {
        json!({
            "source_track_id": self.source_track_id,
            "disposition": self.disposition,
            "checks": self.checks,
            "segments": self.segments,
        })
    }
}

/// `reports/package-hls-summary.json`.
#[derive(Clone, Debug, PartialEq)]
pub struct PackageHlsReport {
    pub analysis_id: String,
    pub asset_id: String,
    pub success: bool,
    pub manifest_paths: Vec<PathBuf>,
    pub track_results: Vec<TrackResult>,
    pub findings: Vec<String>,
    pub report_path: Option<PathBuf>,
    pub schema_version: String,
    pub production_verified: bool,
}

impl PackageHlsReport {
    #[must_use]
    pub fn to_public_dict(&self) -> Value {
        let names: Vec<String> = self
            .manifest_paths
            .iter()
            .map(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        json!({
            "schema_version": self.schema_version,
            "report_kind": "package-hls",
            "analysis_id": self.analysis_id,
            "asset_id": self.asset_id,
            "success": self.success,
            "production_verified": false,
            "manifest_paths": names,
            "tracks": self.track_results.iter().map(TrackResult::to_public_dict).collect::<Vec<_>>(),
            "findings": self.findings,
            "evidence": {
                "packaging": if self.success { "measured-local" } else { "failed" },
                "real_license": "not-verified",
                "conformance": "not-verified",
            },
        })
    }
}

/// What `package_hls` raises, grouped by the Python exception classes `the packaging command`
/// tells apart.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PipelineError {
    /// `SourceError`: the token is reimplemented, exit 1.
    #[error("{0}")]
    Source(String),
    /// `ValueError`, `TypeError`, `OSError`: `package-config-invalid`, exit 2.
    #[error("{0}")]
    Value(String),
    /// Any other exception (`RuntimeError`, `KeyError`, `IndexError`, …):
    /// `package-infrastructure-error`, exit 2.
    #[error("{0}")]
    Infrastructure(String),
}

/// An error inside one track's conversion, by how `convert` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackError {
    /// `HlsMediaError`: its token is the finding.
    Media(String),
    /// `PackagerError`, `ValueError`, `OSError`, `KeyError`, `StopIteration`:
    /// the finding is `track-packaging-failed`.
    Failed(String),
    /// Every other exception propagates out of `package_hls` (`RuntimeError`, `ToolError`,
    /// `IndexError`, `TypeError`, …), as the CLI sorts it.
    Escaped(PipelineError),
    /// The pool's stop flag; never a finding.
    Cancelled,
}

impl UnitError for TrackError {
    fn cancelled() -> Self {
        TrackError::Cancelled
    }

    fn is_cancelled(&self) -> bool {
        matches!(self, TrackError::Cancelled)
    }
}

/// A media failure that propagates out of `package_hls`, by how `the packaging command` reports
/// its class: `ValueError`, `TypeError`, `OSError` → `package-config-invalid`; any other class
/// (`HlsMediaError` and `ToolError` are `RuntimeError`s, `KeyError`, `ZeroDivisionError`, …) →
/// `package-infrastructure-error`.
fn escaped_media(error: &MediaError) -> PipelineError {
    let class = error.python_class();
    let message = error.to_string();
    match class {
        "ValueError" | "TypeError" | "OSError" => PipelineError::Value(message),
        _ => PipelineError::Infrastructure(message),
    }
}

impl From<MediaError> for TrackError {
    fn from(error: MediaError) -> Self {
        match error {
            MediaError::Hls(error) => TrackError::Media(error.0),
            MediaError::Value(message) | MediaError::Key(message) => TrackError::Failed(message),
            MediaError::Os(error) => TrackError::Failed(error.to_string()),
            other => TrackError::Escaped(escaped_media(&other)),
        }
    }
}

/// `PackagerError`, its `ValueError`s and `OSError`s: all caught by `convert`.
impl From<packager::Error> for TrackError {
    fn from(error: packager::Error) -> Self {
        TrackError::Failed(error.to_string())
    }
}

impl From<std::io::Error> for TrackError {
    fn from(error: std::io::Error) -> Self {
        TrackError::Failed(error.to_string())
    }
}

/// An `OSError` outside a track's conversion (`package-config-invalid`).
fn os_value(error: &std::io::Error) -> PipelineError {
    PipelineError::Value(error.to_string())
}

/// A `hls_source` failure propagating out of `package_hls`: a `SourceError` keeps its token;
/// an unclassified exception is sorted by its class.
pub(crate) fn source_failure(failure: Failure) -> PipelineError {
    match failure {
        Failure::Source(error) => PipelineError::Source(error.token().to_string()),
        Failure::Escaped(escaped) if escaped.is_config_class() => {
            PipelineError::Value(escaped.class.to_string())
        }
        Failure::Escaped(escaped) => PipelineError::Infrastructure(escaped.class.to_string()),
        Failure::Cancelled => PipelineError::Infrastructure("Cancelled".to_string()),
    }
}

/// Read the public Widevine protobuf's repeated `key_id` field (number 2).
pub fn protobuf_kids(data: &[u8]) -> Result<Vec<Vec<u8>>, PipelineError> {
    let invalid = || PipelineError::Value("widevine-invalid-payload".to_string());
    let mut pos = 0usize;
    let mut kids = Vec::new();
    let varint = |pos: &mut usize| -> Result<u128, PipelineError> {
        let mut value: u128 = 0;
        for shift in (0..70).step_by(7) {
            let byte = *data.get(*pos).ok_or_else(invalid)?;
            *pos += 1;
            value |= u128::from(byte & 127) << shift;
            if byte & 128 == 0 {
                return Ok(value);
            }
        }
        Err(invalid())
    };
    while pos < data.len() {
        let tag = varint(&mut pos)?;
        let wire = tag & 7;
        if tag >> 3 == 0 {
            return Err(invalid());
        }
        match wire {
            0 => {
                varint(&mut pos)?;
            }
            1 | 5 => pos += if wire == 1 { 8 } else { 4 },
            2 => {
                let length = usize::try_from(varint(&mut pos)?).map_err(|_| invalid())?;
                if pos.checked_add(length).is_none_or(|end| end > data.len()) {
                    return Err(invalid());
                }
                let value = data[pos..pos + length].to_vec();
                pos += length;
                if tag >> 3 == 2 {
                    kids.push(value);
                }
            }
            _ => return Err(invalid()),
        }
        if pos > data.len() {
            return Err(invalid());
        }
    }
    Ok(kids)
}

/// One URI as `validate_signaling` checks it: no control character and no `"`, a scheme among
/// `schemes`, a host, no user information (`urllib.parse.urlsplit`, `CPython` 3.11's, through
/// [`hls_cli::urlsplit`]; its `ValueError` is the same token).
fn check_signaling_uri(value: &str, schemes: &[&str], name: &str) -> Result<(), PipelineError> {
    let invalid = || PipelineError::Value(format!("{name}-invalid-uri"));
    if value.chars().any(|c| u32::from(c) < 32 || c == '"') {
        return Err(invalid());
    }
    let parsed = hls_cli::urlsplit(value).map_err(|_| invalid())?;
    // LA_URL is embedded metadata, never fetched by this pipeline: explicit HTTP test/provider
    // URLs are preserved as well as HTTPS license endpoints.
    if !schemes.contains(&parsed.scheme.as_str())
        || parsed.netloc.is_empty()
        || parsed.username().is_some_and(|user| !user.is_empty())
        || parsed.password().is_some_and(|word| !word.is_empty())
    {
        return Err(invalid());
    }
    Ok(())
}

/// Validate the three DRM signaling values against the key material and build the `PlayReady`
/// objects (`playready::build_playready_v43`). Tokens: `widevine-invalid-pssh`,
/// `widevine-invalid-payload`, `widevine-kid-mismatch`, `playready-invalid-uri`,
/// `fairplay-invalid-uri`; a `PlayReadyError` (a `ValueError`) with its message.
pub fn validate_signaling(
    signaling: &DrmSignaling,
    keys: &KeyMaterial,
) -> Result<PlayReadyObjects, PipelineError> {
    let invalid = || PipelineError::Value("widevine-invalid-pssh".to_string());
    let raw = &signaling.widevine_pssh;
    if raw.len() < 32 || raw.len() > 65536 {
        return Err(invalid());
    }
    let be32 = |at: usize| -> usize {
        raw.get(at..at + 4)
            .and_then(|bytes| bytes.try_into().ok())
            .map_or(usize::MAX, |bytes: [u8; 4]| {
                u32::from_be_bytes(bytes) as usize
            })
    };
    if be32(0) != raw.len()
        || &raw[4..8] != b"pssh"
        || !matches!(raw[8], 0 | 1)
        || raw[9..12] != [0, 0, 0]
        || raw[12..28] != WIDEVINE_SYSTEM_ID
    {
        return Err(invalid());
    }
    let mut offset = 28;
    let mut kids: Vec<Vec<u8>> = Vec::new();
    if raw[8] == 1 {
        if raw.len() < 36 {
            return Err(invalid());
        }
        let count = be32(offset);
        offset += 4;
        if count > 64 || offset + count * 16 + 4 > raw.len() {
            return Err(invalid());
        }
        kids.extend((0..count).map(|i| raw[offset + i * 16..offset + (i + 1) * 16].to_vec()));
        offset += count * 16;
    }
    if offset + 4 > raw.len() || be32(offset) != raw.len() - offset - 4 {
        return Err(invalid());
    }
    kids.extend(protobuf_kids(&raw[offset + 4..])?);
    for kid in &kids {
        // `bytes.fromhex(keys.key_id_hex)` per KID, lazily as Python's `any` evaluates it
        let content_kid = pycompat::fromhex(keys.key_id_hex()).map_err(PipelineError::Value)?;
        if *kid != content_kid {
            return Err(PipelineError::Value("widevine-kid-mismatch".to_string()));
        }
    }
    check_signaling_uri(&signaling.playready_la_url, &["https", "http"], "playready")?;
    check_signaling_uri(&signaling.fairplay_uri, &["skd"], "fairplay")?;
    playready::build_playready_v43(keys.key_id_hex(), &signaling.playready_la_url)
        .map_err(|error| PipelineError::Value(error.message().to_string()))
}

/// `selection.audio_source_id` as `_selected_tracks` tests it: `None` without Python truth;
/// a text id; any other truthy value is never an id (`invalid-audio-selection`), and a list or
/// an object cannot even be looked up (`TypeError: unhashable type`).
fn chosen_audio(
    selection: Option<&PackagingSelection>,
    candidates: &[&SourceTrack],
) -> Result<Option<String>, PipelineError> {
    let Some(value) = selection
        .map(|choice| &choice.audio_source_id)
        .filter(|value| truthy(value))
    else {
        return Ok(None);
    };
    match value {
        Value::String(id) if candidates.iter().any(|track| track.id == *id) => Ok(Some(id.clone())),
        Value::Array(_) => Err(PipelineError::Value("unhashable type: 'list'".to_string())),
        Value::Object(_) => Err(PipelineError::Value("unhashable type: 'dict'".to_string())),
        _ => Err(PipelineError::Value("invalid-audio-selection".to_string())),
    }
}

/// The tracks to package and the muxed audio candidates set aside by the selection.
pub fn selected_tracks(
    analysis: &SourceAnalysis,
    selection: Option<&PackagingSelection>,
    deps: &dyn PipelineDeps,
) -> Result<(Vec<SourceTrack>, Vec<SourceTrack>), PipelineError> {
    validate_track_ids(analysis)?;
    let value = |token: &str| PipelineError::Value(token.to_string());
    if selection.is_some_and(|choice| {
        !py_eq(
            &choice.analysis_id,
            &Value::from(analysis.analysis_id.as_str()),
        ) || !py_eq(
            &choice.snapshot_digest,
            &Value::from(analysis.snapshot_digest.as_str()),
        )
    }) {
        return Err(value("stale-packaging-selection"));
    }
    let candidates: Vec<&SourceTrack> = analysis
        .tracks
        .iter()
        .filter(|track| track.content_type == "audio" && track.muxed)
        .collect();
    let mut selected = chosen_audio(selection, &candidates)?;
    let groups: BTreeSet<&str> = candidates
        .iter()
        .map(|track| track.group_id.as_str())
        .collect();
    if groups.len() > 1 {
        // One global choice must never erase another advertised audio group.
        if groups.iter().any(|group| {
            candidates
                .iter()
                .filter(|track| track.group_id == *group)
                .count()
                > 1
        }) {
            return Err(value("multiple-audio-group-selection-required"));
        }
        return Ok((analysis.tracks.clone(), Vec::new()));
    }
    if candidates.len() > 1 && selected.is_none() {
        // Equivalence is proved by complete decoded PCM, never playlist labels.
        let asset_dir = asset_dir_of(&analysis.snapshot_path)?;
        let work = output_directory(&asset_dir, Path::new("clear/audio-equivalence"))
            .map_err(output_error)?;
        let mut digests = BTreeSet::new();
        for track in &candidates {
            let mut parts: Vec<PathBuf> = track.init_path.iter().cloned().collect();
            parts.extend(track.segments.iter().map(|segment| segment.path.clone()));
            let combined = deps
                .concatenate(&parts, &work.join(format!("{}.mp4", track.id)))
                .map_err(|error| escaped_media(&error))?;
            digests.insert(
                deps.decoded_digest(&combined, "audio", track.stream_index)
                    .map_err(|error| escaped_media(&error))?,
            );
        }
        if digests.len() != 1 {
            return Err(value("audio-selection-required"));
        }
        selected = Some(candidates[0].id.clone());
    } else if !candidates.is_empty() && selected.is_none() {
        selected = Some(candidates[0].id.clone());
    }
    let is_candidate =
        |track: &SourceTrack| candidates.iter().any(|candidate| candidate.id == track.id);
    let chosen = |track: &SourceTrack| selected.as_deref() == Some(track.id.as_str());
    let kept = analysis
        .tracks
        .iter()
        .filter(|track| !is_candidate(track) || chosen(track))
        .cloned()
        .collect();
    let unused = candidates
        .iter()
        .filter(|track| !chosen(track))
        .map(|track| (*track).clone())
        .collect();
    Ok((kept, unused))
}

/// `snapshot_path.parents[2]`: `output/<asset_id>/clear/source/analysis.json` → the asset dir.
fn asset_dir_of(snapshot_path: &Path) -> Result<PathBuf, PipelineError> {
    snapshot_path
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .ok_or_else(|| PipelineError::Infrastructure("tuple index out of range".to_string()))
}

fn validate_track_ids(analysis: &SourceAnalysis) -> Result<(), PipelineError> {
    if analysis
        .tracks
        .iter()
        .any(|track| !valid_track_id(&track.id))
    {
        return Err(PipelineError::Value("invalid-track-id".to_string()));
    }
    Ok(())
}

fn output_error(error: TrackError) -> PipelineError {
    match error {
        TrackError::Media(token) => PipelineError::Value(token),
        _ => PipelineError::Value("unsafe-output-path".to_string()),
    }
}

/// Validate existing destinations before creating directories or opening output files.
/// Symlinks are refused even when they point inside the asset, where source files live.
fn output_directory(asset_dir: &Path, relative: &Path) -> Result<PathBuf, TrackError> {
    let unsafe_path = || TrackError::Media("unsafe-output-path".to_string());
    let destination = asset_dir.join(relative);
    crate::pipeline::confine(&destination, asset_dir).map_err(|_| unsafe_path())?;
    let mut prefix = asset_dir.to_path_buf();
    for component in relative.components() {
        prefix.push(component);
        match fs::symlink_metadata(&prefix) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Err(unsafe_path()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(unsafe_path()),
        }
    }
    let mut pending = vec![destination.clone()];
    while let Some(path) = pending.pop() {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => return Err(unsafe_path()),
            Ok(metadata) if metadata.is_dir() => {
                for entry in fs::read_dir(&path).map_err(|_| unsafe_path())? {
                    pending.push(entry.map_err(|_| unsafe_path())?.path());
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(unsafe_path()),
        }
    }
    Ok(destination)
}

fn track_directories(
    track: &SourceTrack,
    asset_dir: &Path,
) -> Result<(PathBuf, PathBuf), TrackError> {
    if !valid_track_id(&track.id) {
        return Err(TrackError::Media("invalid-track-id".to_string()));
    }
    let folder = output_directory(asset_dir, &Path::new("cbcs").join(&track.id))?;
    let work = output_directory(asset_dir, &Path::new("clear/package-hls").join(&track.id))?;
    Ok((folder, work))
}

fn packaging_directories(analysis: &SourceAnalysis) -> Result<(PathBuf, PathBuf), PipelineError> {
    validate_track_ids(analysis)?;
    let asset_dir = asset_dir_of(&analysis.snapshot_path)?;
    for track in &analysis.tracks {
        track_directories(track, &asset_dir).map_err(output_error)?;
    }
    let manifest_dir =
        output_directory(&asset_dir, Path::new("manifests/clear-hls")).map_err(output_error)?;
    output_directory(&asset_dir, Path::new("reports")).map_err(output_error)?;
    Ok((asset_dir, manifest_dir))
}

/// The cbcs truth of one init segment: scheme, constant IV, KID, no per-sample IV, the
/// expected PSSH boxes, and the pattern (1:9 for video, 0:0 for audio).
fn validate_init(
    path: &Path,
    keys: &KeyMaterial,
    pssh: &[&[u8]],
    content_type: &str,
    deps: &dyn PipelineDeps,
) -> Result<(), TrackError> {
    let probe = deps.probe_init_segment(path)?;
    // `keys.iv_hex.lower` and `keys.key_id_hex.lower`: compared, never printed
    let iv = HexSecret::new(keys.iv_hex().to_lowercase());
    if probe.scheme_type.as_deref() != Some("cbcs")
        || probe.constant_iv_hex.as_ref() != Some(&iv)
        || probe.default_kid_hex.as_deref() != Some(keys.key_id_hex().to_lowercase().as_str())
        || probe.per_sample_iv_size != Some(0)
    {
        return Err(TrackError::Media("cbcs-init-mismatch".to_string()));
    }
    for expected in pssh {
        if !deps.pssh_box_present(path, expected)? {
            return Err(TrackError::Media("init-pssh-mismatch".to_string()));
        }
    }
    let tree = deps.box_tree(path)?;
    let tenc = first_box_named(&tree, "tenc");
    let field = |name: &str| -> Value {
        tenc.and_then(|found| found.get(name))
            .cloned()
            .unwrap_or_else(|| Value::from(0))
    };
    let (crypt, skip) = if content_type == "video" {
        (1, 9)
    } else {
        (0, 0)
    };
    if !py_eq(&field("default_crypt_byte_block"), &Value::from(crypt))
        || !py_eq(&field("default_skip_byte_block"), &Value::from(skip))
    {
        return Err(TrackError::Media("cbcs-pattern-mismatch".to_string()));
    }
    Ok(())
}

/// The first object with `"name": <name>` in a pre-order walk of mp4dump's JSON (objects in
/// key order, arrays in order), as the Python generator yields it.
fn first_box_named<'a>(value: &'a Value, name: &str) -> Option<&'a serde_json::Map<String, Value>> {
    match value {
        Value::Object(map) => {
            if map.get("name").and_then(Value::as_str) == Some(name) {
                return Some(map);
            }
            map.values().find_map(|child| first_box_named(child, name))
        }
        Value::Array(items) => items.iter().find_map(|child| first_box_named(child, name)),
        _ => None,
    }
}

/// Run per-segment units in order: on the caller's thread without a pool (the sequential
/// reference), else through the shared segment pool with 's fail-fast, in-order error
/// selection.
fn run_units<'scope, T: Send + 'scope>(
    pool: Option<&Pool<'scope>>,
    units: Vec<Unit<'scope, T, TrackError>>,
) -> Result<Vec<T>, TrackError> {
    match pool {
        None => units.into_iter().map(|unit| unit()).collect(),
        Some(pool) => run_ordered(|unit| pool.submit(unit), units, None),
    }
}

/// What every unit of one track shares (the Python closure's environment).
#[derive(Clone, Copy)]
pub struct TrackJob<'scope> {
    pub track: &'scope SourceTrack,
    pub keys: &'scope KeyMaterial,
    pub signaling: &'scope DrmSignaling,
    pub pr: &'scope PlayReadyObjects,
    pub packager_bin: &'scope str,
    pub deps: &'scope dyn PipelineDeps,
}

type Prepared = (PathBuf, PacketInfo, PathBuf, PathBuf);

/// `shutil.copyfile(source, destination)`: the bytes only (no permission bits).
fn copyfile(source: &Path, destination: &Path) -> std::io::Result<()> {
    let mut from = fs::File::open(source)?;
    let mut to = fs::File::create(destination)?;
    std::io::copy(&mut from, &mut to)?;
    Ok(())
}

/// `Path.mkdir(exist_ok=True)`: the parent must exist; an existing directory is fine.
fn mkdir_exist_ok(path: &Path) -> std::io::Result<()> {
    match fs::create_dir(path) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        other => other,
    }
}

/// `max(1_000_000.0, value)`: Python keeps the first argument unless the second is greater
/// (so a NaN gives the first).
fn py_max(first: f64, second: f64) -> f64 {
    if second > first { second } else { first }
}

/// `prepare(number, segment)`: re-mux and encrypt one segment in isolation, validate its own
/// init, copy the output segment into place.
fn prepare_segment(
    job: TrackJob<'_>,
    work: &Path,
    folder: &Path,
    number: usize,
    segment: &SourceSegment,
) -> Result<Prepared, TrackError> {
    let track = job.track;
    let deps = job.deps;
    let shift = transport_shift();
    let mut parts: Vec<PathBuf> = track.init_path.iter().cloned().collect();
    parts.push(segment.path.clone());
    let source = deps.concatenate(&parts, &work.join(format!("source_{number}.mp4")))?;
    let before = deps.probe_packets(&source, track.stream_index)?;
    let earliest = before
        .dts
        .iter()
        .map(|dts| frac(i128::from(*dts), i128::from(before.timescale)))
        .min()
        .ok_or_else(|| TrackError::Failed("min() arg is an empty sequence".to_string()))?;
    if earliest + Frac::from_integer(2) < Frac::zero() {
        return Err(TrackError::Media(
            "unsupported-negative-timeline".to_string(),
        ));
    }
    let audio_mp4 = track.content_type == "audio" && track.container == "fmp4";
    let suffix = if audio_mp4 { ".mp4" } else { ".ts" };
    let normalized = deps.normalize_segment(
        &source,
        &work.join(format!("normalized_{number}{suffix}")),
        track.stream_index,
        &shift,
        if audio_mp4 { "audio" } else { "video" },
    )?;
    let remuxed = deps.probe_packets(&normalized, 0)?;
    if !deps.same_timing(&before, &remuxed, &shift)? {
        return Err(TrackError::Media(
            "normalization-timing-mismatch".to_string(),
        ));
    }
    let per = work.join(format!("segment_{number}"));
    mkdir_exist_ok(&per)?;
    let init = per.join("init.mp4");
    let end = media::python::fraction_to_f64(&remuxed.end()?)?;
    let pssh_hex = [
        hex::encode(&job.signaling.widevine_pssh),
        job.pr.pssh_hex.clone(),
    ];
    let template = pycompat::path_str(&per.join("seg_$Number$.m4s"));
    let result = deps.encrypt_cbcs(&EncryptCbcs {
        input_mp4: &normalized,
        init_output: &init,
        segment_template: &template,
        key: job.keys,
        segment_duration_seconds: py_max(1_000_000.0, end + segment.duration + 1.0),
        pssh_hex: Some(&pssh_hex),
        packager_bin: job.packager_bin,
    })?;
    if result.returncode != 0 {
        return Err(TrackError::Media("packager-encryption-failed".to_string()));
    }
    let produced = produced_segments(&per)?;
    if produced.len() != 1 {
        return Err(TrackError::Media(
            "segment-correspondence-mismatch".to_string(),
        ));
    }
    validate_init(
        &init,
        job.keys,
        &[&job.signaling.widevine_pssh, &job.pr.pssh_box],
        &track.content_type,
        deps,
    )?;
    let final_segment = folder.join(format!("seg_{number}.m4s"));
    copyfile(&produced[0], &final_segment)?;
    Ok((source, before, init, final_segment))
}

/// `per.glob("seg_*.m4s")`: every entry named like an output segment (case-sensitive, as the
/// glob is).
#[allow(clippy::case_sensitive_file_extension_comparisons)] // reason: the glob is case-sensitive, as Python's
fn produced_segments(per: &Path) -> Result<Vec<PathBuf>, TrackError> {
    Ok(fs::read_dir(per)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("seg_") && name.ends_with(".m4s"))
        })
        .collect())
}

/// `int(size * 8 * timescale / duration)`: the exact product, one correctly rounded true
/// division (a zero duration is Python's `ZeroDivisionError`), truncated to an int.
#[allow(clippy::cast_possible_truncation)] // reason: truncated to an int, as Python's int
fn bits_per_second(size: u64, timescale: i64, duration: i64) -> Result<i64, TrackError> {
    let numerator = BigInt::from(size) * 8 * timescale;
    let quotient = true_divide(&numerator, &BigInt::from(duration))?;
    Ok(quotient.trunc() as i64)
}

/// `finish(number, segment, prepared, final_init)`: prove the output segment against the
/// track's final init — timing, round-trip decryption, decoded identity.
fn finish_segment(
    job: TrackJob<'_>,
    work: &Path,
    number: usize,
    segment: &SourceSegment,
    prepared: Prepared,
    final_init: &Path,
) -> Result<(OutputSegment, PacketInfo), TrackError> {
    let track = job.track;
    let deps = job.deps;
    let (source, before, _, final_segment) = prepared;
    let encrypted = deps.concatenate(
        &[final_init.to_path_buf(), final_segment.clone()],
        &work.join(format!("encrypted_{number}.mp4")),
    )?;
    let after = deps.probe_packets(&encrypted, 0)?;
    if !deps.same_timing(&before, &after, &transport_shift())? {
        return Err(TrackError::Media("packaged-timing-mismatch".to_string()));
    }
    let clear = work.join(format!("decrypted_{number}.mp4"));
    if deps
        .decrypt_cenc(&encrypted, &clear, job.keys, job.packager_bin)?
        .returncode
        != 0
    {
        return Err(TrackError::Media("roundtrip-decryption-failed".to_string()));
    }
    // Each segment is decoded in full, so no segment can disappear behind an aggregate hash
    // or a correct first/final sample count.
    if deps.decoded_digest(&source, &track.content_type, track.stream_index)?
        != deps.decoded_digest(&clear, &track.content_type, 0)?
    {
        return Err(TrackError::Media("decoded-content-mismatch".to_string()));
    }
    let (start, duration) = span(&after)?;
    let decode_start = *after
        .dts
        .iter()
        .min()
        .ok_or_else(|| TrackError::Failed("min() arg is an empty sequence".to_string()))?;
    Ok((
        OutputSegment {
            source_index: segment.index,
            path: final_segment,
            timescale: after.timescale,
            decode_start,
            duration,
            presentation_start: start,
            sample_count: i64::try_from(after.pts.len()).unwrap_or(i64::MAX),
        },
        after,
    ))
}

/// `start = min(pts); duration = max(p + d) - start`.
fn span(info: &PacketInfo) -> Result<(i64, i64), TrackError> {
    let empty = || TrackError::Failed("min() arg is an empty sequence".to_string());
    let start = *info.pts.iter().min().ok_or_else(empty)?;
    let end = info
        .pts
        .iter()
        .zip(&info.durations)
        .map(|(p, d)| p + d)
        .max()
        .ok_or_else(empty)?;
    Ok((start, end - start))
}

/// `list index out of range` (`prepared[0]`, `finished[-1]` of a track without segments): an
/// `IndexError`, which nothing catches.
fn index_error() -> TrackError {
    TrackError::Escaped(PipelineError::Infrastructure(
        "list index out of range".to_string(),
    ))
}

/// Package one track's segments to CBCS, one source segment at a time : every
/// `prepare`, then every `finish`, each through the pool when there is one.
pub fn package_track<'scope>(
    job: TrackJob<'scope>,
    asset_dir: &Path,
    pool: Option<&Pool<'scope>>,
) -> Result<OutputTrack, TrackError> {
    let track = job.track;
    let (folder, work) = track_directories(track, asset_dir)?;
    fs::create_dir_all(&folder)?;
    fs::create_dir_all(&work)?;
    let prepare_units: Vec<Unit<'scope, Prepared, TrackError>> = track
        .segments
        .iter()
        .enumerate()
        .map(|(number, segment)| {
            let (work, folder) = (work.clone(), folder.clone());
            Box::new(move || prepare_segment(job, &work, &folder, number, segment))
                as Unit<'scope, Prepared, TrackError>
        })
        .collect();
    let prepared = run_units(pool, prepare_units)?;
    let final_init = folder.join("init.mp4");
    let first_init = &prepared.first().ok_or_else(index_error)?.2;
    copyfile(first_init, &final_init)?;
    let finish_units: Vec<Unit<'scope, (OutputSegment, PacketInfo), TrackError>> = track
        .segments
        .iter()
        .zip(prepared)
        .enumerate()
        .map(|(number, (segment, item))| {
            let (work, final_init) = (work.clone(), final_init.clone());
            Box::new(move || finish_segment(job, &work, number, segment, item, &final_init))
                as Unit<'scope, (OutputSegment, PacketInfo), TrackError>
        })
        .collect();
    let finished = run_units(pool, finish_units)?;
    let after = finished
        .last()
        .map(|item| item.1.clone())
        .ok_or_else(index_error)?;
    let output: Vec<OutputSegment> = finished.into_iter().map(|item| item.0).collect();
    // No gap or overlap between adjacent source segments.
    for pair in output.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let gap = frac(
            i128::from(a.presentation_start + a.duration),
            i128::from(a.timescale),
        ) - frac(i128::from(b.presentation_start), i128::from(b.timescale));
        if gap.abs() > frac(1, 45000) {
            return Err(TrackError::Media(
                "output-timeline-discontinuity".to_string(),
            ));
        }
    }
    let mut bandwidth: Option<i64> = None;
    for segment in &output {
        let bits = bits_per_second(
            fs::metadata(&segment.path)?.len(),
            segment.timescale,
            segment.duration,
        )?;
        bandwidth = Some(bandwidth.map_or(bits, |best| best.max(bits)));
    }
    let bandwidth = bandwidth
        .ok_or_else(|| TrackError::Failed("max() arg is an empty sequence".to_string()))?;
    let codec = job.deps.codec_from_init(&final_init, &track.codec)?;
    let mut out = OutputTrack::new(
        &track.id,
        &track.content_type,
        &codec,
        output,
        Some(final_init),
        bandwidth,
    );
    out.language = or_default(&track.language, "und");
    out.role = or_default(&track.role, "main");
    out.name = or_default(&track.name, &track.id);
    out.group_id = or_default(&track.group_id, "audio");
    out.width = track
        .width
        .map_or(0, |width| i64::try_from(width).unwrap_or(i64::MAX));
    out.height = track
        .height
        .map_or(0, |height| i64::try_from(height).unwrap_or(i64::MAX));
    out.frame_rate = if track.content_type == "video" && after.frame_rate != "0/0" {
        after.frame_rate
    } else {
        String::new()
    };
    out.audio_group.clone_from(&track.audio_group);
    out.subtitle_group.clone_from(&track.subtitle_group);
    out.is_default = track.is_default;
    out.autoselect = track.autoselect;
    Ok(out)
}

/// `value or default`.
fn or_default(value: &str, default: &str) -> String {
    if value.is_empty() {
        default.to_string()
    } else {
        value.to_string()
    }
}

const TTML_NS: &str = "http://www.w3.org/ns/ttml";
const TTML_PARAMETER_NS: &str = "http://www.w3.org/ns/ttml#parameter";

/// One IMSC document is a qualified UTF-8 `tt` root with an IMSC1 text profile and no
/// external or binary references.
fn imsc_document_ok(document: &[u8]) -> bool {
    // Qualified STPP XML is UTF-8. NUL-bearing UTF-16/32 and any DOCTYPE are rejected before
    // parsing, so an encoded DOCTYPE cannot evade the byte-level ban.
    if document.len() > 4 * 1024 * 1024
        || document.contains(&0)
        || document.windows(9).any(|window| window == b"<!DOCTYPE")
    {
        return false;
    }
    let Ok(text) = std::str::from_utf8(document) else {
        return false;
    };
    let Ok(tree) = roxmltree::Document::parse(text) else {
        return false;
    };
    let root = tree.root_element();
    let profile = root.attribute((TTML_PARAMETER_NS, "profile")).unwrap_or("");
    let allowed = [
        "http://www.w3.org/ns/ttml/profile/imsc1/text",
        "http://www.w3.org/ns/ttml/profile/imsc1.1/text",
    ];
    if root.tag_name().namespace() != Some(TTML_NS)
        || root.tag_name().name() != "tt"
        || !allowed.contains(&profile)
    {
        return false;
    }
    !root
        .descendants()
        .filter(roxmltree::Node::is_element)
        .any(|element| {
            element
                .attributes()
                .any(|attribute| attribute.name() == "src")
                || matches!(element.tag_name().name(), "image" | "audio")
        })
}

/// `payloads(source, a) != payloads(joined, b)`: both lists read (left first), then compared
/// element by element with Python's `==`.
fn same_payloads(
    deps: &dyn PipelineDeps,
    source: &Path,
    stream: i64,
    joined: &Path,
) -> Result<bool, TrackError> {
    let left = deps.packet_payload_hashes(source, stream)?;
    let right = deps.packet_payload_hashes(joined, 0)?;
    Ok(left.len() == right.len() && left.iter().zip(&right).all(|(a, b)| py_eq(a, b)))
}

/// `concatenate([track.init_path, *rest], output)` as `_copy_imsc` calls it: the init is
/// passed as it is, so without one `None.open` fails once `output` exists (an
/// `AttributeError`, which no handler catches).
fn concatenate_after_init(
    deps: &dyn PipelineDeps,
    init: Option<&PathBuf>,
    rest: &[PathBuf],
    output: &Path,
) -> Result<PathBuf, TrackError> {
    let Some(init) = init else {
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::File::create(output)?;
        return Err(TrackError::Escaped(PipelineError::Infrastructure(
            "'NoneType' object has no attribute 'open'".to_string(),
        )));
    };
    let mut parts = vec![init.clone()];
    parts.extend(rest.iter().cloned());
    Ok(deps.concatenate(&parts, output)?)
}

/// Copy an IMSC (TTML in fMP4) track: validated document by document, re-fragmented one
/// sample per fragment and regrouped by the source segment's packet census.
fn copy_imsc(
    track: &SourceTrack,
    asset_dir: &Path,
    deps: &dyn PipelineDeps,
) -> Result<OutputTrack, TrackError> {
    let unsupported = || TrackError::Media("unsupported-subtitle-profile".to_string());
    let (folder, work) = track_directories(track, asset_dir)?;
    fs::create_dir_all(&folder)?;
    fs::create_dir_all(&work)?;
    let mut sources: Vec<(PathBuf, PacketInfo)> = Vec::new();
    for (number, segment) in track.segments.iter().enumerate() {
        let source = concatenate_after_init(
            deps,
            track.init_path.as_ref(),
            std::slice::from_ref(&segment.path),
            &work.join(format!("source_{number}.mp4")),
        )?;
        let documents = deps.packet_payloads(&source, track.stream_index)?;
        if documents.is_empty() || !documents.iter().all(|document| imsc_document_ok(document)) {
            return Err(unsupported());
        }
        let info = deps.probe_packets(&source, track.stream_index)?;
        sources.push((source, info));
    }
    let all: Vec<PathBuf> = track
        .segments
        .iter()
        .map(|segment| segment.path.clone())
        .collect();
    let combined = concatenate_after_init(
        deps,
        track.init_path.as_ref(),
        &all,
        &work.join("source.mp4"),
    )?;
    let normalized = deps.normalize_segment(
        &combined,
        &work.join("normalized.mp4"),
        track.stream_index,
        &transport_shift(),
        "text",
    )?;
    let init = folder.join("init.mp4");
    let fragments = deps.split_fragmented_mp4(&normalized, &init)?;
    // frag_duration=1 produces one subtitle sample per fragment. Group those opaque fragments
    // by the original segment's packet census, preserving its exact correspondence without
    // parsing or editing tfdt/trun timestamps.
    if fragments.len()
        != sources
            .iter()
            .map(|(_, info)| info.pts.len())
            .sum::<usize>()
    {
        return Err(TrackError::Media(
            "text-segment-correspondence-mismatch".to_string(),
        ));
    }
    let mut cursor = 0usize;
    let mut output = Vec::new();
    for (number, ((source, before), segment)) in sources.iter().zip(&track.segments).enumerate() {
        let count = before.pts.len();
        let path = folder.join(format!("seg_{number}.m4s"));
        fs::write(&path, fragments[cursor..cursor + count].concat())?;
        cursor += count;
        let joined = deps.concatenate(&[init.clone(), path.clone()], &work.join("output.mp4"))?;
        let after = deps.probe_packets(&joined, 0)?;
        if !deps.same_timing(before, &after, &transport_shift())? {
            return Err(TrackError::Media("text-timing-mismatch".to_string()));
        }
        if !same_payloads(deps, source, track.stream_index, &joined)? {
            return Err(TrackError::Media("text-payload-mismatch".to_string()));
        }
        let (start, duration) = span(&after)?;
        let decode_start = *after
            .dts
            .iter()
            .min()
            .ok_or_else(|| TrackError::Failed("min() arg is an empty sequence".to_string()))?;
        output.push(OutputSegment {
            source_index: segment.index,
            path,
            timescale: after.timescale,
            decode_start,
            duration,
            presentation_start: start,
            sample_count: i64::try_from(count).unwrap_or(i64::MAX),
        });
    }
    let mut out = OutputTrack::new(
        &track.id,
        "text",
        "stpp.ttml.im1t",
        output,
        Some(init),
        text_bandwidth(track),
    );
    out.language = or_default(&track.language, "und");
    out.role = or_default(&track.role, "subtitle");
    out.name = or_default(&track.name, &track.id);
    out.group_id = or_default(&track.group_id, "subtitles");
    out.encrypted = false;
    out.is_default = track.is_default;
    out.autoselect = track.autoselect;
    Ok(out)
}

/// `max(1, track.bandwidth or 1000)`.
fn text_bandwidth(track: &SourceTrack) -> i64 {
    track
        .bandwidth
        .filter(|bandwidth| *bandwidth != 0)
        .map_or(1000, |bandwidth| {
            i64::try_from(bandwidth).unwrap_or(i64::MAX)
        })
        .max(1)
}

/// `_vtt_time`: `HH:MM:SS.mmm` or `MM:SS.mmm` as an exact number of seconds.
fn vtt_time(value: &str) -> Result<Frac, TrackError> {
    // Python's `\d` also matches other Unicode decimal digits; WebVTT timestamps are ASCII.
    let pattern = Regex::new(r"^(?:([0-9]{2,}):)?([0-5][0-9]):([0-5][0-9]\.[0-9]{3})$")
        .expect("a valid pattern");
    let unsupported = || TrackError::Media("unsupported-subtitle-profile".to_string());
    let captures = pattern.captures(value).ok_or_else(unsupported)?;
    let hours: i128 = captures
        .get(1)
        .map_or(Ok(0), |m| m.as_str().parse())
        .map_err(|_| unsupported())?;
    let minutes: i128 = captures[2].parse().map_err(|_| unsupported())?;
    let seconds = frac_from_decimal(&captures[3]).ok_or_else(unsupported)?;
    Ok(Frac::from_integer(hours * 3600 + minutes * 60) + seconds)
}

/// Python's `str.splitlines` boundaries.
fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\x0b'
                | '\x0c'
                | '\x1c'
                | '\x1d'
                | '\x1e'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if boundary {
            lines.push(&text[start..index]);
            let mut next = index + c.len_utf8();
            if c == '\r' && chars.peek().is_some_and(|(_, n)| *n == '\n') {
                chars.next();
                next += 1;
            }
            start = next;
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Python's `str.strip`: Unicode whitespace, including U+001C–U+001F.
fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// `_vtt_cue_starts`: the start time of every cue, each cue's timing line validated.
fn vtt_cue_starts(content: &str) -> Result<Vec<Frac>, TrackError> {
    let unsupported = || TrackError::Media("unsupported-subtitle-profile".to_string());
    let separator = Regex::new(r"\r?\n[ \t]*\r?\n").expect("a valid pattern");
    let skipped = Regex::new(r"^(?:NOTE(?:[ \t]|$)|STYLE$|REGION$)").expect("a valid pattern");
    let timing_line =
        Regex::new(r"^([^ \t]+)[ \t]+-->[ \t]+([^ \t]+)(?:[ \t]+.*)?$").expect("a valid pattern");
    let mut starts = Vec::new();
    for block in separator.split(content).skip(1) {
        let lines = py_splitlines(py_strip(block));
        let Some(first) = lines.first() else {
            continue;
        };
        if skipped.is_match(first) {
            continue;
        }
        let timing = if first.contains("-->") {
            *first
        } else {
            lines.get(1).copied().unwrap_or("")
        };
        let captures = timing_line.captures(timing).ok_or_else(unsupported)?;
        let (start, end) = (vtt_time(&captures[1])?, vtt_time(&captures[2])?);
        if end <= start {
            return Err(unsupported());
        }
        starts.push(start);
    }
    Ok(starts)
}

/// `Path.read_text(encoding="utf-8-sig")`: the BOM dropped, universal newlines.
fn read_text_utf8_sig(path: &Path) -> Result<String, TrackError> {
    let bytes = fs::read(path)?;
    let text =
        std::str::from_utf8(&bytes).map_err(|error| TrackError::Failed(error.to_string()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    Ok(crate::toolrun::universal_newlines(text))
}

/// Copy a text track: `WebVTT` segments with the `X-TIMESTAMP-MAP` moved to the common
/// transport offset, or IMSC through [`copy_imsc`].
pub fn copy_text(
    track: &SourceTrack,
    asset_dir: &Path,
    deps: &dyn PipelineDeps,
) -> Result<OutputTrack, TrackError> {
    let (folder, _) = track_directories(track, asset_dir)?;
    if track.container != "webvtt" {
        return copy_imsc(track, asset_dir, deps);
    }
    let unsupported = || TrackError::Media("unsupported-subtitle-profile".to_string());
    fs::create_dir_all(&folder)?;
    let map_line = Regex::new(r"(?m)^X-TIMESTAMP-MAP=LOCAL:([0-9:.]+),MPEGTS:([0-9]+)[ \t]*\r?$")
        .expect("a valid pattern");
    let wrap = frac(1 << 33, 90000);
    let mut cursor: i64 = 0;
    let mut output = Vec::new();
    let mut map_offset: Option<Frac> = None;
    let mut cue_starts: Vec<Frac> = Vec::new();
    for (number, segment) in track.segments.iter().enumerate() {
        let path = folder.join(format!("seg_{number}.vtt"));
        let mut content = read_text_utf8_sig(&segment.path)?;
        if !content.starts_with("WEBVTT") || content.contains('\0') {
            return Err(unsupported());
        }
        cue_starts.extend(vtt_cue_starts(&content)?);
        let maps: Vec<regex::Captures<'_>> = map_line.captures_iter(&content).collect();
        if content.matches("X-TIMESTAMP-MAP").count() != maps.len() || maps.len() > 1 {
            return Err(unsupported());
        }
        let offset = if let Some(found) = maps.first() {
            let transport: i128 = found[2].parse().map_err(|_| unsupported())?;
            if transport >= 1 << 33 {
                return Err(unsupported());
            }
            let local = vtt_time(&found[1])?;
            let offset = frac(transport, 90000) - local;
            let mapping = format!(
                "X-TIMESTAMP-MAP=LOCAL:{},MPEGTS:{}",
                &found[1],
                (transport + 180_000) % (1 << 33)
            );
            let whole_match = found.get(0).expect("the match");
            let (start, end) = (whole_match.start(), whole_match.end());
            content = format!("{}{mapping}{}", &content[..start], &content[end..]);
            offset
        } else {
            let at = content.find('\n').ok_or_else(unsupported)?;
            content = format!(
                "{}X-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:180000\n{}",
                &content[..=at],
                &content[at + 1..]
            );
            Frac::zero()
        };
        match map_offset {
            None => map_offset = Some(offset),
            Some(first) => {
                if !((offset - first) / wrap).is_integer() {
                    return Err(TrackError::Media(
                        "unsupported-webvtt-clock-change".to_string(),
                    ));
                }
            }
        }
        fs::write(&path, content)?;
        let ticks = vtt_ticks(segment.duration);
        output.push(OutputSegment {
            source_index: segment.index,
            path,
            timescale: 90000,
            decode_start: cursor,
            duration: ticks,
            presentation_start: cursor,
            sample_count: 0,
        });
        cursor += ticks;
    }
    let mut out = OutputTrack::new(
        &track.id,
        "text",
        "wvtt",
        output,
        None,
        text_bandwidth(track),
    );
    out.language = or_default(&track.language, "und");
    out.role = or_default(&track.role, "subtitle");
    out.name = or_default(&track.name, &track.id);
    out.group_id = or_default(&track.group_id, "subtitles");
    out.encrypted = false;
    out.text_map_offset = map_offset.map(to_fraction);
    out.text_earliest_cue = cue_starts.iter().min().copied().map(to_fraction);
    out.is_default = track.is_default;
    out.autoselect = track.autoselect;
    Ok(out)
}

/// `round(duration * 90000)`: Python's `round` of a float is to the nearest integer, ties to
/// even.
#[allow(clippy::cast_possible_truncation)] // reason: a VTT timestamp in ticks is far below i64::MAX
fn vtt_ticks(duration_s: f64) -> i64 {
    (duration_s * 90000.0).round_ties_even() as i64
}

/// An explicit bound, like the fetch policy's `connections` / `probe_workers`.
pub const MAX_WORKERS_BOUND: i64 = 64;

/// How many segments are packaged at once : every step is an external process that
/// saturates about one core, so the CPU count (capped at 32, `default_workers`) is the default;
/// 1 is the sequential path.
pub fn resolve_workers(max_workers: Option<i64>) -> Result<usize, PipelineError> {
    match max_workers {
        Some(value) if (1..=MAX_WORKERS_BOUND).contains(&value) => {
            Ok(usize::try_from(value).expect("1..=64"))
        }
        Some(_) => Err(PipelineError::Value(format!(
            "max_workers must be 1..{MAX_WORKERS_BOUND}"
        ))),
        None => Ok(default_workers()),
    }
}

/// `on_log` serialised: each call whole, never interleaved, in completion order. The tool
/// runner's own records are emitted by the CLI's sink, not through this callback.
pub struct SyncLog<'a> {
    lock: Mutex<()>,
    on_log: &'a (dyn Fn(Record) + Sync),
}

impl SyncLog<'_> {
    fn emit(&self, record: Value) {
        if let Value::Object(map) = record {
            let _guard = self
                .lock
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            (self.on_log)(map);
        }
    }
}

struct Conversion<'scope> {
    asset_dir: &'scope Path,
    keys: &'scope KeyMaterial,
    signaling: &'scope DrmSignaling,
    pr: &'scope PlayReadyObjects,
    packager_bin: &'scope str,
    deps: &'scope dyn PipelineDeps,
    log: Option<&'scope SyncLog<'scope>>,
    total: usize,
}

/// One track's conversion with its progress records; `(output, failure token)`.
fn convert<'scope>(
    run: &Conversion<'scope>,
    index: usize,
    track: &'scope SourceTrack,
    pool: Option<&Pool<'scope>>,
) -> Result<(Option<OutputTrack>, Option<String>), PipelineError> {
    let progress = |message: &str, level: &str| {
        if let Some(log) = run.log {
            log.emit(json!({"stage": "package-hls", "level": level, "message": message, "track_id": track.id, "index": index, "total": run.total}));
        }
    };
    progress("track-started", "info");
    let job = TrackJob {
        track,
        keys: run.keys,
        signaling: run.signaling,
        pr: run.pr,
        packager_bin: run.packager_bin,
        deps: run.deps,
    };
    let converted = if track.content_type == "text" {
        copy_text(track, run.asset_dir, run.deps)
    } else {
        package_track(job, run.asset_dir, pool)
    };
    match converted {
        Ok(output) => {
            progress("track-packaged", "info");
            Ok((Some(output), None))
        }
        Err(TrackError::Escaped(error)) => Err(error),
        Err(TrackError::Cancelled) => Err(PipelineError::Infrastructure("Cancelled".to_string())),
        Err(error) => {
            let token = match error {
                TrackError::Media(token) => token,
                _ => "track-packaging-failed".to_string(),
            };
            progress("track-failed", "error");
            Ok((None, Some(token)))
        }
    }
}

type Outcome = Result<(Option<OutputTrack>, Option<String>), PipelineError>;

/// Every selected track: on the caller's thread with one worker, else each on its own thread
/// over one shared segment pool of `workers`; outcomes keep track order.
fn convert_all<'env>(
    run: &Conversion<'env>,
    selected: &'env [SourceTrack],
    workers: usize,
) -> Vec<Outcome> {
    if workers == 1 {
        return selected
            .iter()
            .enumerate()
            .map(|(index, track)| convert(run, index, track, None))
            .collect();
    }
    std::thread::scope(|scope| {
        // Shared by the track threads; the pool shuts down when the last holder drops it.
        let pool = Arc::new(Pool::new(scope, workers));
        let handles: Vec<_> = selected
            .iter()
            .enumerate()
            .map(|(index, track)| {
                let pool = Arc::clone(&pool);
                scope.spawn(move || convert(run, index, track, Some(&pool)))
            })
            .collect();
        drop(pool);
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|payload| panic::resume_unwind(payload))
            })
            .collect()
    })
}

fn segment_rows(output: &OutputTrack) -> Vec<Record> {
    output
        .segments
        .iter()
        .enumerate()
        .filter_map(|(number, segment)| {
            let row = json!({
                "source_index": segment.source_index,
                "output_index": number,
                "timescale": segment.timescale,
                "presentation_start": segment.presentation_start,
                "duration": segment.duration,
                "sample_count": segment.sample_count,
            });
            match row {
                Value::Object(map) => Some(map),
                _ => None,
            }
        })
        .collect()
}

fn tick(value: i64, timescale: i64) -> Frac {
    frac(i128::from(value), i128::from(timescale))
}

/// Every video rendition's segment timeline must match the first's.
fn video_timelines_match(videos: &[&OutputTrack]) -> Vec<String> {
    let tolerance = frac(1, 45000);
    let Some((first, rest)) = videos.split_first() else {
        return Vec::new();
    };
    let expected = &first.segments;
    rest.iter()
        .filter(|track| {
            track.segments.len() != expected.len()
                || expected.iter().zip(&track.segments).any(|(a, b)| {
                    (tick(a.presentation_start, a.timescale)
                        - tick(b.presentation_start, b.timescale))
                    .abs()
                        > tolerance
                        || (tick(a.duration, a.timescale) - tick(b.duration, b.timescale)).abs()
                            > tolerance
                })
        })
        .map(|_| "video-rendition-timeline-mismatch".to_string())
        .collect()
}

fn first_segment(track: &OutputTrack) -> Result<&OutputSegment, PipelineError> {
    track
        .segments
        .first()
        .ok_or_else(|| PipelineError::Infrastructure("tuple index out of range".to_string()))
}

/// The common presentation origin and the text tracks adjusted to it.
fn common_origin(tracks: &[OutputTrack]) -> Result<(Frac, Vec<OutputTrack>), PipelineError> {
    let mut first: Option<Frac> = None;
    for track in tracks.iter().filter(|track| track.codec != "wvtt") {
        let segment = first_segment(track)?;
        let start = tick(segment.presentation_start, segment.timescale);
        first = Some(first.map_or(start, |best| best.min(start)));
    }
    let mut first =
        first.ok_or_else(|| PipelineError::Value("min() arg is an empty sequence".to_string()))?;
    let transport_anchor = first;
    let wrap = frac(1 << 33, 90000);
    let two = Frac::from_integer(2);
    let mut text_offsets: BTreeMap<String, Frac> = BTreeMap::new();
    for track in tracks.iter().filter(|track| track.codec == "wvtt") {
        let mut offset = match &track.text_map_offset {
            Some(offset) => from_fraction(offset)?,
            None => Frac::zero(),
        };
        offset +=
            Frac::from_integer(round_half_even((transport_anchor - two - offset) / wrap)) * wrap;
        text_offsets.insert(track.id.clone(), offset);
        if let Some(cue) = &track.text_earliest_cue {
            // DASH TextEngine drops cues before appendWindowStart=0: the common origin includes
            // the actual text cues, retaining any honest A/V lead-in instead of clipping them.
            first = first.min(from_fraction(cue)? + offset + two);
        }
    }
    let mut grid: i128 = 0;
    for track in tracks {
        grid = grid.gcd(&i128::from(first_segment(track)?.timescale));
    }
    // Round downward on a COMMON integer grid (at most one common tick), so PTO is
    // representable in every timescale and no A/V offset is removed.
    let origin = frac((first * grid).floor().to_integer(), grid);
    let mut adjusted = Vec::with_capacity(tracks.len());
    for track in tracks {
        if track.content_type == "text" && track.codec == "wvtt" {
            // DASH's raw VTT parser ignores HLS X-TIMESTAMP-MAP and applies -PTO to the raw cue
            // clock: that clock stays independent of the shared A/V transport origin.
            let offset = text_offsets
                .get(&track.id)
                .copied()
                .unwrap_or_else(Frac::zero);
            let mut moved = track.clone();
            moved.dash_presentation_time_offset = Some(to_fraction(origin - two - offset));
            for segment in &mut moved.segments {
                let shift = (origin * Frac::from_integer(i128::from(segment.timescale)))
                    .trunc()
                    .to_integer();
                let shift = i64::try_from(shift)
                    .map_err(|error| PipelineError::Infrastructure(error.to_string()))?;
                segment.presentation_start += shift;
                segment.decode_start += shift;
            }
            adjusted.push(moved);
        } else {
            adjusted.push(track.clone());
        }
    }
    Ok((origin, adjusted))
}

/// Package every selected track. `max_workers` bounds how many segments are
/// processed at once across all tracks; tracks run concurrently on top of that pool. Results,
/// findings and the report are assembled in track order whatever the completion order; with
/// `max_workers = 1` everything runs on the caller's thread.
#[allow(clippy::too_many_arguments)] // reason: Python's signature, argument for argument
pub fn package_hls(
    analysis: &SourceAnalysis,
    keys: &KeyMaterial,
    signaling: &DrmSignaling,
    selection: Option<&PackagingSelection>,
    on_log: Option<&(dyn Fn(Record) + Sync)>,
    packager_bin: &str,
    max_workers: Option<i64>,
    deps: &dyn PipelineDeps,
) -> Result<PackageHlsReport, PipelineError> {
    let (asset_dir, manifest_dir) = packaging_directories(analysis)?;
    let workers = resolve_workers(max_workers)?;
    let pr = validate_signaling(signaling, keys)?;
    if !analysis.admitted {
        return Err(PipelineError::Value("source-not-admitted".to_string()));
    }
    deps.verify_source_snapshot(analysis)
        .map_err(source_failure)?;
    let (selected, duplicates) = selected_tracks(analysis, selection, deps)?;
    if selected.is_empty()
        || !selected
            .iter()
            .any(|track| matches!(track.content_type.as_str(), "audio" | "video"))
    {
        return Err(PipelineError::Value("source-no-av-tracks".to_string()));
    }
    // A retry cannot leave previously successful manifests visible after failure.
    if manifest_dir.exists() {
        fs::remove_dir_all(&manifest_dir).map_err(|error| os_value(&error))?;
    }
    let mut results: Vec<TrackResult> = duplicates
        .iter()
        .map(|track| TrackResult::new(&track.id, "duplicate-not-selected", &["selection-recorded"]))
        .collect();
    let log = on_log.map(|on_log| SyncLog {
        lock: Mutex::new(()),
        on_log,
    });
    let run = Conversion {
        asset_dir: &asset_dir,
        keys,
        signaling,
        pr: &pr,
        packager_bin,
        deps,
        log: log.as_ref(),
        total: selected.len(),
    };
    let outcomes = convert_all(&run, &selected, workers);
    let mut tracks: Vec<OutputTrack> = Vec::new();
    let mut findings: Vec<String> = Vec::new();
    for (track, outcome) in selected.iter().zip(outcomes) {
        match outcome? {
            (Some(converted), None) => {
                let checks: &[&str] = if track.content_type == "text" {
                    &["text-payload-pass", "text-timing-map-pass"]
                } else {
                    &[
                        "source-correspondence-pass",
                        "timing-pass",
                        "decoded-identity-pass",
                        "cbcs-signaling-pass",
                    ]
                };
                let mut result = TrackResult::new(&track.id, "packaged", checks);
                result.segments = segment_rows(&converted);
                results.push(result);
                tracks.push(converted);
            }
            (_, token) => {
                let token = token.unwrap_or_else(|| "track-packaging-failed".to_string());
                findings.push(token.clone());
                results.push(TrackResult::new(&track.id, "failed", &[token.as_str()]));
            }
        }
    }
    let mut manifests: Vec<PathBuf> = Vec::new();
    match deps.verify_source_snapshot(analysis) {
        Ok(()) => {}
        // `except ValueError` (a `SourceError` is one)
        Err(Failure::Source(_)) => findings.push("source-snapshot-changed".to_string()),
        Err(Failure::Escaped(escaped)) if escaped.class == "ValueError" => {
            findings.push("source-snapshot-changed".to_string());
        }
        Err(other) => return Err(source_failure(other)),
    }
    let videos: Vec<&OutputTrack> = tracks
        .iter()
        .filter(|track| track.content_type == "video")
        .collect();
    findings.extend(video_timelines_match(&videos));
    if findings.is_empty() && tracks.len() == selected.len() {
        let (origin, adjusted) = common_origin(&tracks)?;
        let presentation = PackagedPresentation {
            tracks: adjusted,
            key_id_hex: keys.key_id_hex().to_string(),
            widevine_pssh: signaling.widevine_pssh.clone(),
            playready: pr.clone(),
            origin: to_fraction(origin),
        };
        match write_manifests(&run, &presentation, &manifest_dir)? {
            Some(paths) => manifests = paths,
            None => findings.push("manifest-generation-failed".to_string()),
        }
    }
    write_report(analysis, &asset_dir, manifests, results, findings)
}

/// A manifest generator failure that is not a `ValueError` or an `OSError` (which become the
/// `manifest-generation-failed` finding): it propagates, sorted by its class.
fn escaped_manifest(error: &PyErr) -> PipelineError {
    match error.class_name() {
        "TypeError" => PipelineError::Value(error.to_string()),
        _ => PipelineError::Infrastructure(error.to_string()),
    }
}

/// Both manifests under `manifests/clear-hls/`; `None` (and no directory) when the generator
/// raised a `ValueError` or an `OSError`.
fn write_manifests(
    run: &Conversion<'_>,
    presentation: &PackagedPresentation,
    manifest_dir: &Path,
) -> Result<Option<Vec<PathBuf>>, PipelineError> {
    fs::create_dir_all(manifest_dir).map_err(|error| os_value(&error))?;
    let fairplay = PrivateFairPlaySignaling {
        uri: run.signaling.fairplay_uri.clone(),
        iv_hex: run.keys.iv_hex().to_string(),
    };
    let written = run
        .deps
        .write_dash(presentation, &manifest_dir.join("manifest.mpd"))
        .and_then(|dash| {
            run.deps
                .write_hls(presentation, &manifest_dir.join("master.m3u8"), &fairplay)
                .map(|hls| std::iter::once(dash).chain(hls).collect::<Vec<PathBuf>>())
        });
    match written {
        Ok(paths) => Ok(Some(paths)),
        Err(error) if error.is_value_error() || matches!(error, PyErr::Os(_)) => {
            let _ = fs::remove_dir_all(manifest_dir);
            Ok(None)
        }
        Err(error) => Err(escaped_manifest(&error)),
    }
}

/// `reports/package-hls-summary.json`: `json.dumps(indent=2)` and a final newline.
fn write_report(
    analysis: &SourceAnalysis,
    asset_dir: &Path,
    manifests: Vec<PathBuf>,
    results: Vec<TrackResult>,
    findings: Vec<String>,
) -> Result<PackageHlsReport, PipelineError> {
    let path = asset_dir.join("reports").join("package-hls-summary.json");
    let report = PackageHlsReport {
        analysis_id: analysis.analysis_id.clone(),
        asset_id: analysis.asset_id.clone(),
        success: findings.is_empty(),
        manifest_paths: manifests,
        track_results: results,
        findings,
        report_path: Some(path.clone()),
        schema_version: "1".to_string(),
        production_verified: false,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| os_value(&error))?;
    }
    let text = format!("{}\n", Dumps::INDENT2.dumps(&report.to_public_dict()));
    fs::write(&path, text).map_err(|error| os_value(&error))?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::{
        Frac, TrackError, frac, frac_from_decimal, py_max, py_splitlines, round_half_even,
        vtt_cue_starts, vtt_time,
    };

    #[test]
    fn rounding_is_half_to_even_like_fraction_round() {
        assert_eq!(round_half_even(frac(5, 2)), 2);
        assert_eq!(round_half_even(frac(7, 2)), 4);
        assert_eq!(round_half_even(frac(-5, 2)), -2);
        assert_eq!(round_half_even(frac(-7, 2)), -4);
        assert_eq!(round_half_even(frac(1, 3)), 0);
        assert_eq!(round_half_even(frac(2, 3)), 1);
        assert_eq!(round_half_even(frac(-1, 2)), 0);
        assert_eq!(round_half_even(frac(-3, 2)), -2);
        assert_eq!(round_half_even(Frac::from_integer(9)), 9);
    }

    #[test]
    fn decimal_literals_are_exact_fractions() {
        assert_eq!(frac_from_decimal("00.000"), Some(Frac::from_integer(0)));
        assert_eq!(frac_from_decimal("05.500"), Some(frac(11, 2)));
        assert_eq!(frac_from_decimal("59.001"), Some(frac(59001, 1000)));
        assert_eq!(frac_from_decimal("7"), Some(Frac::from_integer(7)));
        assert_eq!(frac_from_decimal("a.b"), None);
        assert_eq!(frac_from_decimal(""), None);
    }

    #[test]
    fn vtt_times_follow_the_python_pattern() {
        assert_eq!(vtt_time("00:00:01.500"), Ok(frac(3, 2)));
        assert_eq!(vtt_time("01:00.250"), Ok(frac(241, 4)));
        assert_eq!(
            vtt_time("123:00:00.000"),
            Ok(Frac::from_integer(123 * 3600))
        );
        for bad in [
            "1:00:00.000",
            "00:60:00.000",
            "00:00:00.00",
            "00:00:00,000",
            " 00:00:00.000",
        ] {
            assert_eq!(
                vtt_time(bad),
                Err(TrackError::Media(
                    "unsupported-subtitle-profile".to_string()
                )),
                "{bad}"
            );
        }
    }

    #[test]
    fn cue_starts_skip_notes_styles_and_regions_and_validate_every_timing_line() {
        let content = "WEBVTT\n\nNOTE a comment\n\nSTYLE\n::cue { color: red }\n\nREGION\nid:r\n\n1\n00:00:01.000 --> 00:00:02.000 line:0\nHi\n\n00:00:03.000 --> 00:00:04.000\nThere\n";
        assert_eq!(
            vtt_cue_starts(content),
            Ok(vec![Frac::from_integer(1), Frac::from_integer(3)])
        );
        let unsupported = Err(TrackError::Media(
            "unsupported-subtitle-profile".to_string(),
        ));
        assert_eq!(
            vtt_cue_starts("WEBVTT\n\n00:00:02.000 --> 00:00:01.000\nx\n"),
            unsupported
        );
        assert_eq!(vtt_cue_starts("WEBVTT\n\nno timing here\n"), unsupported);
        assert_eq!(
            vtt_cue_starts("WEBVTT\n\n00:00:01.000-->00:00:02.000\nx\n"),
            unsupported
        );
        assert_eq!(vtt_cue_starts("WEBVTT\n"), Ok(vec![]));
    }

    #[test]
    fn splitlines_breaks_where_python_does() {
        assert_eq!(py_splitlines("a\nb\r\nc\rd"), vec!["a", "b", "c", "d"]);
        assert_eq!(
            py_splitlines("a\x0bb\x0cc\x1cd\u{2028}e\n"),
            vec!["a", "b", "c", "d", "e"]
        );
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
    }

    #[test]
    #[allow(clippy::float_cmp)] // the exact values Python's max returns
    fn max_keeps_the_first_argument_unless_the_second_is_greater() {
        assert_eq!(py_max(1_000_000.0, 7.5), 1_000_000.0);
        assert_eq!(py_max(1_000_000.0, 2_000_000.5), 2_000_000.5);
        assert_eq!(py_max(1_000_000.0, f64::NAN), 1_000_000.0);
    }
}
