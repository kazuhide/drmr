//! Exact audio presentation edits, applied only to caller-owned private MP4 copies.
use super::{AudioClaim, AudioEvidence, AudioPresentation, BAD};
use crate::{
    concurrency::Stop,
    conversion::{ConversionError, files, finite},
    mp4::{self, Mp4Box},
    native::FileIdentity,
};
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    path::Path,
};
use zeroize::Zeroizing;

const METADATA_LIMIT: u64 = 16 * 1024 * 1024;
const BOX_LIMIT: usize = 100_000;
const CHANGED: ConversionError = ConversionError("audio-presentation-source-changed");

pub(super) fn read(
    path: &Path,
    evidence: AudioEvidence,
) -> Result<Option<AudioPresentation>, ConversionError> {
    read_claim(path)?
        .map(|claim| claim.bind(evidence))
        .transpose()
}

/// A declaration is not evidence of the coded count, payloads, clocks or decoded samples.
pub(super) fn read_claim(path: &Path) -> Result<Option<AudioClaim>, ConversionError> {
    let mut source = Source::open(path, false, None)?;
    let Some((claim, track_id)) = claim_from_movie(&source.movie)? else {
        return Ok(None);
    };
    source.validate_fragments(track_id, None)?;
    source.check_identity(path, false)?;
    Ok(Some(claim))
}

pub(super) fn claim_from_movie(bytes: &[u8]) -> Result<Option<(AudioClaim, u32)>, ConversionError> {
    let boxes = mp4::parse_boxes(bytes).map_err(|_| BAD)?;
    let movie = only_movie(&boxes)?;
    if !single_fragmented_audio(movie, bytes)? {
        // Other track layouts keep their existing edit parser and refusal policy.
        return Ok(None);
    }
    let Some(edit) = special_edit(movie, bytes)? else {
        // Ordinary progressive edits and no-edit tracks retain their existing
        // dispatch before any requirements of this new fragmented profile apply.
        return Ok(None);
    };
    let layout = Layout::read(movie, bytes)?;
    Ok(Some((declaration(edit, bytes, layout)?, layout.track_id)))
}

fn single_fragmented_audio(movie: &Mp4Box, bytes: &[u8]) -> Result<bool, ConversionError> {
    let mut tracks = movie
        .children
        .iter()
        .filter(|child| child.kind() == *b"trak");
    let Some(track) = tracks.next() else {
        return Ok(false);
    };
    if tracks.next().is_some() || !movie.children.iter().any(|child| child.kind() == *b"mvex") {
        return Ok(false);
    }
    let media = one(track, *b"mdia")?;
    Ok(one(media, *b"hdlr")?.header.payload(bytes).get(8..12) == Some(b"soun"))
}

pub(super) fn restore(
    path: &Path,
    presentation: AudioPresentation,
    stop: &Stop,
) -> Result<(), ConversionError> {
    check_stop(stop)?;
    let presentation =
        AudioPresentation::new(presentation.evidence, presentation.presented_samples)?;
    let mut source = Source::open(path, true, Some(stop))?;
    let boxes = mp4::parse_boxes(&source.movie).map_err(|_| BAD)?;
    let movie = only_movie(&boxes)?;
    let layout = Layout::read(movie, &source.movie)?;
    source.validate_fragments(layout.track_id, Some(stop))?;
    let edits = edit_box(layout.track)?;
    if let Some(edits) = edits {
        let edit = canonical_edit(edits, &source.movie)?;
        let actual = declaration(edit, &source.movie, layout)?.bind(presentation.evidence)?;
        if actual != presentation {
            return Err(BAD);
        }
    }
    let scale = presentation.movie_scale()?;
    let edits = encoded_edit(presentation, scale)?;
    let replacement = rewrite_movie(&source.movie, movie, Some(&edits), Some(scale))?;
    rewrite(path, source, &replacement, layout.track_id, stop)
}

/// Remove only a canonical declaration from an owned copy, without treating it as coded evidence.
pub(super) fn strip_claim(
    path: &Path,
    claim: AudioClaim,
    stop: &Stop,
) -> Result<(), ConversionError> {
    check_stop(stop)?;
    let mut source = Source::open(path, true, Some(stop))?;
    let boxes = mp4::parse_boxes(&source.movie).map_err(|_| BAD)?;
    let movie = only_movie(&boxes)?;
    let layout = Layout::read(movie, &source.movie)?;
    source.validate_fragments(layout.track_id, Some(stop))?;
    let edits = edit_box(layout.track)?.ok_or(BAD)?;
    let edit = canonical_edit(edits, &source.movie)?;
    if declaration(edit, &source.movie, layout)? != claim {
        return Err(BAD);
    }
    let replacement = rewrite_movie(&source.movie, movie, None, None)?;
    rewrite(path, source, &replacement, layout.track_id, stop)
}

fn one(node: &Mp4Box, kind: [u8; 4]) -> Result<&Mp4Box, ConversionError> {
    let mut matching = node.children.iter().filter(|child| child.kind() == kind);
    let child = matching.next().ok_or(BAD)?;
    if matching.next().is_some() {
        return Err(BAD);
    }
    Ok(child)
}
fn only_movie(boxes: &[Mp4Box]) -> Result<&Mp4Box, ConversionError> {
    match boxes {
        [movie] if movie.kind() == *b"moov" => Ok(movie),
        _ => Err(BAD),
    }
}
fn integer(bytes: &[u8], at: usize) -> Result<u32, ConversionError> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(BAD)?
            .try_into()
            .map_err(|_| BAD)?,
    ))
}
fn wide(bytes: &[u8], at: usize) -> Result<u64, ConversionError> {
    Ok(u64::from_be_bytes(
        bytes
            .get(at..at + 8)
            .ok_or(BAD)?
            .try_into()
            .map_err(|_| BAD)?,
    ))
}
fn signed(bytes: &[u8], at: usize) -> Result<i64, ConversionError> {
    Ok(i64::from_be_bytes(
        bytes
            .get(at..at + 8)
            .ok_or(BAD)?
            .try_into()
            .map_err(|_| BAD)?,
    ))
}
fn edit_box(track: &Mp4Box) -> Result<Option<&Mp4Box>, ConversionError> {
    let mut edits = track
        .children
        .iter()
        .filter(|child| child.kind() == *b"edts");
    let edit = edits.next();
    if edits.next().is_some() {
        return Err(BAD);
    }
    Ok(edit)
}
fn canonical_edit<'a>(edits: &'a Mp4Box, bytes: &[u8]) -> Result<&'a Mp4Box, ConversionError> {
    let edit = one(edits, *b"elst")?;
    if edits.children.len() != 1
        || edit.header.payload(bytes).get(..8) != Some(&[1, 0, 0, 0, 0, 0, 0, 2])
    {
        return Err(BAD);
    }
    Ok(edit)
}

/// Recognize only two edits, while leaving validated ordinary single edits to the old path.
fn special_edit<'a>(
    movie: &'a Mp4Box,
    bytes: &[u8],
) -> Result<Option<&'a Mp4Box>, ConversionError> {
    let mut found = None;
    let mut edits_seen = 0;
    for track in movie
        .children
        .iter()
        .filter(|child| child.kind() == *b"trak")
    {
        let Some(edits) = edit_box(track)? else {
            continue;
        };
        edits_seen += 1;
        let edit = one(edits, *b"elst")?;
        let payload = edit.header.payload(bytes);
        if edits.children.len() != 1 || payload.get(1..4) != Some(&[0, 0, 0]) {
            return Err(BAD);
        }
        match integer(payload, 4)? {
            1 => {
                let width = match payload.first() {
                    Some(0) => 4,
                    Some(1) => 8,
                    _ => return Err(BAD),
                };
                if payload.len() != 8 + width * 2 + 4
                    || payload.get(8 + width * 2..) != Some(&[0, 1, 0, 0])
                    || (width == 4 && integer(payload, 12)? & 0x8000_0000 != 0)
                    || (width == 8 && signed(payload, 16)? < 0)
                {
                    return Err(BAD);
                }
            }
            2 => {
                if found.replace(canonical_edit(edits, bytes)?).is_some() {
                    return Err(BAD);
                }
            }
            _ => return Err(BAD),
        }
    }
    let all = mp4::walk(std::slice::from_ref(movie));
    if all.iter().filter(|node| node.kind() == *b"edts").count() != edits_seen
        || all.iter().filter(|node| node.kind() == *b"elst").count() != edits_seen
        || (found.is_some() && edits_seen != 1)
    {
        return Err(BAD);
    }
    Ok(found)
}

#[derive(Clone, Copy)]
struct Layout<'a> {
    track: &'a Mp4Box,
    track_id: u32,
    movie_scale: u32,
    sample_rate: u32,
    fragment_duration: bool,
}
impl<'a> Layout<'a> {
    fn read(movie: &'a Mp4Box, bytes: &[u8]) -> Result<Self, ConversionError> {
        let track = one(movie, *b"trak")?;
        let movie_header = one(movie, *b"mvhd")?;
        let media = one(track, *b"mdia")?;
        let media_header = one(media, *b"mdhd")?;
        let track_header = one(track, *b"tkhd")?;
        let movie_payload = movie_header.header.payload(bytes);
        let media_payload = media_header.header.payload(bytes);
        let track_payload = track_header.header.payload(bytes);
        let movie_at = header_fields(movie_payload, false)?;
        let media_at = header_fields(media_payload, false)?;
        let track_at = header_fields(track_payload, true)?;
        if movie_payload.len() != if movie_payload[0] == 0 { 100 } else { 112 }
            || media_payload.len() != if media_payload[0] == 0 { 24 } else { 36 }
            || movie.children.iter().any(|child| {
                !matches!(
                    &child.kind(),
                    b"mvhd" | b"trak" | b"mvex" | b"pssh" | b"meta"
                )
            })
        {
            return Err(BAD);
        }
        let movie_scale = integer(movie_payload, movie_at)?;
        if movie_scale == 0
            || integer(media_payload, media_at)? != 90_000
            || integer(
                movie_payload,
                movie_at + if movie_payload[0] == 0 { 8 } else { 12 },
            )? != 0x0001_0000
        {
            return Err(BAD);
        }
        let track_id = integer(track_payload, track_at)?;
        if track_id == 0 {
            return Err(BAD);
        }
        let handler = one(media, *b"hdlr")?.header.payload(bytes);
        if handler.get(..4) != Some(&[0, 0, 0, 0]) || handler.get(8..12) != Some(b"soun") {
            return Err(BAD);
        }
        let table = one(one(media, *b"minf")?, *b"stbl")?;
        let description = one(table, *b"stsd")?;
        if description.header.payload(bytes).get(..8) != Some(&[0, 0, 0, 0, 0, 0, 0, 1]) {
            return Err(BAD);
        }
        let [entry] = description.children.as_slice() else {
            return Err(BAD);
        };
        let entry_payload = entry.header.payload(bytes);
        if !matches!(&entry.kind(), b"mp4a" | b"enca") || entry_payload.get(8..16) != Some(&[0; 8])
        {
            return Err(BAD);
        }
        let rate = integer(entry_payload, 24)?;
        if rate & 0xffff != 0 || !matches!(rate >> 16, 44_100 | 48_000) {
            return Err(BAD);
        }
        let extends = one(movie, *b"mvex")?;
        let defaults = one(extends, *b"trex")?;
        let defaults_payload = defaults.header.payload(bytes);
        if defaults_payload.len() != 24
            || integer(defaults_payload, 0)? != 0
            || integer(defaults_payload, 4)? != track_id
            || integer(defaults_payload, 8)? != 1
        {
            return Err(BAD);
        }
        let fragment_duration = optional_fragment_duration(extends, bytes)?;
        validate_metadata_tree(movie, track, table, bytes)?;
        Ok(Self {
            track,
            track_id,
            movie_scale,
            sample_rate: rate >> 16,
            fragment_duration,
        })
    }
}

fn optional_fragment_duration(extends: &Mp4Box, bytes: &[u8]) -> Result<bool, ConversionError> {
    if extends
        .children
        .iter()
        .any(|child| !matches!(&child.kind(), b"trex" | b"mehd"))
    {
        return Err(BAD);
    }
    let mut durations = extends
        .children
        .iter()
        .filter(|child| child.kind() == *b"mehd");
    let Some(duration) = durations.next() else {
        return Ok(false);
    };
    let payload = duration.header.payload(bytes);
    if durations.next().is_some()
        || !matches!(
            (integer(payload, 0)?, payload.len()),
            (0, 8) | (0x0100_0000, 12)
        )
    {
        return Err(BAD);
    }
    Ok(true)
}

pub(in crate::conversion) fn metadata_count(
    movie: &Mp4Box,
    bytes: &[u8],
) -> Result<usize, ConversionError> {
    let mut objects = movie
        .children
        .iter()
        .filter(|child| child.kind() == *b"meta");
    let Some(object) = objects.next() else {
        return Ok(0);
    };
    if objects.next().is_some()
        || object.header.payload(bytes).get(..4) != Some(&[0; 4])
        || object.children.len() != 2
    {
        return Err(BAD);
    }
    let handler = one(object, *b"hdlr")?.header.payload(bytes);
    if handler.len() != 24
        || handler.get(..8) != Some(&[0; 8])
        || handler.get(8..12) != Some(b"ID32")
        || handler.get(12..) != Some(&[0; 12])
    {
        return Err(BAD);
    }
    let id3 = one(object, *b"ID32")?.header.payload(bytes);
    // ID3 is preserved as opaque, non-timed metadata. Its full-box flags and
    // language prefix are bounded here; no ID3 address is fetched or logged.
    if id3.len() < 6 || id3.get(..4) != Some(&[0; 4]) {
        return Err(BAD);
    }
    Ok(1)
}

fn validate_metadata_tree(
    movie: &Mp4Box,
    track: &Mp4Box,
    table: &Mp4Box,
    bytes: &[u8],
) -> Result<(), ConversionError> {
    let metadata = metadata_count(movie, bytes)?;
    let all = mp4::walk(std::slice::from_ref(movie));
    for kind in [
        *b"trak", *b"mvhd", *b"tkhd", *b"mdhd", *b"mdia", *b"minf", *b"stbl", *b"stsd", *b"mvex",
        *b"trex",
    ] {
        if all.iter().filter(|node| node.kind() == kind).count() != 1 {
            return Err(BAD);
        }
    }
    if all.iter().filter(|node| node.kind() == *b"hdlr").count() != 1 + metadata
        || all.iter().filter(|node| node.kind() == *b"meta").count() != metadata
    {
        return Err(BAD);
    }
    let edit_count = usize::from(edit_box(track)?.is_some());
    if all.iter().filter(|node| node.kind() == *b"edts").count() != edit_count
        || all.iter().filter(|node| node.kind() == *b"elst").count() != edit_count
    {
        return Err(BAD);
    }
    for kind in [*b"stts", *b"stsc", *b"stsz"] {
        one(table, kind)?;
        if all.iter().filter(|node| node.kind() == kind).count() != 1 {
            return Err(BAD);
        }
    }
    if all
        .iter()
        .filter(|node| matches!(&node.kind(), b"stco" | b"co64"))
        .count()
        != 1
    {
        return Err(BAD);
    }
    for node in all {
        let payload = node.header.payload(bytes);
        if matches!(&node.kind(), b"stco" | b"co64" | b"stts" | b"stsc") && payload != [0; 8] {
            return Err(BAD);
        }
        if node.kind() == *b"stsz" && payload != [0; 12] {
            return Err(BAD);
        }
    }
    Ok(())
}

/// Return the timescale/track-ID offset and require zero duration in the actual header.
fn header_fields(payload: &[u8], track: bool) -> Result<usize, ConversionError> {
    let (field, duration, width, length) = match (payload.first(), track) {
        (Some(0), false) => (12, 16, 4, None),
        (Some(1), false) => (20, 24, 8, None),
        (Some(0), true) => (12, 20, 4, Some(84)),
        (Some(1), true) => (20, 28, 8, Some(96)),
        _ => return Err(BAD),
    };
    let flags = integer(payload, 0)? & 0x00ff_ffff;
    if (track && !matches!(flags, 3 | 7))
        || (!track && flags != 0)
        || payload
            .get(duration..duration + width)
            .is_none_or(|bytes| bytes.iter().any(|byte| *byte != 0))
        || length.is_some_and(|length| payload.len() != length)
    {
        return Err(BAD);
    }
    Ok(field)
}

fn declaration(
    edit: &Mp4Box,
    bytes: &[u8],
    layout: Layout<'_>,
) -> Result<AudioClaim, ConversionError> {
    let payload = edit.header.payload(bytes);
    let expected_scale = match layout.sample_rate {
        44_100 => 4_410_000,
        48_000 => 720_000,
        _ => return Err(BAD),
    };
    if payload.len() != 48
        || payload.get(..8) != Some(&[1, 0, 0, 0, 0, 0, 0, 2])
        || signed(payload, 16)? != -1
        || payload.get(24..28) != Some(&[0, 1, 0, 0])
        || payload.get(44..48) != Some(&[0, 1, 0, 0])
        || layout.movie_scale != expected_scale
        || layout.fragment_duration
    {
        return Err(BAD);
    }
    let first = signed(payload, 36)?;
    let origin = u64::try_from(first).map_err(|_| BAD)?;
    if wide(payload, 8)? != rescale(origin, 90_000, layout.movie_scale)? {
        return Err(BAD);
    }
    let samples = rescale(wide(payload, 28)?, layout.movie_scale, layout.sample_rate)?;
    if samples == 0 {
        return Err(BAD);
    }
    Ok(AudioClaim {
        sample_rate: layout.sample_rate,
        raw_first_pts: first,
        presented_samples: samples,
    })
}
fn rescale(value: u64, from: u32, to: u32) -> Result<u64, ConversionError> {
    let value = u128::from(value).checked_mul(u128::from(to)).ok_or(BAD)?;
    if from == 0 || to == 0 || !value.is_multiple_of(u128::from(from)) {
        return Err(BAD);
    }
    u64::try_from(value / u128::from(from)).map_err(|_| BAD)
}
fn boxed(kind: [u8; 4], payload: &[u8]) -> Result<Vec<u8>, ConversionError> {
    let limit = usize::try_from(METADATA_LIMIT).map_err(|_| BAD)?;
    let length = payload
        .len()
        .checked_add(8)
        .filter(|length| *length <= limit)
        .ok_or(BAD)?;
    let mut bytes = Vec::with_capacity(length);
    bytes.extend(u32::try_from(length).map_err(|_| BAD)?.to_be_bytes());
    bytes.extend(kind);
    bytes.extend(payload);
    Ok(bytes)
}
fn encoded_edit(presentation: AudioPresentation, scale: u32) -> Result<Vec<u8>, ConversionError> {
    let mut payload = vec![1, 0, 0, 0];
    payload.extend(2_u32.to_be_bytes());
    payload.extend(
        rescale(
            u64::try_from(presentation.evidence.raw_first_pts).map_err(|_| BAD)?,
            90_000,
            scale,
        )?
        .to_be_bytes(),
    );
    payload.extend((-1_i64).to_be_bytes());
    payload.extend([0, 1, 0, 0]);
    payload.extend(
        rescale(
            presentation.presented_samples,
            presentation.evidence.sample_rate,
            scale,
        )?
        .to_be_bytes(),
    );
    payload.extend(presentation.evidence.raw_first_pts.to_be_bytes());
    payload.extend([0, 1, 0, 0]);
    boxed(*b"edts", &boxed(*b"elst", &payload)?)
}
fn rewrite_movie(
    bytes: &[u8],
    node: &Mp4Box,
    edits: Option<&[u8]>,
    movie_scale: Option<u32>,
) -> Result<Vec<u8>, ConversionError> {
    if node.kind() == *b"mvhd" {
        let mut payload = node.header.payload(bytes).to_vec();
        if let Some(scale) = movie_scale {
            let at = header_fields(&payload, false)?;
            payload[at..at + 4].copy_from_slice(&scale.to_be_bytes());
        }
        return boxed(node.kind(), &payload);
    }
    if !matches!(&node.kind(), b"moov" | b"trak" | b"mvex") {
        return Ok(node.header.bytes(bytes).to_vec());
    }
    let mut payload = Vec::new();
    for child in &node.children {
        if child.kind() != *b"edts" && child.kind() != *b"mehd" {
            payload.extend(rewrite_movie(bytes, child, edits, movie_scale)?);
        }
    }
    if node.kind() == *b"trak"
        && let Some(edits) = edits
    {
        payload.extend(edits);
    }
    boxed(node.kind(), &payload)
}

struct Span {
    kind: [u8; 4],
    offset: u64,
    size: u64,
    header: Vec<u8>,
}
struct Source {
    file: File,
    identity: FileIdentity,
    length: u64,
    modified: std::time::SystemTime,
    movie: Vec<u8>,
    boxes: Vec<Span>,
}
impl Source {
    fn open(path: &Path, private: bool, stop: Option<&Stop>) -> Result<Self, ConversionError> {
        let mut file = open(path, private)?;
        let identity = FileIdentity::read(&file).map_err(|_| BAD)?;
        let metadata = file.metadata().map_err(|_| BAD)?;
        let length = metadata.len();
        let modified = metadata.modified().map_err(|_| BAD)?;
        if length == 0 || length > crate::conversion::MAX_MEDIA_BYTES {
            return Err(BAD);
        }
        let mut boxes = Vec::new();
        let mut movie = None;
        let mut offset = 0_u64;
        while offset < length {
            if let Some(stop) = stop {
                check_stop(stop)?;
            }
            if boxes.len() == BOX_LIMIT {
                return Err(BAD);
            }
            file.seek(SeekFrom::Start(offset)).map_err(|_| BAD)?;
            let mut header = vec![0; 8];
            file.read_exact(&mut header).map_err(|_| BAD)?;
            let small = integer(&header, 0)?;
            let size = match small {
                0 => length.checked_sub(offset).ok_or(BAD)?,
                1 => {
                    header.resize(16, 0);
                    file.read_exact(&mut header[8..]).map_err(|_| BAD)?;
                    wide(&header, 8)?
                }
                value => u64::from(value),
            };
            if size < u64::try_from(header.len()).map_err(|_| BAD)? || size > length - offset {
                return Err(BAD);
            }
            let kind = header[4..8].try_into().map_err(|_| BAD)?;
            if kind == *b"moov" {
                if movie.is_some() || size > METADATA_LIMIT {
                    return Err(BAD);
                }
                let mut bytes = header.clone();
                bytes.resize(usize::try_from(size).map_err(|_| BAD)?, 0);
                file.read_exact(&mut bytes[header.len()..])
                    .map_err(|_| BAD)?;
                movie = Some(bytes);
            }
            boxes.push(Span {
                kind,
                offset,
                size,
                header,
            });
            offset = offset.checked_add(size).ok_or(BAD)?;
        }
        Ok(Self {
            file,
            identity,
            length,
            modified,
            movie: movie.ok_or(BAD)?,
            boxes,
        })
    }
    fn check_identity(&self, path: &Path, private: bool) -> Result<(), ConversionError> {
        let current = open(path, private).map_err(|_| CHANGED)?;
        if FileIdentity::read(&self.file).map_err(|_| CHANGED)? != self.identity
            || FileIdentity::read(&current).map_err(|_| CHANGED)? != self.identity
            || self.file.metadata().map_err(|_| CHANGED)?.len() != self.length
            || current.metadata().map_err(|_| CHANGED)?.len() != self.length
            || self
                .file
                .metadata()
                .and_then(|meta| meta.modified())
                .map_err(|_| CHANGED)?
                != self.modified
            || current
                .metadata()
                .and_then(|meta| meta.modified())
                .map_err(|_| CHANGED)?
                != self.modified
        {
            return Err(CHANGED);
        }
        Ok(())
    }
    fn validate_fragments(
        &mut self,
        track_id: u32,
        stop: Option<&Stop>,
    ) -> Result<(), ConversionError> {
        let mut movie_seen = false;
        let mut pending = false;
        let mut file_types = 0;
        for span in &self.boxes {
            if let Some(stop) = stop {
                check_stop(stop)?;
            }
            match &span.kind {
                b"ftyp" if !movie_seen && !pending => {
                    file_types += 1;
                    if file_types != 1 || span.offset != 0 || span.size > 1024 * 1024 {
                        return Err(BAD);
                    }
                }
                b"moov" if !movie_seen && !pending => movie_seen = true,
                b"moof" if movie_seen && !pending => {
                    let bytes = read_span(&mut self.file, span, METADATA_LIMIT)?;
                    validate_fragment(&bytes, track_id)?;
                    pending = true;
                }
                b"mdat" if pending => pending = false,
                b"styp" | b"sidx" | b"mfra" | b"ssix" | b"free" | b"skip" if !pending => {}
                _ => return Err(BAD),
            }
        }
        if !movie_seen || file_types != 1 || pending {
            return Err(BAD);
        }
        Ok(())
    }
}
fn open(path: &Path, private: bool) -> Result<File, ConversionError> {
    if private {
        crate::native::open_private_read(path).map_err(|_| BAD)
    } else {
        files::regular_input(path)
    }
}
fn read_span(file: &mut File, span: &Span, limit: u64) -> Result<Vec<u8>, ConversionError> {
    if span.size > limit {
        return Err(BAD);
    }
    file.seek(SeekFrom::Start(span.offset)).map_err(|_| BAD)?;
    let mut bytes = vec![0; usize::try_from(span.size).map_err(|_| BAD)?];
    file.read_exact(&mut bytes).map_err(|_| BAD)?;
    if bytes.get(..span.header.len()) != Some(span.header.as_slice()) {
        return Err(CHANGED);
    }
    Ok(bytes)
}
pub(super) fn validate_fragment(bytes: &[u8], track_id: u32) -> Result<(), ConversionError> {
    let boxes = mp4::parse_boxes(bytes).map_err(|_| BAD)?;
    let [fragment] = boxes.as_slice() else {
        return Err(BAD);
    };
    if fragment.kind() != *b"moof" || fragment.children.len() != 2 {
        return Err(BAD);
    }
    let header = one(fragment, *b"mfhd")?.header.payload(bytes);
    if header.len() != 8 || integer(header, 0)? != 0 {
        return Err(BAD);
    }
    let track = one(fragment, *b"traf")?;
    for child in &track.children {
        if !matches!(
            &child.kind(),
            b"tfhd" | b"tfdt" | b"trun" | b"senc" | b"saiz" | b"saio" | b"sgpd" | b"sbgp"
        ) {
            return Err(BAD);
        }
    }
    let header = one(track, *b"tfhd")?.header.payload(bytes);
    let flags = integer(header, 0)?;
    let extras = [2, 8, 16, 32]
        .into_iter()
        .filter(|flag| flags & flag != 0)
        .count();
    if flags & !0x0002_003a != 0
        || flags & 0x0002_0000 == 0
        || header.len() != 8 + extras * 4
        || integer(header, 4)? != track_id
        || (flags & 2 != 0 && integer(header, 8)? != 1)
    {
        return Err(BAD);
    }
    let decode = one(track, *b"tfdt")?.header.payload(bytes);
    if !matches!(
        (integer(decode, 0)?, decode.len()),
        (0, 8) | (0x0100_0000, 12)
    ) {
        return Err(BAD);
    }
    let run = one(track, *b"trun")?.header.payload(bytes);
    let flags = integer(run, 0)?;
    let count = usize::try_from(integer(run, 4)?).map_err(|_| BAD)?;
    let per_sample = [0x100, 0x200, 0x400, 0x800]
        .into_iter()
        .filter(|flag| flags & flag != 0)
        .count();
    let expected = count
        .checked_mul(per_sample)
        .and_then(|n| n.checked_mul(4))
        .and_then(|n| n.checked_add(12 + usize::from(flags & 4 != 0) * 4))
        .ok_or(BAD)?;
    if flags & !0x0100_0f05 != 0
        || flags & 1 == 0
        || count == 0
        || count > 1_000_000
        || run.len() != expected
        || integer(run, 8)? != u32::try_from(bytes.len() + 8).map_err(|_| BAD)?
    {
        return Err(BAD);
    }
    Ok(())
}

fn check_stop(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}
fn transfer(
    input: &mut File,
    mut output: Option<&mut File>,
    mut count: u64,
    digest: &mut Sha256,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let mut bytes = Zeroizing::new(vec![0; 64 * 1024]);
    while count != 0 {
        check_stop(stop)?;
        let length = usize::try_from(count.min(u64::try_from(bytes.len()).map_err(|_| BAD)?))
            .map_err(|_| BAD)?;
        input.read_exact(&mut bytes[..length]).map_err(|_| BAD)?;
        digest.update(&bytes[..length]);
        if let Some(output) = &mut output {
            output.write_all(&bytes[..length]).map_err(|_| BAD)?;
        }
        count -= u64::try_from(length).map_err(|_| BAD)?;
    }
    Ok(())
}
fn verify_source(
    path: &Path,
    source: &Source,
    expected: [u8; 32],
    stop: &Stop,
) -> Result<(), ConversionError> {
    source.check_identity(path, true)?;
    let mut current = open(path, true).map_err(|_| CHANGED)?;
    if FileIdentity::read(&current).map_err(|_| CHANGED)? != source.identity {
        return Err(CHANGED);
    }
    let mut digest = Sha256::new();
    transfer(&mut current, None, source.length, &mut digest, stop)?;
    let mut last = [0];
    if current.read(&mut last).map_err(|_| CHANGED)? != 0
        || <[u8; 32]>::from(digest.finalize()) != expected
    {
        return Err(CHANGED);
    }
    source.check_identity(path, true)
}
fn rewrite(
    path: &Path,
    mut source: Source,
    movie: &[u8],
    track_id: u32,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let stage = files::UnpublishedStage::new(path.parent())?;
    let temporary = stage.path().join("audio-presentation.mp4");
    let mut output = stage.create_file(&temporary)?;
    let created = FileIdentity::read(&output).map_err(|_| BAD)?;
    let mut digest = Sha256::new();
    for span in &source.boxes {
        check_stop(stop)?;
        if matches!(&span.kind, b"moov" | b"ftyp" | b"moof") {
            let limit = if span.kind == *b"ftyp" {
                1024 * 1024
            } else {
                METADATA_LIMIT
            };
            let bytes = Zeroizing::new(read_span(&mut source.file, span, limit)?);
            digest.update(&bytes);
            match &span.kind {
                b"moov" => {
                    if bytes.as_slice() != source.movie {
                        return Err(CHANGED);
                    }
                    output.write_all(movie).map_err(|_| BAD)?;
                }
                b"ftyp" => output
                    .write_all(&finite::generic_ftyp(&bytes)?)
                    .map_err(|_| BAD)?,
                _ => {
                    validate_fragment(&bytes, track_id)?;
                    output.write_all(&bytes).map_err(|_| BAD)?;
                }
            }
        } else {
            source
                .file
                .seek(SeekFrom::Start(span.offset))
                .map_err(|_| BAD)?;
            transfer(
                &mut source.file,
                (!matches!(&span.kind, b"sidx" | b"mfra" | b"ssix")).then_some(&mut output),
                span.size,
                &mut digest,
                stop,
            )?;
        }
    }
    output.sync_all().map_err(|_| BAD)?;
    verify_source(path, &source, digest.finalize().into(), stop)?;
    let bound = stage.created_file_source(&temporary, &output)?;
    drop(output);
    drop(bound.reader()?);
    if FileIdentity::read(&files::regular_input(&temporary)?).map_err(|_| BAD)? != created {
        return Err(CHANGED);
    }
    check_stop(stop)?;
    drop(source);
    // Both entries are exclusively owned private files. Close read handles for
    // Windows, then replace atomically; an interrupted copy never changes input.
    std::fs::rename(&temporary, path).map_err(|_| BAD)?;
    if FileIdentity::read(&files::regular_input(path)?).map_err(|_| BAD)? != created {
        return Err(CHANGED);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full(kind: [u8; 4], flags: u32, payload: &[u8]) -> Vec<u8> {
        boxed(kind, &[flags.to_be_bytes().as_slice(), payload].concat()).unwrap()
    }
    fn initialization(rate: u32) -> Vec<u8> {
        let mut movie = vec![0; 100];
        movie[12..16].copy_from_slice(&1000_u32.to_be_bytes());
        movie[20..24].copy_from_slice(&0x0001_0000_u32.to_be_bytes());
        movie[24..26].copy_from_slice(&0x0100_u16.to_be_bytes());
        let mut track = vec![0; 84];
        track[..4].copy_from_slice(&7_u32.to_be_bytes());
        track[12..16].copy_from_slice(&1_u32.to_be_bytes());
        let mut media = vec![0; 24];
        media[12..16].copy_from_slice(&90_000_u32.to_be_bytes());
        let mut entry = vec![0; 28];
        entry[6..8].copy_from_slice(&1_u16.to_be_bytes());
        entry[16..18].copy_from_slice(&2_u16.to_be_bytes());
        entry[18..20].copy_from_slice(&16_u16.to_be_bytes());
        entry[24..28].copy_from_slice(&(rate << 16).to_be_bytes());
        let table = boxed(
            *b"stbl",
            &[
                full(
                    *b"stsd",
                    0,
                    &[
                        1_u32.to_be_bytes().as_slice(),
                        &boxed(*b"mp4a", &entry).unwrap(),
                    ]
                    .concat(),
                ),
                full(*b"stts", 0, &[0; 4]),
                full(*b"stsc", 0, &[0; 4]),
                full(*b"stsz", 0, &[0; 8]),
                full(*b"stco", 0, &[0; 4]),
            ]
            .concat(),
        )
        .unwrap();
        let media = boxed(
            *b"mdia",
            &[
                boxed(*b"mdhd", &media).unwrap(),
                full(
                    *b"hdlr",
                    0,
                    &[&[0; 4], b"soun".as_slice(), &[0; 12], b"Sound\0".as_slice()].concat(),
                ),
                boxed(*b"minf", &table).unwrap(),
            ]
            .concat(),
        )
        .unwrap();
        let track = boxed(
            *b"trak",
            &[boxed(*b"tkhd", &track).unwrap(), media].concat(),
        )
        .unwrap();
        let defaults = [1_u32, 1, 0, 0, 0]
            .into_iter()
            .flat_map(u32::to_be_bytes)
            .collect::<Vec<_>>();
        let movie = boxed(
            *b"moov",
            &[
                boxed(*b"mvhd", &movie).unwrap(),
                track,
                boxed(*b"mvex", &full(*b"trex", 0, &defaults)).unwrap(),
            ]
            .concat(),
        )
        .unwrap();
        [
            boxed(*b"ftyp", b"cmfc\0\0\0\0isomcmfccmfsdash").unwrap(),
            movie,
        ]
        .concat()
    }
    fn presentation(rate: u32, first: i64) -> AudioPresentation {
        AudioPresentation::new(
            AudioEvidence {
                sample_rate: rate,
                coded_frames: 4,
                raw_first_pts: first,
                raw_coded_end: first + 4096 * 90_000 / i64::from(rate),
            },
            4096 - 17,
        )
        .unwrap()
    }
    fn fragment(first: i64, absolute: bool) -> Vec<u8> {
        let mut run = 4_u32.to_be_bytes().to_vec();
        run.extend(128_u32.to_be_bytes());
        for _ in 0..4 {
            run.extend(1920_u32.to_be_bytes());
            run.extend(1_u32.to_be_bytes());
        }
        let mut header = 1_u32.to_be_bytes().to_vec();
        if absolute {
            header.extend(128_u64.to_be_bytes());
        }
        let track = boxed(
            *b"traf",
            &[
                full(
                    *b"tfhd",
                    if absolute { 0x0002_0001 } else { 0x0002_0000 },
                    &header,
                ),
                full(
                    *b"tfdt",
                    0x0100_0000,
                    &u64::try_from(first).unwrap().to_be_bytes(),
                ),
                full(*b"trun", 0x301, &run),
            ]
            .concat(),
        )
        .unwrap();
        [
            boxed(
                *b"moof",
                &[full(*b"mfhd", 0, &1_u32.to_be_bytes()), track].concat(),
            )
            .unwrap(),
            boxed(*b"mdat", &[1, 2, 3, 4]).unwrap(),
        ]
        .concat()
    }
    fn write_private(path: &Path, bytes: &[u8]) {
        let mut file = files::create_new(path).unwrap();
        file.write_all(bytes).unwrap();
        file.sync_all().unwrap();
    }
    fn payload_offset(bytes: &[u8], kind: [u8; 4]) -> usize {
        let boxes = mp4::parse_boxes(bytes).unwrap();
        let all = mp4::walk(&boxes);
        let node = all.iter().find(|node| node.kind() == kind).unwrap();
        node.header.offset + node.header.header_size
    }
    fn media_bytes(bytes: &[u8]) -> Vec<u8> {
        mp4::parse_boxes(bytes)
            .unwrap()
            .iter()
            .filter(|node| matches!(&node.kind(), b"moof" | b"mdat"))
            .flat_map(|node| node.header.bytes(bytes).to_vec())
            .collect()
    }

    fn shaka_metadata(mut bytes: Vec<u8>, bad_meta: bool, bad_duration: bool) -> Vec<u8> {
        let boxes = mp4::parse_boxes(&bytes).unwrap();
        let movie = boxes.iter().find(|node| node.kind() == *b"moov").unwrap();
        let mut handler = vec![0; 24];
        handler[8..12].copy_from_slice(b"ID32");
        let mut metadata = vec![u8::from(bad_meta), 0, 0, 0];
        metadata.extend(boxed(*b"hdlr", &handler).unwrap());
        metadata.extend(full(*b"ID32", 0, &[0x55, 0xc4]));
        let mut payload = Vec::new();
        for child in &movie.children {
            if child.kind() == *b"mvex" {
                payload.extend(
                    boxed(
                        *b"mvex",
                        &[
                            full(
                                *b"mehd",
                                u32::from(bad_duration),
                                &123_456_u32.to_be_bytes(),
                            ),
                            child.header.payload(&bytes).to_vec(),
                        ]
                        .concat(),
                    )
                    .unwrap(),
                );
            } else {
                payload.extend(child.header.bytes(&bytes));
            }
        }
        payload.extend(boxed(*b"meta", &metadata).unwrap());
        let begin = movie.header.offset;
        bytes.truncate(begin);
        bytes.extend(boxed(*b"moov", &payload).unwrap());
        bytes
    }

    #[test]
    fn restore_read_strip_keep_exact_origins_and_media_and_remove_cmaf_claims() {
        for rate in [44_100, 48_000] {
            for first in [0, 126_000] {
                let owner = files::Stage::new(None).unwrap();
                let path = owner.path().join("audio.mp4");
                let media = fragment(first, false);
                let bytes = [
                    initialization(rate),
                    boxed(*b"sidx", &[]).unwrap(),
                    media.clone(),
                    boxed(*b"mfra", &[]).unwrap(),
                    boxed(*b"ssix", &[]).unwrap(),
                ]
                .concat();
                write_private(&path, &bytes);
                let expected = presentation(rate, first);
                restore(&path, expected, &Stop::new()).unwrap();
                assert_eq!(read(&path, expected.evidence).unwrap(), Some(expected));
                let actual = std::fs::read(&path).unwrap();
                assert_eq!(media_bytes(&actual), media);
                assert!(!actual.windows(3).any(|bytes| bytes == b"cmf"));
                let boxes = mp4::parse_boxes(&actual).unwrap();
                assert!(
                    boxes
                        .iter()
                        .all(|node| !matches!(&node.kind(), b"sidx" | b"mfra" | b"ssix"))
                );
                let edit = payload_offset(&actual, *b"elst");
                assert_eq!(&actual[edit..edit + 8], &[1, 0, 0, 0, 0, 0, 0, 2]);
                assert_eq!(signed(&actual, edit + 16).unwrap(), -1);
                if first == 0 {
                    assert_eq!(wide(&actual, edit + 8).unwrap(), 0);
                }
                restore(&path, expected, &Stop::new()).unwrap();
                assert_eq!(std::fs::read(&path).unwrap(), actual);
                strip_claim(&path, expected.claim(), &Stop::new()).unwrap();
                assert_eq!(read_claim(&path).unwrap(), None);
                assert_eq!(media_bytes(&std::fs::read(&path).unwrap()), media);
            }
        }
    }

    #[test]
    fn ordinary_single_edits_dispatch_before_the_new_profile_checks() {
        let owner = files::Stage::new(None).unwrap();
        let path = owner.path().join("ordinary.mp4");
        let mut edit = 1_u32.to_be_bytes().to_vec();
        edit.extend(10_000_u32.to_be_bytes());
        edit.extend(0_i32.to_be_bytes());
        edit.extend([0, 1, 0, 0]);
        let track = boxed(
            *b"trak",
            &boxed(*b"edts", &full(*b"elst", 0, &edit)).unwrap(),
        )
        .unwrap();
        // The old dispatcher, not this new profile, validates progressive headers.
        write_private(&path, &boxed(*b"moov", &track).unwrap());
        assert_eq!(read_claim(&path).unwrap(), None);
    }

    #[test]
    fn video_and_multiple_tracks_keep_legacy_complex_edit_refusals() {
        let owner = files::Stage::new(None).unwrap();
        let path = owner.path().join("legacy.mp4");
        write_private(&path, &initialization(44_100));
        let expected = presentation(44_100, 126_000);
        restore(&path, expected, &Stop::new()).unwrap();
        let original = std::fs::read(&path).unwrap();
        let mut video = original.clone();
        let handler = payload_offset(&video, *b"hdlr") + 8;
        video[handler..handler + 4].copy_from_slice(b"vide");
        let boxes = mp4::parse_boxes(&original).unwrap();
        let mut multiple = Vec::new();
        for node in &boxes {
            if node.kind() == *b"moov" {
                let mut payload = node.header.payload(&original).to_vec();
                payload.extend(one(node, *b"trak").unwrap().header.bytes(&original));
                multiple.extend(boxed(*b"moov", &payload).unwrap());
            } else {
                multiple.extend(node.header.bytes(&original));
            }
        }
        for candidate in [video, multiple] {
            std::fs::write(&path, &candidate).unwrap();
            assert_eq!(read_claim(&path).unwrap(), None);
            assert_eq!(
                finite::window(&path, 0).unwrap_err().0,
                "presentation-window-not-supported"
            );
            assert!(restore(&path, expected, &Stop::new()).is_err());
            assert!(strip_claim(&path, expected.claim(), &Stop::new()).is_err());
            assert_eq!(std::fs::read(&path).unwrap(), candidate);
        }
    }

    #[test]
    fn known_id3_metadata_is_retained_and_old_fragment_duration_is_removed() {
        for (bad_meta, bad_duration) in [(false, false), (true, false), (false, true)] {
            let owner = files::Stage::new(None).unwrap();
            let path = owner.path().join("audio.mp4");
            let bytes = shaka_metadata(initialization(44_100), bad_meta, bad_duration);
            write_private(&path, &bytes);
            let result = restore(&path, presentation(44_100, 126_000), &Stop::new());
            if bad_meta || bad_duration {
                assert!(result.is_err());
                assert_eq!(std::fs::read(&path).unwrap(), bytes);
            } else {
                result.unwrap();
                let actual = std::fs::read(&path).unwrap();
                let boxes = mp4::parse_boxes(&actual).unwrap();
                let all = mp4::walk(&boxes);
                assert!(!all.iter().any(|node| node.kind() == *b"mehd"));
                let meta = all.iter().find(|node| node.kind() == *b"meta").unwrap();
                let original_boxes = mp4::parse_boxes(&bytes).unwrap();
                let original_all = mp4::walk(&original_boxes);
                let original_meta = original_all
                    .iter()
                    .find(|node| node.kind() == *b"meta")
                    .unwrap();
                assert_eq!(
                    meta.header.bytes(&actual),
                    original_meta.header.bytes(&bytes)
                );
                assert_eq!(
                    read(&path, presentation(44_100, 126_000).evidence).unwrap(),
                    Some(presentation(44_100, 126_000))
                );
            }
        }
    }

    #[test]
    fn malformed_edit_headers_scales_and_ranges_cannot_become_claims() {
        let owner = files::Stage::new(None).unwrap();
        let path = owner.path().join("audio.mp4");
        write_private(&path, &initialization(44_100));
        let expected = presentation(44_100, 126_000);
        restore(&path, expected, &Stop::new()).unwrap();
        let valid = std::fs::read(&path).unwrap();
        for (kind, relative, replacement) in [
            (*b"elst", 1, vec![1]),
            (*b"elst", 4, 3_u32.to_be_bytes().to_vec()),
            (*b"elst", 16, 0_i64.to_be_bytes().to_vec()),
            (*b"elst", 36, 126_001_i64.to_be_bytes().to_vec()),
            (*b"elst", 44, vec![0, 2, 0, 0]),
            (*b"mvhd", 12, 1000_u32.to_be_bytes().to_vec()),
            (*b"mvhd", 16, 1_u32.to_be_bytes().to_vec()),
            (*b"tkhd", 20, 1_u32.to_be_bytes().to_vec()),
            (*b"mdhd", 12, 44_100_u32.to_be_bytes().to_vec()),
            (*b"mp4a", 24, (32_000_u32 << 16).to_be_bytes().to_vec()),
        ] {
            let mut changed = valid.clone();
            let at = payload_offset(&changed, kind) + relative;
            changed[at..at + replacement.len()].copy_from_slice(&replacement);
            std::fs::write(&path, changed).unwrap();
            assert!(
                read_claim(&path).is_err(),
                "{} at {relative}",
                String::from_utf8_lossy(&kind)
            );
        }
        std::fs::write(&path, &valid).unwrap();
        let wrong =
            AudioPresentation::new(expected.evidence, expected.presented_samples - 1).unwrap();
        assert!(strip_claim(&path, wrong.claim(), &Stop::new()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), valid);
        let mut wrong_evidence = expected.evidence;
        wrong_evidence.sample_rate = 48_000;
        assert!(read(&path, wrong_evidence).is_err());
    }

    #[test]
    fn absolute_fragment_offsets_and_cancellation_leave_owned_input_unchanged() {
        let owner = files::Stage::new(None).unwrap();
        let path = owner.path().join("audio.mp4");
        let bytes = [initialization(48_000), fragment(126_000, true)].concat();
        write_private(&path, &bytes);
        assert!(restore(&path, presentation(48_000, 126_000), &Stop::new()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let stop = Stop::new();
        stop.set();
        assert_eq!(
            restore(&path, presentation(48_000, 126_000), &stop).unwrap_err(),
            ConversionError("cancelled")
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[cfg(unix)]
    #[test]
    fn revalidation_refuses_a_replaced_source_without_touching_the_foreign_file() {
        let owner = files::Stage::new(None).unwrap();
        let path = owner.path().join("audio.mp4");
        let bytes = initialization(48_000);
        write_private(&path, &bytes);
        let source = Source::open(&path, true, None).unwrap();
        std::fs::rename(&path, owner.path().join("retained-original.mp4")).unwrap();
        write_private(&path, b"foreign file");
        let digest = Sha256::digest(&bytes).into();
        assert_eq!(
            verify_source(&path, &source, digest, &Stop::new()).unwrap_err(),
            CHANGED
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"foreign file");
    }
}
