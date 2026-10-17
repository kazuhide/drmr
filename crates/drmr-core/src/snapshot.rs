//! Deterministic clear-HLS source snapshots and offline verification.
//!
//! The digest hashes canonical JSON without the digest and analysis-ID fields.
//! The analysis ID is `hls-` plus the first 24 digest digits. Verification recomputes
//! the digest and rehashes recorded resources without fetching them again.

use crate::canonical::Dumps;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};

/// The private record's file name inside the snapshot directory.
pub const SNAPSHOT_FILE: &str = "analysis.json";

/// The error tokens of `verify_source_snapshot`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    /// The record cannot be read or lacks the shape the tool writes.
    #[error("snapshot-invalid")]
    Invalid,
    /// The record or a recorded file no longer matches what was frozen.
    #[error("snapshot-changed")]
    Changed,
}

impl SnapshotError {
    /// The token the implementation reports (`SourceError`'s message).
    pub fn token(self) -> &'static str {
        match self {
            SnapshotError::Invalid => "snapshot-invalid",
            SnapshotError::Changed => "snapshot-changed",
        }
    }
}

/// Lower-case hex SHA-256 (`source::_hash`).
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// The SHA-256 of a file, streamed (a segment can be hundreds of megabytes).
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher)?;
    Ok(hex::encode(hasher.finalize()))
}

/// The digest of a private record that carries neither `snapshot_digest` nor `analysis_id`.
pub fn snapshot_digest(private: &Value) -> String {
    sha256_hex(Dumps::CANONICAL.dumps(private).as_bytes())
}

/// `"hls-" + digest[:24]`.
pub fn analysis_id(digest: &str) -> String {
    let prefix: String = digest.chars().take(24).collect();
    format!("hls-{prefix}")
}

/// What a verification measured.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SnapshotReport {
    /// `asset_id` of the record.
    pub asset_id: String,
    /// `analysis_id` of the record.
    pub analysis_id: String,
    /// `snapshot_digest` of the record.
    pub recorded_digest: String,
    /// The digest recomputed from the record.
    pub measured_digest: String,
    /// Number of recorded resources (playlists and segments).
    pub resources: usize,
    /// Number of recorded tracks.
    pub tracks: usize,
    /// Number of files re-hashed, when files were verified.
    pub files_verified: Option<usize>,
}

/// `path` as the record file: the directory's `analysis.json`, or the file itself.
pub fn record_path(path: &Path) -> PathBuf {
    if path.is_dir() {
        path.join(SNAPSHOT_FILE)
    } else {
        path.to_path_buf()
    }
}

/// Recompute the digest of the record at `path` and check `analysis_id` derives from it.
pub fn verify_record(path: &Path) -> Result<SnapshotReport, SnapshotError> {
    let (report, _) = load(path)?;
    Ok(report)
}

/// [`verify_record`], then re-hash every recorded resource file (confined to the snapshot
/// directory); a missing or unreadable file is `snapshot-invalid`, a changed one
/// `snapshot-changed`.
pub fn verify_files(path: &Path) -> Result<SnapshotReport, SnapshotError> {
    let file = record_path(path);
    let (mut report, private) = load(&file)?;
    let directory = file.parent().ok_or(SnapshotError::Invalid)?;
    let resources = private
        .get("resources")
        .and_then(Value::as_array)
        .ok_or(SnapshotError::Invalid)?;
    let mut verified = 0;
    for resource in resources {
        let relative = resource
            .get("path")
            .and_then(Value::as_str)
            .ok_or(SnapshotError::Invalid)?;
        let recorded = resource
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or(SnapshotError::Invalid)?;
        let local = confined_file(directory, relative).ok_or(SnapshotError::Invalid)?;
        let measured = sha256_file(&local).map_err(|_| SnapshotError::Invalid)?;
        if measured != recorded {
            return Err(SnapshotError::Changed);
        }
        verified += 1;
    }
    // Python's `read_analysis` resolves every track file the same way (`_safe_local`: inside
    // the directory, a regular file); a record whose track files are gone or escaped is invalid.
    let tracks = private
        .get("tracks")
        .and_then(Value::as_array)
        .ok_or(SnapshotError::Invalid)?;
    for track in tracks {
        if let Some(init) = track.get("init_path").and_then(Value::as_str) {
            confined_file(directory, init).ok_or(SnapshotError::Invalid)?;
        }
        let segments = track
            .get("segments")
            .and_then(Value::as_array)
            .ok_or(SnapshotError::Invalid)?;
        for segment in segments {
            let path = segment
                .get("path")
                .and_then(Value::as_str)
                .ok_or(SnapshotError::Invalid)?;
            confined_file(directory, path).ok_or(SnapshotError::Invalid)?;
        }
    }
    report.files_verified = Some(verified);
    Ok(report)
}

/// [`safe_local`], then the file as it really is: resolved (symlinks followed) it must still lie
/// under the resolved snapshot directory and be a regular file — `source::_safe_local`
/// (`resolve`, `is_relative_to`, `is_file`). Returns the resolved file.
pub fn confined_file(directory: &Path, relative: &str) -> Option<PathBuf> {
    let candidate = safe_local(directory, relative)?;
    let root = std::fs::canonicalize(directory).ok()?;
    let real = std::fs::canonicalize(&candidate).ok()?;
    (real.starts_with(&root) && real.is_file()).then_some(real)
}

/// `directory / relative` as `pathlib` joins it (`.` components and a trailing `/` dropped),
/// only when `relative` is a non-empty relative path without `..`: the lexical half of the
/// confinement (keeps the lexical refusal of `..` and absolute paths);
/// [`verify_files`] then resolves the real file.
pub fn safe_local(directory: &Path, relative: &str) -> Option<PathBuf> {
    if relative.is_empty() {
        return None;
    }
    let mut joined = directory.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(part) => joined.push(part),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(joined)
}

fn load(path: &Path) -> Result<(SnapshotReport, Value), SnapshotError> {
    let file = record_path(path);
    let bytes = std::fs::read(&file).map_err(|_| SnapshotError::Invalid)?;
    let mut private: Value = serde_json::from_slice(&bytes).map_err(|_| SnapshotError::Invalid)?;
    let map = private.as_object_mut().ok_or(SnapshotError::Invalid)?;
    if map.get("schema_version").and_then(Value::as_str) != Some("1") {
        return Err(SnapshotError::Invalid);
    }
    let recorded_digest = take_recorded(map, "snapshot_digest")?;
    let analysis = take_recorded(map, "analysis_id")?;
    let asset_id = map
        .get("asset_id")
        .and_then(Value::as_str)
        .ok_or(SnapshotError::Invalid)?
        .to_string();
    let resources = map
        .get("resources")
        .and_then(Value::as_array)
        .ok_or(SnapshotError::Invalid)?
        .len();
    let tracks = map
        .get("tracks")
        .and_then(Value::as_array)
        .ok_or(SnapshotError::Invalid)?
        .len();
    let measured_digest = snapshot_digest(&private);
    let report = SnapshotReport {
        asset_id,
        analysis_id: analysis.clone(),
        recorded_digest: recorded_digest.clone(),
        measured_digest: measured_digest.clone(),
        resources,
        tracks,
        files_verified: None,
    };
    // The id is reimplemented, not checked against the digest: Python's `verify_source_snapshot`
    // compares it with the analysis's own id, never with the digest (of the P0
    // review — Rust is stricter only once Python is).
    if recorded_digest != measured_digest {
        return Err(SnapshotError::Changed);
    }
    Ok((report, private))
}

/// The recorded digest or id: absent is `snapshot-invalid` (Python's `KeyError`); present but
/// not a string compares unequal to the measured value, `snapshot-changed` (Python's `!=`).
fn take_recorded(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
) -> Result<String, SnapshotError> {
    match map.shift_remove(key) {
        None => Err(SnapshotError::Invalid),
        Some(Value::String(text)) => Ok(text),
        Some(_) => Err(SnapshotError::Changed),
    }
}
