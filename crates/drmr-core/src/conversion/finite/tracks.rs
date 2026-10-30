//! Join complete fragmented tracks without changing their media clocks or sample bytes.
use super::{
    BAD, ConversionError, File, Path, Read, Stop, Write, audio, boxed, integer, mp4, one, rescale,
    scale,
};
use std::path::PathBuf;
mod split;
pub(in crate::conversion) use split::split_parts;

fn movie_scale(parts: &[PathBuf]) -> Result<u32, ConversionError> {
    let mut common = 1_u32;
    for part in parts {
        let movie = audio::movie(part)?;
        let nodes = mp4::parse_boxes(&movie.bytes).map_err(|_| BAD)?;
        let own = scale(one(nodes.first().ok_or(BAD)?, *b"mvhd")?, &movie.bytes)?;
        let (mut a, mut b) = (common, own);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        common = common
            .checked_div(a)
            .and_then(|n| n.checked_mul(own))
            .ok_or(BAD)?;
    }
    Ok(common)
}

fn track(
    bytes: &[u8],
    node: &mp4::Mp4Box,
    id: u32,
    from: u32,
    to: u32,
) -> Result<Vec<u8>, ConversionError> {
    let mut data = node.header.bytes(bytes).to_vec();
    let nodes = mp4::parse_boxes(&data).map_err(|_| BAD)?;
    let all = mp4::walk(&nodes);
    for child in all {
        let payload = child.header.payload(&data);
        let at = child.header.offset + child.header.header_size;
        match &child.kind() {
            b"tkhd" => {
                let (field, duration, width) = match payload.first() {
                    Some(0) => (12, 20, 4),
                    Some(1) => (20, 28, 8),
                    _ => return Err(BAD),
                };
                if payload
                    .get(duration..duration + width)
                    .is_none_or(|n| n.iter().any(|n| *n != 0))
                {
                    return Err(BAD);
                }
                data[at + field..at + field + 4].copy_from_slice(&id.to_be_bytes());
            }
            b"elst" => {
                let width = match payload.first() {
                    Some(0) => 4,
                    Some(1) => 8,
                    _ => return Err(BAD),
                };
                if payload.get(1..4) != Some(&[0, 0, 0]) {
                    return Err(BAD);
                }
                let count = usize::try_from(integer(payload, 4)?).map_err(|_| BAD)?;
                if !(1..=2).contains(&count) || payload.len() != 8 + count * (2 * width + 4) {
                    return Err(BAD);
                }
                let mut replacements = Vec::new();
                for index in 0..count {
                    let pos = 8 + index * (2 * width + 4);
                    if payload.get(pos + 2 * width..pos + 2 * width + 4) != Some(&[0, 1, 0, 0]) {
                        return Err(BAD);
                    }
                    let duration = if width == 4 {
                        u64::from(integer(payload, pos)?)
                    } else {
                        u64::from_be_bytes(payload[pos..pos + 8].try_into().map_err(|_| BAD)?)
                    };
                    let value = rescale(duration, from, to)?;
                    let encoded = if width == 4 {
                        u32::try_from(value)
                            .map_err(|_| BAD)?
                            .to_be_bytes()
                            .to_vec()
                    } else {
                        value.to_be_bytes().to_vec()
                    };
                    replacements.push((at + pos, encoded));
                }
                for (pos, value) in replacements {
                    data[pos..pos + width].copy_from_slice(&value);
                }
            }
            _ => {}
        }
    }
    Ok(data)
}

fn initialization(parts: &[PathBuf]) -> Result<Vec<u8>, ConversionError> {
    if parts.is_empty() || parts.len() > 2 {
        return Err(BAD);
    }
    let target_scale = movie_scale(parts)?;
    let mut common = Vec::new();
    let mut tracks = Vec::new();
    let mut defaults = Vec::new();
    let mut providers = Vec::<Vec<u8>>::new();
    let mut ftyp = None;
    for (index, part) in parts.iter().enumerate() {
        let movie = audio::movie(part)?;
        if !movie.fragmented {
            return Err(BAD);
        }
        let nodes = mp4::parse_boxes(&movie.bytes).map_err(|_| BAD)?;
        let moov = nodes.first().ok_or(BAD)?;
        let own_scale = scale(one(moov, *b"mvhd")?, &movie.bytes)?;
        let id = u32::try_from(index + 1).map_err(|_| BAD)?;
        tracks.extend(track(
            &movie.bytes,
            one(moov, *b"trak")?,
            id,
            own_scale,
            target_scale,
        )?);
        let extends = one(moov, *b"mvex")?;
        let mut trex = one(extends, *b"trex")?.header.bytes(&movie.bytes).to_vec();
        if trex.len() != 32 {
            return Err(BAD);
        }
        trex[12..16].copy_from_slice(&id.to_be_bytes());
        defaults.extend(trex);
        for child in &moov.children {
            if child.kind() == *b"pssh" {
                let pssh = child.header.bytes(&movie.bytes).to_vec();
                if !providers.contains(&pssh) {
                    providers.push(pssh);
                }
            } else if index == 0 && !matches!(&child.kind(), b"trak" | b"mvex") {
                let mut bytes = child.header.bytes(&movie.bytes).to_vec();
                if child.kind() == *b"mvhd" {
                    let (offset, duration, width) = match bytes.get(8) {
                        Some(0) => (12, 16, 4),
                        Some(1) => (20, 24, 8),
                        _ => return Err(BAD),
                    };
                    if bytes
                        .get(8 + duration..8 + duration + width)
                        .is_none_or(|n| n.iter().any(|n| *n != 0))
                    {
                        return Err(BAD);
                    }
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
        if index == 0 {
            let mut file = File::open(part).map_err(|_| BAD)?;
            let mut header = [0; 8];
            file.read_exact(&mut header).map_err(|_| BAD)?;
            let size = integer(&header, 0)? as usize;
            if &header[4..] != b"ftyp" || !(16..=1024 * 1024).contains(&size) {
                return Err(BAD);
            }
            let mut bytes = header.to_vec();
            bytes.resize(size, 0);
            file.read_exact(&mut bytes[8..]).map_err(|_| BAD)?;
            ftyp = Some(super::generic_ftyp(&bytes)?);
        }
        if common.len() + tracks.len() + defaults.len() > 16 * 1024 * 1024 {
            return Err(ConversionError("resource-budget"));
        }
    }
    common.extend(tracks);
    common.extend(boxed(*b"mvex", &defaults)?);
    for provider in providers {
        common.extend(provider);
    }
    let mut bytes = ftyp.ok_or(BAD)?;
    bytes.extend(boxed(*b"moov", &common)?);
    Ok(bytes)
}

pub(in crate::conversion) fn combine_parts(
    parts: &[PathBuf],
    output: &Path,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let metadata = initialization(parts)?;
    let mut file = super::super::files::create_new(output)?;
    file.write_all(&metadata).map_err(|_| BAD)?;
    let mut sequence = 0;
    for (index, path) in parts.iter().enumerate() {
        super::fragment::append_fragments(path, index, &mut file, stop, &mut sequence)?;
    }
    file.sync_all().map_err(|_| BAD)
}
