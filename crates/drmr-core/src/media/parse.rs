//! The parse halves of the media helpers: the `mp4dump --format json` tree readers of
//! `media_inspect` (`parse_tenc_constant_iv_hex`, `parse_init_probe`,
//! `parse_fragment_boundary`) and the ffprobe JSON readers that `hls_media` and
//! `media_inspect` write inline. Pure: no tool runs here, so the engines and the tests share
//! them. Every lookup follows Python's: `d["k"]` on a dict is a `KeyError` when missing and a
//! `TypeError` on anything else, iteration walks a list's items (a dict's keys, a string's
//! characters), and the values are converted with Python's `int` / `float` / `Fraction`.

use super::python::{self, eq_int, repr_str, truthy, type_name};
use super::{
    FragmentBoundary, HexSecret, HlsMediaError, InitProbe, MediaError, PacketInfo, SegmentTiming,
};
use num_bigint::BigInt;
use num_traits::{FromPrimitive as _, One as _, ToPrimitive as _};
use serde_json::Value;

/// `_walk_boxes`: every dict of an mp4dump tree, depth first in document order (a dict before
/// its values; every value of a dict is walked, not only `children`).
fn walk_boxes(node: &Value) -> Vec<&serde_json::Map<String, Value>> {
    let mut boxes = Vec::new();
    collect(node, &mut boxes);
    boxes
}

fn collect<'a>(node: &'a Value, boxes: &mut Vec<&'a serde_json::Map<String, Value>>) {
    match node {
        Value::Object(map) => {
            boxes.push(map);
            for value in map.values() {
                collect(value, boxes);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect(item, boxes);
            }
        }
        _ => {}
    }
}

fn named(map: &serde_json::Map<String, Value>, name: &str) -> bool {
    map.get("name").and_then(Value::as_str) == Some(name)
}

/// `_hex_from_bracketed_bytes`: mp4dump's `"[44 0e 84 86 …]"` as `"440e8486…"`. Python's
/// `ValueError` quotes the value; this one never does (the value may be the constant IV).
fn hex_from_bracketed_bytes(value: &Value) -> Result<String, MediaError> {
    let Value::String(text) = value else {
        return Err(MediaError::uncaught(
            "AttributeError",
            format!("'{}' object has no attribute 'strip'", type_name(value)),
        ));
    };
    let stripped = python::strip(text);
    let inner = stripped
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .filter(|inner| {
            !inner.is_empty() && inner.chars().all(|c| c.is_ascii_hexdigit() || c == ' ')
        })
        .ok_or_else(|| MediaError::Value("not a bracketed byte array".to_string()))?;
    Ok(inner.replace(' ', "").to_ascii_lowercase())
}

/// `media::parse_tenc_constant_iv_hex`: the first `tenc` box's constant IV as lower-case
/// hex; `None` without a `tenc` box, for per-sample IVs (`default_Per_Sample_IV_Size != 0`) or
/// without a constant IV.
pub fn parse_tenc_constant_iv_hex(mp4dump_tree: &Value) -> Result<Option<HexSecret>, MediaError> {
    for found in walk_boxes(mp4dump_tree) {
        if !named(found, "tenc") {
            continue;
        }
        if found
            .get("default_Per_Sample_IV_Size")
            .is_some_and(|size| !eq_int(size, 0))
        {
            return Ok(None);
        }
        return match found.get("default_constant_IV") {
            Some(iv) if truthy(iv) => Ok(Some(HexSecret::new(hex_from_bracketed_bytes(iv)?))),
            _ => Ok(None),
        };
    }
    Ok(None)
}

/// The typed form of a JSON `scheme_type` (see [`InitProbe`]).
fn scheme_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(crate::canonical::Dumps::DEFAULT.dumps(other)),
    }
}

/// The typed form of a JSON `default_Per_Sample_IV_Size` (see [`InitProbe`]): an integer, a
/// `bool` as 0 / 1, an integral float as its integer; anything else (never equal to 0 in
/// Python) as absent.
fn iv_size(value: &Value) -> Option<i64> {
    match value {
        Value::Bool(flag) => Some(i64::from(*flag)),
        Value::Number(number) => number.as_i64().or_else(|| {
            let float = number.as_f64().filter(|float| float.fract() == 0.0)?;
            BigInt::from_f64(float)?.to_i64()
        }),
        _ => None,
    }
}

/// `media::parse_init_probe`: the first `schm` box's `scheme_type`; the first `tenc`
/// box's KID, per-sample IV size and — when that size is 0 — constant IV.
pub fn parse_init_probe(mp4dump_tree: &Value) -> Result<InitProbe, MediaError> {
    let boxes = walk_boxes(mp4dump_tree);
    let mut probe = InitProbe::default();
    if let Some(schm) = boxes.iter().find(|found| named(found, "schm")) {
        probe.scheme_type = schm.get("scheme_type").and_then(scheme_text);
    }
    if let Some(tenc) = boxes.iter().find(|found| named(found, "tenc")) {
        let size = tenc.get("default_Per_Sample_IV_Size");
        probe.per_sample_iv_size = size.and_then(iv_size);
        if let Some(kid) = tenc.get("default_KID").filter(|kid| truthy(kid)) {
            probe.default_kid_hex = Some(hex_from_bracketed_bytes(kid)?);
        }
        if size.is_some_and(|size| eq_int(size, 0))
            && let Some(iv) = tenc.get("default_constant_IV").filter(|iv| truthy(iv))
        {
            probe.constant_iv_hex = Some(HexSecret::new(hex_from_bracketed_bytes(iv)?));
        }
    }
    Ok(probe)
}

/// `media::parse_fragment_boundary`: the first non-null `base media decode time` of a
/// `tfdt` box and the first non-null `sample count` of a `trun` box (never `saiz`'s), `None`
/// unless both exist.
pub fn parse_fragment_boundary(mp4dump_tree: &Value) -> Option<FragmentBoundary> {
    let mut tfdt: Option<&Value> = None;
    let mut sample_count: Option<&Value> = None;
    for found in walk_boxes(mp4dump_tree) {
        if named(found, "tfdt") && tfdt.is_none() {
            tfdt = found.get("base media decode time").filter(|v| !v.is_null());
        }
        if named(found, "trun") && sample_count.is_none() {
            sample_count = found.get("sample count").filter(|v| !v.is_null());
        }
    }
    Some(FragmentBoundary {
        base_media_decode_time: tfdt?.clone(),
        sample_count: sample_count?.clone(),
    })
}

/// `container[key]`: a `KeyError` when a dict lacks it, a `TypeError` on a non-dict.
pub(super) fn getitem<'a>(container: &'a Value, key: &str) -> Result<&'a Value, MediaError> {
    match container {
        Value::Object(map) => map.get(key).ok_or_else(|| MediaError::Key(repr_str(key))),
        Value::Array(_) => Err(MediaError::uncaught(
            "TypeError",
            "list indices must be integers or slices, not str".to_string(),
        )),
        Value::String(_) => Err(MediaError::uncaught(
            "TypeError",
            "string indices must be integers".to_string(),
        )),
        other => Err(MediaError::uncaught(
            "TypeError",
            format!("'{}' object is not subscriptable", type_name(other)),
        )),
    }
}

/// `for item in value`: a list's items, a dict's keys, a string's characters; a `TypeError`
/// for anything else.
pub(super) fn iterate(value: &Value) -> Result<Vec<Value>, MediaError> {
    match value {
        Value::Array(items) => Ok(items.clone()),
        Value::Object(map) => Ok(map.keys().cloned().map(Value::String).collect()),
        Value::String(text) => Ok(text.chars().map(|c| Value::String(c.to_string())).collect()),
        other => Err(MediaError::uncaught(
            "TypeError",
            format!("'{}' object is not iterable", type_name(other)),
        )),
    }
}

/// `dict.get(key, default)` on a value that must be a dict (`AttributeError` otherwise).
fn get_or<'a>(
    container: &'a Value,
    key: &str,
    default: &'a Value,
) -> Result<&'a Value, MediaError> {
    match container {
        Value::Object(map) => Ok(map.get(key).unwrap_or(default)),
        other => Err(MediaError::uncaught(
            "AttributeError",
            format!("'{}' object has no attribute 'get'", type_name(other)),
        )),
    }
}

fn hls(token: &str) -> MediaError {
    HlsMediaError::new(token).into()
}

/// The parse of `media::probe_packets` over ffprobe's `-show_streams -show_packets` JSON:
/// the stream by `index` (`source-stream-missing`), its `time_base` with numerator 1
/// (`unsupported-timebase`), its packets with `pts`, `dts` and `duration`
/// (`unsupported-packet-timing`), every duration positive (`invalid-packet-duration`).
pub fn packets_from_probe(data: &Value, stream_index: i64) -> Result<PacketInfo, MediaError> {
    // streams = {int(s["index"]): s for s in data["streams"]} — a later index replaces an earlier.
    let mut streams: Vec<(i128, Value)> = Vec::new();
    for stream in iterate(getitem(data, "streams")?)? {
        let index = python::int(getitem(&stream, "index")?)?;
        match streams.iter_mut().find(|(known, _)| *known == index) {
            Some(entry) => entry.1 = stream,
            None => streams.push((index, stream)),
        }
    }
    let stream = streams
        .iter()
        .find(|(index, _)| *index == i128::from(stream_index))
        .map(|(_, stream)| stream)
        .ok_or_else(|| hls("source-stream-missing"))?;
    let timebase = python::fraction(getitem(stream, "time_base")?)?;
    if !timebase.numer().is_one() {
        return Err(hls("unsupported-timebase"));
    }
    let mut packets = Vec::new();
    for packet in iterate(getitem(data, "packets")?)? {
        if python::int(getitem(&packet, "stream_index")?)? == i128::from(stream_index) {
            packets.push(packet);
        }
    }
    let has_timing = |packet: &Value| {
        ["pts", "dts", "duration"]
            .iter()
            .all(|key| packet.as_object().is_some_and(|map| map.contains_key(*key)))
    };
    if packets.is_empty() || !packets.iter().all(has_timing) {
        return Err(hls("unsupported-packet-timing"));
    }
    for packet in &packets {
        if python::int(getitem(packet, "duration")?)? <= 0 {
            return Err(hls("invalid-packet-duration"));
        }
    }
    let column = |key: &str| -> Result<Vec<i64>, MediaError> {
        packets
            .iter()
            .map(|packet| python::int64(getitem(packet, key)?))
            .collect()
    };
    let timescale = timebase.denom().to_i64().ok_or_else(|| {
        MediaError::uncaught(
            "OverflowError",
            "time_base denominator beyond i64".to_string(),
        )
    })?;
    let (pts, dts, durations) = (column("pts")?, column("dts")?, column("duration")?);
    let frame_rate = match stream.get("avg_frame_rate") {
        None => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => crate::canonical::Dumps::DEFAULT.dumps(other),
    };
    Ok(PacketInfo {
        timescale,
        pts,
        dts,
        durations,
        frame_rate,
    })
}

/// One packet's payload from ffprobe's `-show_data` hex dump: per non-blank line, the hex
/// words between the offset's `:` and the two spaces before the printable column.
fn payload_of(packet: &Value) -> Result<Vec<u8>, MediaError> {
    let data = getitem(packet, "data")?;
    let Value::String(dump) = data else {
        return Err(MediaError::uncaught(
            "AttributeError",
            format!("'{}' object has no attribute 'splitlines'", type_name(data)),
        ));
    };
    let mut payload = Vec::new();
    for line in python::splitlines(dump) {
        if python::strip(line).is_empty() {
            continue;
        }
        let (_, rest) = line.split_once(':').ok_or_else(|| {
            MediaError::uncaught("IndexError", "list index out of range".to_string())
        })?;
        let words = python::strip(rest);
        let hexadecimal = words.split_once("  ").map_or(words, |(hex, _)| hex);
        let mut bytes = python::fromhex(hexadecimal)?;
        payload.append(&mut bytes);
    }
    Ok(payload)
}

/// The parse of `media::packet_payloads` over ffprobe's `-show_packets -show_data` JSON:
/// every packet of the stream (compared without `int`), its payload of
/// exactly `size` bytes. A missing field, a bad line or a size mismatch is
/// `text-payload-invalid`; a non-string `data` is Python's uncaught `AttributeError`.
pub fn payloads_from_probe(data: &Value, stream_index: i64) -> Result<Vec<Vec<u8>>, MediaError> {
    let invalid = || hls("text-payload-invalid");
    let caught = |error: MediaError| match error {
        MediaError::Key(_)
        | MediaError::Value(_)
        | MediaError::Uncaught {
            class: "IndexError" | "TypeError",
            ..
        } => invalid(),
        other => other,
    };
    let mut result = Vec::new();
    for packet in iterate(getitem(data, "packets").map_err(caught)?).map_err(caught)? {
        if !eq_int(
            getitem(&packet, "stream_index").map_err(caught)?,
            stream_index,
        ) {
            continue;
        }
        let payload = payload_of(&packet).map_err(caught)?;
        let size = python::int(getitem(&packet, "size").map_err(caught)?).map_err(caught)?;
        if i128::try_from(payload.len()).ok() != Some(size) {
            return Err(invalid());
        }
        result.push(payload);
    }
    Ok(result)
}

/// The parse of `subtitle payload verification`'s `payloads` over ffprobe's
/// `-show_packets -show_data_hash sha256` JSON: each packet's `data_hash` as printed, for the
/// packets whose `stream_index == stream_index`.
pub fn payload_hashes_from_probe(
    data: &Value,
    stream_index: i64,
) -> Result<Vec<Value>, MediaError> {
    let mut hashes = Vec::new();
    for packet in iterate(getitem(data, "packets")?)? {
        if eq_int(getitem(&packet, "stream_index")?, stream_index) {
            hashes.push(getitem(&packet, "data_hash")?.clone());
        }
    }
    Ok(hashes)
}

/// The parse of `media::probe_segment_timing` over ffprobe's
/// `-show_entries format=duration -show_entries packet=pts_time` JSON:
/// `float(data.get("format", {}).get("duration", "nan"))` and `len(data.get("packets", []))`.
pub fn segment_timing_from_probe(data: &Value) -> Result<SegmentTiming, MediaError> {
    let empty_dict = Value::Object(serde_json::Map::new());
    let nan = Value::String("nan".to_string());
    let format = get_or(data, "format", &empty_dict)?;
    let duration_s = python::float(get_or(format, "duration", &nan)?)?;
    let empty_list = Value::Array(Vec::new());
    let packets = get_or(data, "packets", &empty_list)?;
    let packet_count = match packets {
        Value::Array(items) => items.len(),
        Value::Object(map) => map.len(),
        Value::String(text) => text.chars().count(),
        other => {
            return Err(MediaError::uncaught(
                "TypeError",
                format!("object of type '{}' has no len()", type_name(other)),
            ));
        }
    };
    Ok(SegmentTiming {
        duration_s,
        packet_count,
    })
}
