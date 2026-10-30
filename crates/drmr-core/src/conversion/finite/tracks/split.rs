//! Private track views retain metadata and fragment pairs with one self-contained track ID.
use super::{BAD, ConversionError, File, Path, Read, Stop, Write, audio, boxed, integer, mp4, one};
use std::{
    io::{Seek as _, SeekFrom},
    path::PathBuf,
};

const VIEW_TRACK_ID: u32 = 1;

fn initialization(movie: &audio::Movie, index: usize) -> Result<Vec<u8>, ConversionError> {
    let boxes = mp4::parse_boxes(&movie.bytes).map_err(|_| BAD)?;
    let moov = boxes.first().ok_or(BAD)?;
    let tracks: Vec<_> = moov
        .children
        .iter()
        .filter(|n| n.kind() == *b"trak")
        .collect();
    let selected = tracks.get(index).ok_or(BAD)?;
    let track_header = one(selected, *b"tkhd")?;
    let header = track_header.header.payload(&movie.bytes);
    let at = match header.first() {
        Some(0) => 12,
        Some(1) => 20,
        _ => return Err(BAD),
    };
    let id = integer(header, at)?;
    let mut payload = Vec::new();
    for child in &moov.children {
        if child.kind() == *b"trak" {
            if child.header.offset == selected.header.offset {
                let mut bytes = child.header.bytes(&movie.bytes).to_vec();
                let pos = track_header.header.offset - child.header.offset
                    + track_header.header.header_size
                    + at;
                bytes
                    .get_mut(pos..pos + 4)
                    .ok_or(BAD)?
                    .copy_from_slice(&VIEW_TRACK_ID.to_be_bytes());
                payload.extend(bytes);
            }
        } else if child.kind() == *b"mvex" {
            let mut defaults = Vec::new();
            for entry in &child.children {
                if entry.kind() == *b"trex" {
                    if integer(entry.header.payload(&movie.bytes), 4)? == id {
                        let mut bytes = entry.header.bytes(&movie.bytes).to_vec();
                        let pos = entry.header.header_size + 4;
                        bytes
                            .get_mut(pos..pos + 4)
                            .ok_or(BAD)?
                            .copy_from_slice(&VIEW_TRACK_ID.to_be_bytes());
                        defaults.extend(bytes);
                    }
                } else if entry.kind() != *b"mehd" {
                    return Err(BAD);
                }
            }
            let nodes = mp4::parse_boxes(&defaults).map_err(|_| BAD)?;
            if nodes.len() != 1 {
                return Err(BAD);
            }
            payload.extend(boxed(*b"mvex", &defaults)?);
        } else {
            payload.extend(child.header.bytes(&movie.bytes));
        }
    }
    boxed(*b"moov", &payload)
}

fn copy_parts(
    input: &Path,
    outputs: &mut [File],
    ids: &[u32],
    stop: &Stop,
) -> Result<(), ConversionError> {
    let mut file = super::super::super::files::regular_input(input)?;
    let identity = crate::native::FileIdentity::read(&file).map_err(|_| BAD)?;
    let length = file.metadata().map_err(|_| BAD)?.len();
    let mut at = 0;
    let mut pending = None;
    let mut count = vec![0u32; outputs.len()];
    for _ in 0..100_000 {
        if at == length {
            break;
        }
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        let mut header = [0; 8];
        file.read_exact(&mut header).map_err(|_| BAD)?;
        let size = u64::from(integer(&header, 0)?);
        if size < 8 || size > length - at {
            return Err(BAD);
        }
        match &header[4..] {
            b"moof" => {
                if pending.is_some() || size > 16 * 1024 * 1024 {
                    return Err(BAD);
                }
                let mut bytes = header.to_vec();
                bytes.resize(usize::try_from(size).map_err(|_| BAD)?, 0);
                file.read_exact(&mut bytes[8..]).map_err(|_| BAD)?;
                super::super::fragment::validate(&bytes)?;
                let nodes = mp4::parse_boxes(&bytes).map_err(|_| BAD)?;
                let moof = nodes.first().ok_or(BAD)?;
                let traf = one(moof, *b"traf")?;
                let track_header = one(traf, *b"tfhd")?;
                let id = integer(track_header.header.payload(&bytes), 4)?;
                let selected = ids.iter().position(|n| *n == id).ok_or(BAD)?;
                // Match the original ID first; only the private view uses ID 1.
                let pos = track_header.header.offset + track_header.header.header_size + 4;
                bytes
                    .get_mut(pos..pos + 4)
                    .ok_or(BAD)?
                    .copy_from_slice(&VIEW_TRACK_ID.to_be_bytes());
                count[selected] = count[selected].checked_add(1).ok_or(BAD)?;
                let seq = one(moof, *b"mfhd")?;
                let pos = seq.header.offset + seq.header.header_size + 4;
                bytes[pos..pos + 4].copy_from_slice(&count[selected].to_be_bytes());
                outputs[selected].write_all(&bytes).map_err(|_| BAD)?;
                pending = Some(selected);
            }
            b"mdat" => {
                let selected = pending.take().ok_or(BAD)?;
                outputs[selected].write_all(&header).map_err(|_| BAD)?;
                super::super::copy_n(&mut file, &mut outputs[selected], size - 8, stop)?;
            }
            b"ftyp" | b"moov" | b"sidx" | b"mfra" | b"ssix" | b"free" => {
                if pending.is_some() {
                    return Err(BAD);
                }
                file.seek(SeekFrom::Start(at + size)).map_err(|_| BAD)?;
            }
            _ => return Err(BAD),
        }
        at += size;
    }
    if at != length
        || pending.is_some()
        || count.contains(&0)
        || crate::native::FileIdentity::read(&file).map_err(|_| BAD)? != identity
        || file.metadata().map_err(|_| BAD)?.len() != length
    {
        return Err(BAD);
    }
    Ok(())
}

/// Split only the fragment layout emitted by the complete-track combiner.
/// The metadata catalogue validates every original sample range before any view is used.
pub(in crate::conversion) fn split_parts(
    input: &Path,
    directory: &Path,
    stop: &Stop,
) -> Result<Vec<PathBuf>, ConversionError> {
    let metadata = super::super::super::native_media::inspect(input, stop)?;
    if !metadata.fragmented || !(1..=2).contains(&metadata.tracks.len()) {
        return Err(BAD);
    }
    let movie = audio::movie(input)?;
    let mut outputs = Vec::new();
    let mut paths = Vec::new();
    for index in 0..metadata.tracks.len() {
        let path = directory.join(format!("view-{index}.mp4"));
        outputs.push(super::super::super::files::create_new(&path)?);
        paths.push(path);
    }
    // File type must precede initialization, so append fragments into separate files
    // after their independently selected headers have been written.
    let mut source = super::super::super::files::regular_input(input)?;
    let mut header = [0; 8];
    source.read_exact(&mut header).map_err(|_| BAD)?;
    let size = usize::try_from(integer(&header, 0)?).map_err(|_| BAD)?;
    if &header[4..] != b"ftyp" || !(16..=1024 * 1024).contains(&size) {
        return Err(BAD);
    }
    let mut ftyp = header.to_vec();
    ftyp.resize(size, 0);
    source.read_exact(&mut ftyp[8..]).map_err(|_| BAD)?;
    let ftyp = super::super::generic_ftyp(&ftyp)?;
    for (index, output) in outputs.iter_mut().enumerate() {
        let init = initialization(&movie, index)?;
        output
            .write_all(&ftyp)
            .and_then(|()| output.write_all(&init))
            .map_err(|_| BAD)?;
    }
    let ids: Vec<_> = metadata.tracks.iter().map(|track| track.track_id).collect();
    // The header has already been emitted; the copier skips the original ftyp.
    copy_parts(input, &mut outputs, &ids, stop)?;
    for output in outputs {
        output.sync_all().map_err(|_| BAD)?;
    }
    Ok(paths)
}

#[cfg(test)]
mod tests;
