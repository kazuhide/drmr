//! Assemble independently encrypted tracks while retaining moof-relative byte addresses.
use super::{
    BAD, ConversionError, File, Path, Read, Seek, SeekFrom, Stop, Write, audio, boxed, copy_n,
    integer, mp4, one, rescale, rewritten, scale, window,
};
pub(super) fn validate(bytes: &[u8]) -> Result<(), ConversionError> {
    let nodes = mp4::parse_boxes(bytes).map_err(|_| BAD)?;
    let moof = nodes
        .first()
        .filter(|n| n.kind() == *b"moof" && nodes.len() == 1)
        .ok_or(BAD)?;
    let traf = one(moof, *b"traf")?;
    let tfhd = one(traf, *b"tfhd")?.header.payload(bytes);
    let flags = integer(tfhd, 0)?;
    if flags & 1 != 0 || flags & 0x20000 == 0 {
        return Err(BAD);
    }
    let trun = one(traf, *b"trun")?.header.payload(bytes);
    if integer(trun, 0)? & 1 == 0 {
        return Err(BAD);
    }
    Ok(())
}
fn initialization(
    parts: &[std::path::PathBuf],
    original: &Path,
) -> Result<Vec<u8>, ConversionError> {
    let source = audio::movie(original)?;
    let source_nodes = mp4::parse_boxes(&source.bytes).map_err(|_| BAD)?;
    let target_scale = scale(
        one(source_nodes.first().ok_or(BAD)?, *b"mvhd")?,
        &source.bytes,
    )?;
    let mut common = Vec::new();
    let mut tracks = Vec::new();
    let mut defaults = Vec::new();
    let mut ftyp = None;
    for (index, path) in parts.iter().enumerate() {
        let movie = audio::movie(path)?;
        let nodes = mp4::parse_boxes(&movie.bytes).map_err(|_| BAD)?;
        let moov = nodes.first().ok_or(BAD)?;
        let track = one(moov, *b"trak")?;
        let id = u32::try_from(index + 1).map_err(|_| BAD)?;
        let own = window(path, 0)?.ok_or(BAD)?;
        if !own.equivalent(window(original, index)?.ok_or(BAD)?) {
            return Err(BAD);
        }
        let mut ed = vec![1, 0, 0, 0];
        ed.extend(1u32.to_be_bytes());
        ed.extend(rescale(own.duration, own.scale, target_scale)?.to_be_bytes());
        ed.extend(own.start.to_be_bytes());
        ed.extend([0, 1, 0, 0]);
        let mut bytes = rewritten(
            &movie.bytes,
            track,
            &boxed(*b"edts", &boxed(*b"elst", &ed)?)?,
        )?;
        let parsed = mp4::parse_boxes(&bytes).map_err(|_| BAD)?;
        let tkhd = one(parsed.first().ok_or(BAD)?, *b"tkhd")?;
        let payload = tkhd.header.payload(&bytes);
        let offset = match payload.first() {
            Some(0) => 12,
            Some(1) => 20,
            _ => return Err(BAD),
        };
        let at = tkhd.header.offset + tkhd.header.header_size + offset;
        bytes[at..at + 4].copy_from_slice(&id.to_be_bytes());
        tracks.extend(bytes);
        let trex = one(one(moov, *b"mvex")?, *b"trex")?;
        let mut bytes = trex.header.bytes(&movie.bytes).to_vec();
        bytes[12..16].copy_from_slice(&id.to_be_bytes());
        defaults.extend(bytes);
        if index == 0 {
            for child in &moov.children {
                if !matches!(&child.kind(), b"trak" | b"mvex") {
                    let mut bytes = child.header.bytes(&movie.bytes).to_vec();
                    if child.kind() == *b"mvhd" {
                        let offset = match bytes.get(8) {
                            Some(0) => 12,
                            Some(1) => 20,
                            _ => return Err(BAD),
                        };
                        bytes[8 + offset..12 + offset].copy_from_slice(&target_scale.to_be_bytes());
                        let end = bytes.len();
                        bytes[end - 4..].copy_from_slice(
                            &u32::try_from(parts.len() + 1)
                                .map_err(|_| BAD)?
                                .to_be_bytes(),
                        );
                    }
                    common.extend(bytes);
                }
            }
            let mut file = File::open(path).map_err(|_| BAD)?;
            let mut header = [0; 8];
            file.read_exact(&mut header).map_err(|_| BAD)?;
            let size = integer(&header, 0)? as usize;
            if &header[4..] != b"ftyp" || !(8..=1024 * 1024).contains(&size) {
                return Err(BAD);
            }
            let mut bytes = header.to_vec();
            bytes.resize(size, 0);
            file.read_exact(&mut bytes[8..]).map_err(|_| BAD)?;
            ftyp = Some(super::generic_ftyp(&bytes)?);
        }
    }
    common.extend(tracks);
    common.extend(boxed(*b"mvex", &defaults)?);
    let mut bytes = ftyp.ok_or(BAD)?;
    bytes.extend(boxed(*b"moov", &common)?);
    Ok(bytes)
}
fn append_fragments(
    path: &Path,
    index: usize,
    output: &mut File,
    stop: &Stop,
    sequence: &mut u32,
) -> Result<(), ConversionError> {
    let mut file = super::super::files::regular_input(path)?;
    let length = file.metadata().map_err(|_| BAD)?.len();
    let mut at = 0;
    let mut pending = false;
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
        let kind = &header[4..];
        if size < 8 || size > length - at {
            return Err(BAD);
        }
        if kind == b"moof" {
            if pending || size > 16 * 1024 * 1024 {
                return Err(BAD);
            }
            let mut bytes = header.to_vec();
            bytes.resize(usize::try_from(size).map_err(|_| BAD)?, 0);
            file.read_exact(&mut bytes[8..]).map_err(|_| BAD)?;
            validate(&bytes)?;
            let nodes = mp4::parse_boxes(&bytes).map_err(|_| BAD)?;
            let moof = nodes.first().ok_or(BAD)?;
            let traf = one(moof, *b"traf")?;
            let tfhd = one(traf, *b"tfhd")?;
            let flags = integer(tfhd.header.payload(&bytes), 0)?;
            if flags & 1 != 0 || flags & 0x20000 == 0 {
                return Err(BAD);
            }
            let trun = one(traf, *b"trun")?;
            if integer(trun.header.payload(&bytes), 0)? & 1 == 0 {
                return Err(BAD);
            }
            let track_at = tfhd.header.offset + tfhd.header.header_size + 4;
            let mfhd = one(moof, *b"mfhd")?;
            let seq_at = mfhd.header.offset + mfhd.header.header_size + 4;
            bytes[track_at..track_at + 4]
                .copy_from_slice(&u32::try_from(index + 1).map_err(|_| BAD)?.to_be_bytes());
            *sequence = sequence.checked_add(1).ok_or(BAD)?;
            bytes[seq_at..seq_at + 4].copy_from_slice(&sequence.to_be_bytes());
            output.write_all(&bytes).map_err(|_| BAD)?;
            pending = true;
        } else if kind == b"mdat" {
            if !pending {
                return Err(BAD);
            }
            output.write_all(&header).map_err(|_| BAD)?;
            copy_n(&mut file, output, size - 8, stop)?;
            pending = false;
        } else if matches!(
            kind,
            b"ftyp" | b"moov" | b"sidx" | b"mfra" | b"ssix" | b"free"
        ) {
            if pending {
                return Err(BAD);
            }
            file.seek(SeekFrom::Start(at + size)).map_err(|_| BAD)?;
        } else {
            return Err(BAD);
        }
        at += size;
    }
    if pending || at != length {
        return Err(BAD);
    }
    Ok(())
}
pub(super) fn combine(
    parts: &[std::path::PathBuf],
    original: &Path,
    output: &Path,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let metadata = initialization(parts, original)?;
    let mut output = super::super::files::create_new(output)?;
    output.write_all(&metadata).map_err(|_| BAD)?;
    let mut sequence = 0;
    for (index, path) in parts.iter().enumerate() {
        append_fragments(path, index, &mut output, stop, &mut sequence)?;
    }
    output.sync_all().map_err(|_| BAD)
}
