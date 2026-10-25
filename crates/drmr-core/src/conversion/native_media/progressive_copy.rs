//! Clear progressive MP4 copies retain the original byte layout and playback metadata.
//!
//! Replacing protection boxes with equal-sized, zero-filled padding keeps both
//! 32-bit and 64-bit chunk offsets valid, including when the movie follows media.
//! Callers admit the finite progressive conversion profile before using this writer.
use super::{
    BAD, CHUNK_BYTES, ConversionError, InputKeys, UNSUPPORTED, catalogue, init, read,
    visit_progressive,
};
use crate::sha256::Sha256;
use crate::{concurrency::Stop, mp4};
use std::{
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    path::Path,
};
use zeroize::Zeroizing;

const IO_ERROR: ConversionError = ConversionError("media-copy-failed");

fn clear_movie(bytes: &mut [u8]) -> Result<(), ConversionError> {
    let boxes = mp4::parse_boxes(bytes).map_err(|_| BAD)?;
    let nodes = mp4::walk(&boxes);
    // Read every original format before clearing the protection containers.
    for node in &nodes {
        if matches!(&node.kind(), b"enca" | b"encv") {
            let sinf = read::one(&node.children, *b"sinf")?;
            let format = read::one(&sinf.children, *b"frma")?.header.payload(bytes);
            let expected = if node.kind() == *b"enca" {
                *b"mp4a"
            } else {
                *b"avc1"
            };
            if format != expected {
                return Err(UNSUPPORTED);
            }
            let at = node.header.offset + 4;
            bytes[at..at + 4].copy_from_slice(&expected);
        }
    }
    for node in nodes {
        if matches!(&node.kind(), b"sinf" | b"senc" | b"saiz" | b"saio") {
            let at = node.header.offset;
            bytes[at + 4..at + 8].copy_from_slice(b"free");
            // Remove key identifiers, IVs and subsample records rather than
            // retaining their bytes inside otherwise ignored padding.
            bytes[at + node.header.header_size..at + node.header.size].fill(0);
        }
    }
    Ok(())
}

/// Stream a qualified progressive source into a private, exclusively created file.
/// A successful return requires complete sample-table validation, all declared
/// protected sample bytes to be decrypted, and an unchanged original.
pub(in crate::conversion) fn copy_progressive(
    source: &Path,
    destination: &Path,
    keys: &InputKeys,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let mut reader = read::Reader::open(source, stop, true)?;
    if reader.length > super::super::MAX_MEDIA_BYTES {
        return Err(ConversionError("media-size-limit"));
    }
    let layout = reader.layout()?;
    let mut init = init::parse(&layout)?;
    if init.movie.fragmented {
        return Err(UNSUPPORTED);
    }
    catalogue(&mut reader, &layout, &mut init)?;
    if init
        .movie
        .tracks
        .iter()
        .any(|track| track.sample_count == 0)
    {
        return Err(BAD);
    }
    for track in &init.tracks {
        if let Some(protection) = &track.encryption
            && !keys.keys.iter().any(|key| key.id == protection.kid)
        {
            return Err(ConversionError("input-key-required"));
        }
    }
    let mut movie = layout.movie.clone();
    clear_movie(&mut movie)?;
    let mut output = super::super::files::create_new(destination)?;
    reader.file.seek(SeekFrom::Start(0)).map_err(|_| IO_ERROR)?;
    let mut buffer = Zeroizing::new(vec![0; CHUNK_BYTES]);
    let mut digest = Sha256::new();
    let mut remaining = reader.length;
    while remaining != 0 {
        read::check_stop(stop)?;
        let size = usize::try_from(remaining.min(CHUNK_BYTES as u64)).map_err(|_| BAD)?;
        reader
            .file
            .read_exact(&mut buffer[..size])
            .map_err(|_| IO_ERROR)?;
        digest.update(&buffer[..size]);
        output.write_all(&buffer[..size]).map_err(|_| IO_ERROR)?;
        remaining -= size as u64;
    }
    if Some(digest.finalize()) != reader.expected_fingerprint() {
        return Err(ConversionError("native-source-changed"));
    }
    for track in &init.tracks {
        if track.encryption.is_some() {
            visit_progressive(&mut reader, &layout, track, keys, &mut |_, info, bytes| {
                let offset = info
                    .range
                    .start
                    .checked_add(u64::from(info.chunk_offset))
                    .filter(|offset| {
                        offset
                            .checked_add(bytes.len() as u64)
                            .is_some_and(|end| end <= info.range.end)
                    })
                    .ok_or(BAD)?;
                output
                    .seek(SeekFrom::Start(offset))
                    .and_then(|_| output.write_all(bytes))
                    .map_err(|_| IO_ERROR)
            })?;
        }
    }
    output
        .seek(SeekFrom::Start(layout.movie_offset))
        .and_then(|_| output.write_all(&movie))
        .map_err(|_| IO_ERROR)?;
    output.flush().map_err(|_| IO_ERROR)?;
    // Close the writer before any subsequent inspection opens the candidate,
    // including platforms with restrictive file-sharing modes.
    drop(output);
    reader.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests;
