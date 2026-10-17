//! Read and verify frozen source records without network access.
//!
//! Confine every recorded path to the snapshot directory, rehash files, and compare
//! the track graph. Numeric comparisons preserve compatible JSON value semantics.
//! Older sequential duration totals remain accepted; other duration totals must be
//! within `1e-9` of the accurately summed total.

use super::fsum::{fsum, isclose_abs, sequential_sum};
use super::pypath::resolve;
use super::pytext::{py_float, py_int};
use super::{ByteRange, Escaped, Failure, SourceAnalysis, SourceSegment, SourceTrack};
use crate::snapshot::{confined_file, sha256_file, snapshot_digest};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::Path;

fn invalid() -> Failure {
    Failure::source("snapshot-invalid")
}

fn changed() -> Failure {
    Failure::source("snapshot-changed")
}

/// `read_analysis(path)`: the analysis recorded in `path` (an `analysis.json`), verified.
/// Paths resolve as Python's `_safe_local` does (inside the snapshot directory, a regular
/// file; `..` and absolute paths are refused lexically as well). The analysis id
/// is reimplemented as recorded, not derived from the digest.
pub fn read_analysis(path: &Path) -> Result<SourceAnalysis, Failure> {
    let path = resolve(path).map_err(Failure::Escaped)?;
    let bytes = std::fs::read(&path).map_err(|_| invalid())?;
    let data: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let Value::Object(data) = data else {
        return Err(Failure::Escaped(Escaped::new("AttributeError")));
    };
    if data.get("schema_version") != Some(&Value::from("1")) {
        return Err(invalid());
    }
    let directory = path.parent().ok_or_else(invalid)?;
    let mut tracks = Vec::new();
    for row in iterate(data.get("tracks").ok_or_else(invalid)?)? {
        tracks.push(read_track(&row, directory)?);
    }
    let analysis_id = string(data.get("analysis_id").ok_or_else(invalid)?)?;
    let asset_id = string(data.get("asset_id").ok_or_else(invalid)?)?;
    let snapshot_digest = string(data.get("snapshot_digest").ok_or_else(invalid)?)?;
    let analysis = SourceAnalysis {
        schema_version: "1".to_string(),
        source_kind: "hls-clear".to_string(),
        analysis_id,
        asset_id,
        admitted: true,
        tracks,
        findings: Vec::new(),
        snapshot_path: path.clone(),
        snapshot_digest,
    };
    verify_source_snapshot(&analysis)?;
    Ok(analysis)
}

/// `for item in value`: a list's items, a dict's keys, a string's characters; anything else
/// is not iterable (`TypeError`: `snapshot-invalid`).
fn iterate(value: &Value) -> Result<Vec<Value>, Failure> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(map) => Ok(map.keys().map(|key| Value::from(key.as_str())).collect()),
        Value::String(text) => Ok(text.chars().map(|c| Value::from(c.to_string())).collect()),
        _ => Err(invalid()),
    }
}

/// Read a required text field from a recorded snapshot.
/// Non-string values are rejected as `snapshot-invalid`.
fn string(value: &Value) -> Result<String, Failure> {
    match value {
        Value::String(text) => Ok(text.clone()),
        _ => Err(invalid()),
    }
}

/// `int(value)` of a decoded JSON value.
fn py_int_value(value: &Value) -> Result<i64, Failure> {
    let integer = match value {
        Value::Bool(flag) => i128::from(*flag),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                i128::from(integer)
            } else if let Some(integer) = number.as_u64() {
                i128::from(integer)
            } else {
                let float = number.as_f64().ok_or_else(invalid)?;
                #[allow(clippy::cast_possible_truncation)] // int(float) truncates; range checked
                let truncated = float.trunc() as i128;
                truncated
            }
        }
        Value::String(text) => py_int(text).ok_or_else(invalid)?,
        _ => return Err(invalid()),
    };
    i64::try_from(integer).map_err(|_| invalid())
}

/// `float(value)` of a decoded JSON value.
fn py_float_value(value: &Value) -> Result<f64, Failure> {
    match value {
        Value::Bool(flag) => Ok(if *flag { 1.0 } else { 0.0 }),
        Value::Number(number) => number.as_f64().ok_or_else(invalid),
        Value::String(text) => py_float(text).ok_or_else(invalid),
        _ => Err(invalid()),
    }
}

/// Python's truth of a decoded JSON value.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// `_safe_local(directory, value)` for a recorded path, through `confined_file`'s
/// canonicalisation: a symlink loop is `snapshot-invalid` here, where 3.11's `resolve` raises
/// `RuntimeError` past the except tuples — an accepted divergence : Python's planned
/// fix classifies it the same way, and `verify-snapshot` shares the helper.
fn local(directory: &Path, value: &Value) -> Result<std::path::PathBuf, Failure> {
    let Value::String(relative) = value else {
        return Err(invalid());
    };
    confined_file(directory, relative).ok_or_else(invalid)
}

fn read_segment(segment: &Value, directory: &Path) -> Result<SourceSegment, Failure> {
    let Value::Object(segment) = segment else {
        return Err(invalid());
    };
    let index = py_int_value(segment.get("index").ok_or_else(invalid)?)?;
    let path = local(directory, segment.get("path").ok_or_else(invalid)?)?;
    let duration = py_float_value(segment.get("duration").ok_or_else(invalid)?)?;
    let sha256 = string(segment.get("sha256").ok_or_else(invalid)?)?;
    let byte_range = read_byte_range(segment.get("byte_range"))?;
    Ok(SourceSegment {
        index,
        path,
        duration,
        sha256,
        byte_range,
    })
}

/// The snapshot stores byte ranges as `[offset, length]`, with a nonempty length.
fn read_byte_range(range: Option<&Value>) -> Result<Option<ByteRange>, Failure> {
    let Some(range) = range.filter(|value| !value.is_null()) else {
        return Ok(None);
    };
    let Value::Array(items) = range else {
        return Err(invalid());
    };
    let [offset, length] = items.as_slice() else {
        return Err(invalid());
    };
    let offset = offset.as_u64().ok_or_else(invalid)?;
    let length = length
        .as_u64()
        .filter(|length| *length > 0)
        .ok_or_else(invalid)?;
    Ok(Some(ByteRange { offset, length }))
}

/// A recorded flag (`is_default`, `autoselect`): a boolean, else `invalid-selection-flags`.
fn flag(row: &Map<String, Value>, key: &str) -> Result<bool, Failure> {
    match row.get(key) {
        None => Ok(false),
        Some(Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(Failure::source("invalid-selection-flags")),
    }
}

/// A recorded boolean that is not a selection flag (`muxed`): a boolean, or a number equal to
/// one (Python's `1 == True` keeps the record verifiable).
fn boolean(value: &Value) -> Result<bool, Failure> {
    match value {
        Value::Bool(flag) => Ok(*flag),
        Value::Number(number) if number.as_f64() == Some(0.0) => Ok(false),
        Value::Number(number) if number.as_f64() == Some(1.0) => Ok(true),
        _ => Err(invalid()),
    }
}

/// A recorded optional count (`width`, `height`, `bandwidth`).
fn count(value: Option<&Value>) -> Result<Option<u64>, Failure> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => {
            if let Some(integer) = number.as_u64() {
                return Ok(Some(integer));
            }
            match number.as_f64() {
                Some(float) if float >= 0.0 && float.fract() == 0.0 && float < 1.8e19 => {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    // reason: integral, non-negative and below 2**64: checked above
                    let integer = float as u64;
                    Ok(Some(integer))
                }
                _ => Err(invalid()),
            }
        }
        Some(_) => Err(invalid()),
    }
}

fn text_or(row: &Map<String, Value>, key: &str, default: &str) -> Result<String, Failure> {
    row.get(key).map_or_else(|| Ok(default.to_string()), string)
}

fn read_track(row: &Value, directory: &Path) -> Result<SourceTrack, Failure> {
    let Value::Object(row) = row else {
        return Err(invalid());
    };
    let mut segments = Vec::new();
    for segment in iterate(row.get("segments").ok_or_else(invalid)?)? {
        segments.push(read_segment(&segment, directory)?);
    }
    let init_path = match row.get("init_path") {
        Some(value) if truthy(value) => Some(local(directory, value)?),
        _ => None,
    };
    let init_sha256 = match row.get("init_sha256") {
        None | Some(Value::Null) => None,
        Some(value) => Some(string(value)?),
    };
    for key in ["id", "content_type", "container", "codec"] {
        if !row.contains_key(key) {
            return Err(invalid()); // a required SourceTrack argument: TypeError
        }
    }
    let is_default = flag(row, "is_default")?;
    let autoselect = flag(row, "autoselect")?;
    Ok(SourceTrack {
        id: text_or(row, "id", "")?,
        content_type: text_or(row, "content_type", "")?,
        container: text_or(row, "container", "")?,
        codec: text_or(row, "codec", "")?,
        segments,
        init_path,
        init_sha256,
        language: text_or(row, "language", "und")?,
        role: text_or(row, "role", "main")?,
        group_id: text_or(row, "group_id", "")?,
        name: text_or(row, "name", "")?,
        width: count(row.get("width"))?,
        height: count(row.get("height"))?,
        bandwidth: count(row.get("bandwidth"))?,
        codecs: text_or(row, "codecs", "")?,
        stream_index: row.get("stream_index").map_or(Ok(0), py_int_exact)?,
        playlist_id: text_or(row, "playlist_id", "")?,
        muxed: row.get("muxed").map_or(Ok(false), boolean)?,
        audio_group: text_or(row, "audio_group", "")?,
        subtitle_group: text_or(row, "subtitle_group", "")?,
        profile: text_or(row, "profile", "")?,
        is_default,
        autoselect,
    })
}

/// A recorded integer kept as it is (`stream_index`): an integer, or a float or boolean equal
/// to one.
fn py_int_exact(value: &Value) -> Result<i64, Failure> {
    match value {
        Value::Bool(flag) => Ok(i64::from(*flag)),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                return Ok(integer);
            }
            match number.as_f64() {
                Some(float) if float.fract() == 0.0 && float.abs() < 9.0e18 => {
                    #[allow(clippy::cast_possible_truncation)] // integral, in range
                    let integer = float as i64;
                    Ok(integer)
                }
                _ => Err(invalid()),
            }
        }
        _ => Err(invalid()),
    }
}

/// `verify_source_snapshot(analysis)`: the record re-read from disk, its digest recomputed and
/// compared with the recorded one and the analysis's, its id compared with the analysis's,
/// every recorded file re-hashed, and the private track graph compared with the analysis's
/// tracks (the older-total tolerance applied to each track's `duration`).
pub fn verify_source_snapshot(analysis: &SourceAnalysis) -> Result<(), Failure> {
    let bytes = std::fs::read(&analysis.snapshot_path).map_err(|_| invalid())?;
    let data: Value = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    let mut data = match data {
        Value::Object(map) => map,
        Value::Array(_) => return Err(invalid()), // list.pop("…"): TypeError
        _ => return Err(Failure::Escaped(Escaped::new("AttributeError"))),
    };
    let digest = data.shift_remove("snapshot_digest").ok_or_else(invalid)?;
    let analysis_id = data.shift_remove("analysis_id").ok_or_else(invalid)?;
    let measured = snapshot_digest(&Value::Object(data.clone()));
    if digest.as_str() != Some(measured.as_str())
        || digest.as_str() != Some(analysis.snapshot_digest.as_str())
        || analysis_id.as_str() != Some(analysis.analysis_id.as_str())
    {
        return Err(changed());
    }
    let directory = analysis.snapshot_path.parent().ok_or_else(invalid)?;
    let mut resource_ranges = HashMap::new();
    for resource in iterate(data.get("resources").ok_or_else(invalid)?)? {
        let Value::Object(resource) = resource else {
            return Err(invalid());
        };
        let path = local(directory, resource.get("path").ok_or_else(invalid)?)?;
        let range = read_byte_range(resource.get("byte_range"))?;
        resource_ranges.insert(path.clone(), range);
        let hashed = sha256_file(&path).map_err(|_| invalid())?;
        let recorded = resource.get("sha256").ok_or_else(invalid)?;
        if recorded.as_str() != Some(hashed.as_str()) {
            return Err(changed());
        }
    }
    let mut measured_tracks = Vec::with_capacity(analysis.tracks.len());
    for track in &analysis.tracks {
        for segment in &track.segments {
            let resource_range = resource_ranges.get(&segment.path).copied().flatten();
            if resource_range != segment.byte_range {
                return Err(changed());
            }
        }
        let durations = track.segments.iter().map(|segment| segment.duration);
        if fsum(durations).is_err_and(|error| error == super::fsum::FsumError::Overflow) {
            return Err(Failure::Escaped(Escaped::new("OverflowError")));
        }
        measured_tracks.push(track.to_private_dict(directory).ok_or_else(invalid)?);
    }
    let mut saved_tracks = Vec::new();
    for row in iterate(data.get("tracks").ok_or_else(invalid)?)? {
        let Value::Object(row) = row else {
            return Err(invalid()); // dict(row) of a non-mapping
        };
        saved_tracks.push(row);
    }
    for ((saved, measured), track) in saved_tracks
        .iter_mut()
        .zip(&measured_tracks)
        .zip(&analysis.tracks)
    {
        let legacy = sequential_sum(track.segments.iter().map(|segment| segment.duration));
        let fsum_total = measured
            .get("duration")
            .and_then(Value::as_f64)
            .unwrap_or(f64::NAN);
        let accepted = match saved.get("duration") {
            Some(Value::Number(number)) => {
                num_eq_float(number, legacy)
                    || isclose_abs(number.as_f64().unwrap_or(f64::NAN), fsum_total, 1e-9)
            }
            _ => false, // `type(...) not in (float, int)`: a boolean or anything else
        };
        if !accepted {
            return Err(changed());
        }
        saved.insert(
            "duration".into(),
            measured.get("duration").cloned().unwrap_or(Value::Null),
        );
    }
    // Both recorded and reconstructed ranges use their serialized JSON array form.
    let equal = saved_tracks.len() == measured_tracks.len()
        && saved_tracks
            .iter()
            .zip(&measured_tracks)
            .all(|(saved, measured)| map_eq(saved, measured));
    if !equal {
        return Err(changed());
    }
    Ok(())
}

/// A JSON number as Python sees it: an integer or a float.
enum PyNumber {
    Int(i128),
    Float(f64),
}

fn as_py_number(value: &Value) -> Option<PyNumber> {
    match value {
        Value::Bool(flag) => Some(PyNumber::Int(i128::from(*flag))),
        Value::Number(number) => Some(if let Some(integer) = number.as_i64() {
            PyNumber::Int(i128::from(integer))
        } else if let Some(integer) = number.as_u64() {
            PyNumber::Int(i128::from(integer))
        } else {
            PyNumber::Float(number.as_f64()?)
        }),
        _ => None,
    }
}

/// Python's exact comparison of an integer with a float.
fn int_eq_float(integer: i128, float: f64) -> bool {
    if !float.is_finite() || float.fract() != 0.0 || float.abs() >= 1.7e38 {
        return false;
    }
    #[allow(clippy::cast_possible_truncation)] // integral and within i128
    let as_integer = float as i128;
    as_integer == integer
}

#[allow(clippy::float_cmp)] // Python's `==`: exact
fn num_eq_float(number: &serde_json::Number, float: f64) -> bool {
    match as_py_number(&Value::Number(number.clone())) {
        Some(PyNumber::Int(integer)) => int_eq_float(integer, float),
        Some(PyNumber::Float(value)) => value == float,
        None => false,
    }
}

fn map_eq(left: &Map<String, Value>, right: &Map<String, Value>) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .all(|(key, value)| right.get(key).is_some_and(|other| py_eq(value, other)))
}

/// Python's `==` between two decoded JSON values.
pub fn py_eq(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| py_eq(x, y))
        }
        (Value::Object(a), Value::Object(b)) => map_eq(a, b),
        _ => match (as_py_number(left), as_py_number(right)) {
            (Some(PyNumber::Int(a)), Some(PyNumber::Int(b))) => a == b,
            (Some(PyNumber::Float(a)), Some(PyNumber::Float(b))) => a == b,
            (Some(PyNumber::Int(a)), Some(PyNumber::Float(b)))
            | (Some(PyNumber::Float(b)), Some(PyNumber::Int(a))) => int_eq_float(a, b),
            _ => false,
        },
    }
}
