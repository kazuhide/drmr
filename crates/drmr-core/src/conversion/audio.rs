//! Refuse finite audio edits whose end padding cannot yet survive fragmented repackaging.
//! Open-ended priming trims remain supported and are verified against the original media.
use super::ConversionError;
use crate::mp4::{self, Mp4Box};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
const METADATA_LIMIT: u64 = 16 * 1024 * 1024;
const UNSUPPORTED: ConversionError = ConversionError("audio-presentation-edit-not-supported");
struct Movie {
    bytes: Vec<u8>,
}
fn movie(path: &Path) -> Result<Movie, ConversionError> {
    let mut file = File::open(path).map_err(|_| ConversionError("media-metadata-unreadable"))?;
    let length = file
        .metadata()
        .map_err(|_| ConversionError("media-metadata-unreadable"))?
        .len();
    let mut position = 0;
    let mut movie = None;
    for _ in 0..100_000 {
        if position == length {
            return movie.ok_or(ConversionError("mp4-initialization-missing"));
        }
        let mut header = [0; 8];
        file.read_exact(&mut header)
            .map_err(|_| ConversionError("invalid-mp4-box"))?;
        let small = u32::from_be_bytes(header[..4].try_into().unwrap());
        let mut bytes = header.to_vec();
        let size = match small {
            0 => length - position,
            1 => {
                let mut large = [0; 8];
                file.read_exact(&mut large)
                    .map_err(|_| ConversionError("invalid-mp4-box"))?;
                bytes.extend(large);
                u64::from_be_bytes(large)
            }
            n => u64::from(n),
        };
        if size < bytes.len() as u64 || size > length - position {
            return Err(ConversionError("invalid-mp4-box"));
        }
        if &header[4..] == b"emsg" {
            return Err(ConversionError("inband-events-not-supported"));
        }
        if &header[4..] == b"moov" {
            if movie.is_some() {
                return Err(ConversionError("invalid-mp4-metadata"));
            }
            if size > METADATA_LIMIT {
                return Err(ConversionError("mp4-metadata-size-limit"));
            }
            let head = bytes.len();
            bytes.resize(
                usize::try_from(size).map_err(|_| ConversionError("mp4-metadata-size-limit"))?,
                0,
            );
            file.read_exact(&mut bytes[head..])
                .map_err(|_| ConversionError("invalid-mp4-box"))?;
            if small == 0 {
                bytes[..4].copy_from_slice(
                    &u32::try_from(size)
                        .map_err(|_| ConversionError("mp4-metadata-size-limit"))?
                        .to_be_bytes(),
                );
            }
            movie = Some(Movie { bytes });
        }
        position += size;
        file.seek(SeekFrom::Start(position))
            .map_err(|_| ConversionError("media-metadata-unreadable"))?;
    }
    Err(ConversionError("mp4-initialization-missing"))
}
fn child(node: &Mp4Box, kind: [u8; 4]) -> Result<&Mp4Box, ConversionError> {
    let mut found = node.children.iter().filter(|node| node.kind() == kind);
    let value = found.next().ok_or(UNSUPPORTED)?;
    if found.next().is_some() {
        return Err(UNSUPPORTED);
    }
    Ok(value)
}
struct Edit {
    duration: u64,
}
fn edit(node: &Mp4Box, data: &[u8]) -> Result<Option<Edit>, ConversionError> {
    let Some(edts) = node.children.iter().find(|node| node.kind() == *b"edts") else {
        return Ok(None);
    };
    let elst = child(edts, *b"elst")?;
    let payload = elst.header.payload(data);
    let width = match payload.first() {
        Some(0) => 4,
        Some(1) => 8,
        _ => return Err(UNSUPPORTED),
    };
    if payload.get(4..8) != Some(&[0, 0, 0, 1])
        || payload.get(8 + 2 * width..) != Some(&[0, 1, 0, 0])
    {
        return Err(UNSUPPORTED);
    }
    let duration = if width == 4 {
        u64::from(u32::from_be_bytes(payload[8..12].try_into().unwrap()))
    } else {
        u64::from_be_bytes(payload[8..16].try_into().unwrap())
    };
    let media_time = if width == 4 {
        i64::from(i32::from_be_bytes(payload[12..16].try_into().unwrap()))
    } else {
        i64::from_be_bytes(payload[16..24].try_into().unwrap())
    };
    if media_time < 0 {
        return Err(UNSUPPORTED);
    }
    Ok(Some(Edit { duration }))
}
pub(super) fn require_supported_audio(path: &Path, index: usize) -> Result<(), ConversionError> {
    let movie = movie(path)?;
    let boxes = mp4::parse_boxes(&movie.bytes).map_err(|_| UNSUPPORTED)?;
    let moov = boxes
        .iter()
        .find(|node| node.kind() == *b"moov")
        .ok_or(UNSUPPORTED)?;
    let track = moov
        .children
        .iter()
        .filter(|node| node.kind() == *b"trak")
        .nth(index)
        .ok_or(UNSUPPORTED)?;
    let mdia = child(track, *b"mdia")?;
    if child(mdia, *b"hdlr")?
        .header
        .payload(&movie.bytes)
        .get(8..12)
        != Some(b"soun")
    {
        return Ok(());
    }
    if edit(track, &movie.bytes)?.is_some_and(|edit| edit.duration > 0) {
        return Err(ConversionError("finite-audio-tail-not-supported"));
    }
    Ok(())
}
pub(super) fn validate_sample_groups(path: &Path) -> Result<(), ConversionError> {
    let movie = movie(path)?;
    let boxes =
        mp4::parse_boxes(&movie.bytes).map_err(|_| ConversionError("invalid-mp4-metadata"))?;
    for node in mp4::walk(&boxes) {
        if matches!(&node.kind(), b"sgpd" | b"sbgp")
            && !matches!(
                node.header.payload(&movie.bytes).get(4..8),
                Some(b"roll" | b"prol")
            )
        {
            return Err(ConversionError("sample-groups-not-supported"));
        }
    }
    Ok(())
}
