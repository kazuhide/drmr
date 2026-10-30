//! Same-width track references in an already admitted, exclusively owned tool copy.
use super::{BAD, ConversionError, MAX_METADATA_BYTES, Movie, read::check_stop};
use crate::{concurrency::Stop, mp4};
use std::{
    fs::File,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
};

fn header(file: &mut File, at: u64, end: u64) -> Result<(u64, u64, [u8; 4]), ConversionError> {
    if end.checked_sub(at).is_none_or(|n| n < 8) {
        return Err(BAD);
    }
    file.seek(SeekFrom::Start(at)).map_err(|_| BAD)?;
    let mut bytes = [0; 8];
    file.read_exact(&mut bytes).map_err(|_| BAD)?;
    let small = u32::from_be_bytes(bytes[..4].try_into().unwrap());
    let (size, head) = match small {
        0 => (end - at, 8),
        1 => {
            let mut wide = [0; 8];
            file.read_exact(&mut wide).map_err(|_| BAD)?;
            (u64::from_be_bytes(wide), 16)
        }
        n => (u64::from(n), 8),
    };
    if size < head || size > end - at {
        return Err(BAD);
    }
    Ok((size, head, bytes[4..].try_into().unwrap()))
}

fn replacement(ids: &[u32], original: u32) -> Option<u32> {
    ids.iter()
        .position(|id| *id == original)
        .and_then(|i| u32::try_from(i + 1).ok())
}

fn write_id(
    file: &mut File,
    at: u64,
    old: u32,
    new: u32,
    stop: &Stop,
) -> Result<(), ConversionError> {
    check_stop(stop)?;
    file.seek(SeekFrom::Start(at)).map_err(|_| BAD)?;
    let mut current = [0; 4];
    file.read_exact(&mut current).map_err(|_| BAD)?;
    if current != old.to_be_bytes() {
        return Err(ConversionError("native-source-changed"));
    }
    file.seek(SeekFrom::Start(at))
        .and_then(|_| file.write_all(&new.to_be_bytes()))
        .map_err(|_| BAD)
}

fn core(
    file: &mut File,
    at: u64,
    size: u64,
    ids: &[u32],
    stop: &Stop,
) -> Result<(), ConversionError> {
    let length = usize::try_from(size).map_err(|_| BAD)?;
    if length > MAX_METADATA_BYTES {
        return Err(BAD);
    }
    file.seek(SeekFrom::Start(at)).map_err(|_| BAD)?;
    let mut bytes = vec![0; length];
    file.read_exact(&mut bytes).map_err(|_| BAD)?;
    // A size-zero top-level box was already bounded by the admitted file length.
    if bytes[..4] == [0; 4] {
        bytes[..4].copy_from_slice(&u32::try_from(size).map_err(|_| BAD)?.to_be_bytes());
    }
    let nodes = mp4::parse_boxes(&bytes).map_err(|_| BAD)?;
    let root = nodes.first().ok_or(BAD)?;
    for container in &root.children {
        // Descriptive metadata can contain identically named, uninterpreted boxes.
        // Only references at the native parser's actual control locations are mapped.
        let kind = match (&root.kind(), &container.kind()) {
            (b"moov", b"trak") => *b"tkhd",
            (b"moov", b"mvex") => *b"trex",
            (b"moof", b"traf") => *b"tfhd",
            _ => continue,
        };
        for node in container.children.iter().filter(|node| node.kind() == kind) {
            check_stop(stop)?;
            let payload = node.header.payload(&bytes);
            let field = if kind == *b"tkhd" {
                match payload.first() {
                    Some(0) => 12,
                    Some(1) => 20,
                    _ => return Err(BAD),
                }
            } else {
                4
            };
            let old = super::read::integer(payload, field)?;
            let new = replacement(ids, old).ok_or(BAD)?;
            write_id(
                file,
                at + (node.header.offset + node.header.header_size + field) as u64,
                old,
                new,
                stop,
            )?;
        }
    }
    Ok(())
}

fn opaque_reference(
    file: &mut File,
    at: u64,
    size: u64,
    head: u64,
    ids: &[u32],
    stop: &Stop,
) -> Result<(), ConversionError> {
    if size - head < 8 {
        return Ok(());
    }
    file.seek(SeekFrom::Start(at + head)).map_err(|_| BAD)?;
    let mut prefix = [0; 8];
    file.read_exact(&mut prefix).map_err(|_| BAD)?;
    // Native admission does not certify indexes. Only known version prefixes are
    // rebound; counts, offsets, extensions and unmatched references stay byte-exact.
    if prefix[0] <= 1 && prefix[1..4] == [0; 3] {
        let old = u32::from_be_bytes(prefix[4..].try_into().unwrap());
        if let Some(new) = replacement(ids, old) {
            write_id(file, at + head + 4, old, new, stop)?;
        }
    }
    Ok(())
}

pub(in crate::conversion) fn canonicalize_tool_ids(
    file: &mut File,
    movie: &Movie,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let ids: Vec<_> = movie.tracks.iter().map(|track| track.track_id).collect();
    let end = file.metadata().map_err(|_| BAD)?.len();
    let mut at = 0;
    while at < end {
        check_stop(stop)?;
        let (size, head, kind) = header(file, at, end)?;
        match &kind {
            b"moov" | b"moof" => core(file, at, size, &ids, stop)?,
            b"sidx" => opaque_reference(file, at, size, head, &ids, stop)?,
            b"mfra" => {
                let mut child = at + head;
                while child < at + size {
                    check_stop(stop)?;
                    // Malformed or extended opaque index tails were never parsed by
                    // native admission; preserve them instead of adding a new gate.
                    let Ok((length, header_size, kind)) = header(file, child, at + size) else {
                        break;
                    };
                    if kind == *b"tfra" {
                        opaque_reference(file, child, length, header_size, &ids, stop)?;
                    }
                    child += length;
                }
            }
            _ => {}
        }
        at += size;
    }
    file.sync_all().map_err(|_| BAD)
}

#[cfg(test)]
mod tests;
