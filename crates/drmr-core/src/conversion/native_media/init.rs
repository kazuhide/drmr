use super::{BAD, ConversionError, Edit, Movie, Protection, Scheme, TrackDescriptor, UNSUPPORTED};
use super::{
    crypto::TrackEncryption,
    read::{Layout, count, full, integer, one, optional, signed16, unsigned64},
};
use crate::{
    inspection::TrackKind,
    mp4::{self, Mp4Box},
};

#[derive(Clone, Copy, Default)]
pub(super) struct Defaults {
    pub duration: u32,
    pub size: u32,
    pub flags: u32,
}
pub(super) struct Track {
    pub descriptor: TrackDescriptor,
    pub table: Mp4Box,
    pub encryption: Option<TrackEncryption>,
    pub defaults: Defaults,
    pub auxiliary: Vec<Mp4Box>,
    pub groups: super::groups::Descriptions,
}
pub(super) struct Initialization {
    pub movie: Movie,
    pub tracks: Vec<Track>,
}

fn clock(bytes: &[u8]) -> Result<(u32, u64), ConversionError> {
    let version = *bytes.first().ok_or(BAD)?;
    if version > 1 {
        return Err(BAD);
    }
    full(bytes, version, 0)?;
    let (scale, duration) = if version == 0 {
        (integer(bytes, 12)?, u64::from(integer(bytes, 16)?))
    } else {
        (integer(bytes, 20)?, unsigned64(bytes, 24)?)
    };
    if scale == 0 {
        return Err(BAD);
    }
    Ok((scale, duration))
}
pub(super) fn matrix(bytes: &[u8], at: usize) -> Result<[i32; 9], ConversionError> {
    let mut matrix = [0; 9];
    for (index, value) in matrix.iter_mut().enumerate() {
        *value = integer(bytes, at + index * 4)?.cast_signed();
    }
    Ok(matrix)
}
fn edits(track: &Mp4Box, bytes: &[u8]) -> Result<Vec<Edit>, ConversionError> {
    let Some(edit_container) = optional(&track.children, *b"edts")? else {
        return Ok(Vec::new());
    };
    if edit_container.children.len() != 1 {
        return Err(UNSUPPORTED);
    }
    let payload = one(&edit_container.children, *b"elst")?
        .header
        .payload(bytes);
    let version = *payload.first().ok_or(BAD)?;
    if version > 1 {
        return Err(BAD);
    }
    full(payload, version, 0)?;
    let width = if version == 0 { 12 } else { 20 };
    let total = count(payload, 4, 8, width)?;
    if total == 0 {
        return Err(UNSUPPORTED);
    }
    let mut edits = Vec::with_capacity(total);
    for item in payload[8..].chunks_exact(width) {
        let (duration, media_time, rate) = if version == 0 {
            (
                u64::from(integer(item, 0)?),
                i64::from(integer(item, 4)?.cast_signed()),
                8,
            )
        } else {
            (unsigned64(item, 0)?, unsigned64(item, 8)?.cast_signed(), 16)
        };
        let rate_integer = signed16(item, rate)?;
        let rate_fraction = signed16(item, rate + 2)?;
        if media_time < -1 || rate_integer != 1 || rate_fraction != 0 {
            return Err(UNSUPPORTED);
        }
        edits.push(Edit {
            duration,
            media_time,
            rate_integer,
            rate_fraction,
        });
    }
    Ok(edits)
}
pub(super) fn language(mdhd: &[u8]) -> Result<Option<String>, ConversionError> {
    let at = if mdhd[0] == 0 { 20 } else { 32 };
    let code = signed16(mdhd, at)?.cast_unsigned();
    if code == 0 || code == 0x7fff {
        return Ok(None);
    }
    if code & 0x8000 != 0 {
        return Err(BAD);
    }
    let letters = [
        ((code >> 10) & 31) as u8,
        ((code >> 5) & 31) as u8,
        (code & 31) as u8,
    ];
    if letters.iter().any(|letter| !(1..=26).contains(letter)) {
        return Err(UNSUPPORTED);
    }
    Ok(Some(
        letters
            .into_iter()
            .map(|letter| char::from(letter + b'`'))
            .collect(),
    ))
}
fn descriptor<'a>(bytes: &'a [u8], at: &mut usize, tag: u8) -> Result<&'a [u8], ConversionError> {
    if bytes.get(*at) != Some(&tag) {
        return Err(UNSUPPORTED);
    }
    *at += 1;
    let mut length = 0usize;
    let mut complete = false;
    for _ in 0..4 {
        let value = *bytes.get(*at).ok_or(BAD)?;
        *at += 1;
        length = length
            .checked_mul(128)
            .and_then(|n| n.checked_add(usize::from(value & 127)))
            .ok_or(BAD)?;
        if value & 128 == 0 {
            complete = true;
            break;
        }
    }
    if !complete {
        return Err(BAD);
    }
    let end = at.checked_add(length).ok_or(BAD)?;
    let payload = bytes.get(*at..end).ok_or(BAD)?;
    *at = end;
    Ok(payload)
}
/// Extract AAC `AudioSpecificConfig` without trusting descriptor length or nesting claims.
pub(super) fn audio_specific_config(bytes: &[u8]) -> Result<Vec<u8>, ConversionError> {
    full(bytes, 0, 0)?;
    let mut at = 4;
    let es = descriptor(bytes, &mut at, 3)?;
    if at != bytes.len() || es.len() < 3 {
        return Err(BAD);
    }
    let flags = es[2];
    let mut offset = 3;
    if flags & 0x80 != 0 {
        offset += 2;
    }
    if flags & 0x40 != 0 {
        let n = usize::from(*es.get(offset).ok_or(BAD)?);
        offset = offset.checked_add(1 + n).ok_or(BAD)?;
    }
    if flags & 0x20 != 0 {
        offset += 2;
    }
    let decoder = descriptor(es, &mut offset, 4)?;
    if decoder.len() < 13 || decoder[0] != 0x40 || decoder[1] != 0x15 {
        return Err(UNSUPPORTED);
    }
    let mut config_at = 13;
    let config = descriptor(decoder, &mut config_at, 5)?;
    if config_at != decoder.len() || config.len() < 2 || config.len() > 65_535 {
        return Err(UNSUPPORTED);
    }
    super::AacConfiguration::parse(config)?;
    if offset < es.len() {
        let sl = descriptor(es, &mut offset, 6)?;
        if sl != [2] {
            return Err(UNSUPPORTED);
        }
    }
    if offset != es.len() {
        return Err(BAD);
    }
    Ok(config.to_vec())
}
fn table(track: &Mp4Box) -> Result<Mp4Box, ConversionError> {
    let mdia = one(&track.children, *b"mdia")?;
    let minf = one(&mdia.children, *b"minf")?;
    let table = one(&minf.children, *b"stbl")?;
    for node in &table.children {
        if !matches!(
            &node.kind(),
            b"stsd"
                | b"stts"
                | b"ctts"
                | b"stsc"
                | b"stsz"
                | b"stco"
                | b"co64"
                | b"stss"
                | b"sdtp"
                | b"senc"
                | b"saiz"
                | b"saio"
                | b"sgpd"
                | b"sbgp"
                | b"free"
                | b"skip"
        ) {
            return Err(UNSUPPORTED);
        }
    }
    Ok(table.clone())
}
fn protection(encryption: Option<&TrackEncryption>) -> Protection {
    encryption.map_or(
        Protection {
            scheme: Scheme::Clear,
            key_id: None,
            per_sample_iv_size: 0,
            crypt_byte_block: 0,
            skip_byte_block: 0,
        },
        |value| Protection {
            scheme: value.scheme,
            key_id: Some(value.kid),
            per_sample_iv_size: value.iv_size,
            crypt_byte_block: value.crypt,
            skip_byte_block: value.skip,
        },
    )
}
fn defaults(
    moov: &Mp4Box,
    data: &[u8],
) -> Result<std::collections::BTreeMap<u32, Defaults>, ConversionError> {
    let mut defaults = std::collections::BTreeMap::new();
    if let Some(mvex) = optional(&moov.children, *b"mvex")? {
        for node in &mvex.children {
            if matches!(&node.kind(), b"mehd" | b"free" | b"skip") {
                continue;
            }
            if node.kind() != *b"trex" {
                return Err(UNSUPPORTED);
            }
            let p = node.header.payload(data);
            full(p, 0, 0)?;
            if p.len() != 24 || integer(p, 8)? != 1 {
                return Err(UNSUPPORTED);
            }
            let id = integer(p, 4)?;
            let value = Defaults {
                duration: integer(p, 12)?,
                size: integer(p, 16)?,
                flags: integer(p, 20)?,
            };
            if id == 0 || defaults.insert(id, value).is_some() {
                return Err(BAD);
            }
        }
    }
    Ok(defaults)
}
fn fragment_duration(moov: &Mp4Box, data: &[u8]) -> Result<Option<u64>, ConversionError> {
    let Some(mvex) = optional(&moov.children, *b"mvex")? else {
        return Ok(None);
    };
    let Some(mehd) = optional(&mvex.children, *b"mehd")? else {
        return Ok(None);
    };
    let p = mehd.header.payload(data);
    let version = *p.first().ok_or(BAD)?;
    if version > 1 {
        return Err(BAD);
    }
    full(p, version, 0)?;
    Ok(Some(match (version, p.len()) {
        (0, 8) => u64::from(integer(p, 4)?),
        (1, 12) => unsigned64(p, 4)?,
        _ => return Err(BAD),
    }))
}
fn track_header(bytes: &[u8]) -> Result<(u32, u64), ConversionError> {
    let (version, id_at, duration_at) = match (bytes.first(), bytes.len()) {
        (Some(0), 84) => (0, 12, 20),
        (Some(1), 96) => (1, 20, 28),
        _ => return Err(BAD),
    };
    full(bytes, version, 15)?;
    let extra = if version == 0 { 0 } else { 12 };
    let after_id = if version == 0 { 16 } else { 24 };
    if bytes[after_id..after_id + 4].iter().any(|byte| *byte != 0)
        || bytes[24 + extra..32 + extra].iter().any(|byte| *byte != 0)
        || bytes[38 + extra..40 + extra].iter().any(|byte| *byte != 0)
    {
        return Err(BAD);
    }
    let id = integer(bytes, id_at)?;
    if id == 0 {
        return Err(BAD);
    }
    let duration = if version == 0 {
        u64::from(integer(bytes, duration_at)?)
    } else {
        unsigned64(bytes, duration_at)?
    };
    Ok((id, duration))
}
fn media_header(
    minf: &Mp4Box,
    kind: TrackKind,
    data: &[u8],
) -> Result<super::MediaHeader, ConversionError> {
    let sound = optional(&minf.children, *b"smhd")?;
    let video = optional(&minf.children, *b"vmhd")?;
    if minf.children.iter().any(|node| {
        !matches!(
            &node.kind(),
            b"smhd" | b"vmhd" | b"dinf" | b"stbl" | b"free" | b"skip"
        )
    }) {
        return Err(UNSUPPORTED);
    }
    match (kind, sound, video) {
        (TrackKind::Audio, Some(node), None) => {
            let p = node.header.payload(data);
            full(p, 0, 0)?;
            if p.len() != 8 || signed16(p, 6)? != 0 {
                return Err(BAD);
            }
            Ok(super::MediaHeader::Audio {
                balance: signed16(p, 4)?,
            })
        }
        (TrackKind::Video, None, Some(node)) => {
            let p = node.header.payload(data);
            if full(p, 0, 1)? != 1 || p.len() != 12 {
                return Err(BAD);
            }
            Ok(super::MediaHeader::Video {
                graphics_mode: signed16(p, 4)?.cast_unsigned(),
                opcolor: [
                    signed16(p, 6)?.cast_unsigned(),
                    signed16(p, 8)?.cast_unsigned(),
                    signed16(p, 10)?.cast_unsigned(),
                ],
            })
        }
        _ => Err(BAD),
    }
}
fn track(
    index: usize,
    node: &Mp4Box,
    defaults: &mut std::collections::BTreeMap<u32, Defaults>,
    fragmented: bool,
    data: &[u8],
) -> Result<Track, ConversionError> {
    let tkhd = one(&node.children, *b"tkhd")?.header.payload(data);
    let (track_id, tkhd_duration) = track_header(tkhd)?;
    let mdia = one(&node.children, *b"mdia")?;
    let mdhd = one(&mdia.children, *b"mdhd")?.header.payload(data);
    if !matches!((mdhd.first(), mdhd.len()), (Some(0), 24) | (Some(1), 36)) {
        return Err(BAD);
    }
    let (timescale, media_duration) = clock(mdhd)?;
    let handler = one(&mdia.children, *b"hdlr")?.header.payload(data);
    full(handler, 0, 0)?;
    if handler.len() < 24 {
        return Err(BAD);
    }
    let kind = match handler.get(8..12) {
        Some(b"vide") => TrackKind::Video,
        Some(b"soun") => TrackKind::Audio,
        _ => return Err(UNSUPPORTED),
    };
    let minf = one(&mdia.children, *b"minf")?;
    let media_header = media_header(minf, kind, data)?;
    let dref = one(&one(&minf.children, *b"dinf")?.children, *b"dref")?;
    if dref.header.payload(data)
        != [
            0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 12, b'u', b'r', b'l', b' ', 0, 0, 0, 1,
        ]
    {
        return Err(UNSUPPORTED);
    }
    let table = table(node)?;
    let super::entry::ParsedEntry {
        codec,
        decoder_config_kind,
        decoder_config,
        decoder_config_box,
        sample_entry_metadata,
        encryption,
    } = super::entry::parse(&table, node, mdhd, kind, data)?;
    let defaults = if fragmented {
        defaults.remove(&track_id).ok_or(BAD)?
    } else {
        Defaults::default()
    };
    let auxiliary = table
        .children
        .iter()
        .chain(node.children.iter())
        .filter(|child| matches!(&child.kind(), b"senc" | b"saiz" | b"saio"))
        .cloned()
        .collect();
    let descriptor = TrackDescriptor {
        index,
        track_id,
        kind,
        timescale,
        media_duration,
        tkhd_duration,
        sample_count: 0,
        codec,
        decoder_config_kind,
        decoder_config,
        decoder_config_box,
        sample_entry_metadata,
        media_header,
        edits: edits(node, data)?,
        protection: protection(encryption.as_ref()),
    };
    let groups = super::groups::Descriptions::parse(&table.children, data)?;
    Ok(Track {
        descriptor,
        table,
        encryption,
        defaults,
        auxiliary,
        groups,
    })
}
/// Reject unmodeled playback controls at semantic container boundaries.
fn semantic_children(container: &Mp4Box) -> Result<(), ConversionError> {
    let known: &[[u8; 4]] = match &container.kind() {
        b"moov" => &[
            *b"mvhd", *b"trak", *b"mvex", *b"iods", *b"pssh", *b"udta", *b"meta",
        ],
        b"trak" => &[
            *b"tkhd", *b"mdia", *b"edts", *b"senc", *b"saiz", *b"saio", *b"udta", *b"meta",
        ],
        b"mdia" => &[*b"mdhd", *b"hdlr", *b"minf"],
        b"minf" => &[*b"smhd", *b"vmhd", *b"dinf", *b"stbl"],
        b"dinf" => &[*b"dref"],
        b"mvex" => &[*b"mehd", *b"trex"],
        _ => return Ok(()),
    };
    for child in &container.children {
        if !known.contains(&child.kind()) && !matches!(&child.kind(), b"free" | b"skip") {
            return Err(UNSUPPORTED);
        }
    }
    Ok(())
}
fn census(moov: &Mp4Box, tracks: &[Track]) -> Result<(), ConversionError> {
    // A moov/udta meta box describes the file (for example static ID32 tags), not
    // a timed media track. Its descriptive tags are outside sample preservation;
    // nested sample, edit, group or protection declarations still cannot escape
    // this census. Timed metadata handlers are refused by track admission.
    let mut allowed = std::collections::BTreeSet::new();
    if let Some(mvex) = optional(&moov.children, *b"mvex")? {
        allowed.extend(mvex.children.iter().map(|node| node.header.offset));
    }
    for node in moov.children.iter().filter(|node| node.kind() == *b"trak") {
        if let Some(edit_container) = optional(&node.children, *b"edts")? {
            allowed.insert(edit_container.header.offset);
            allowed.extend(
                edit_container
                    .children
                    .iter()
                    .map(|child| child.header.offset),
            );
        }
    }
    for track in tracks {
        allowed.extend(track.auxiliary.iter().map(|node| node.header.offset));
        allowed.extend(
            track
                .table
                .children
                .iter()
                .filter(|node| matches!(&node.kind(), b"sgpd" | b"sbgp"))
                .map(|node| node.header.offset),
        );
        let stsd = one(&track.table.children, *b"stsd")?;
        if let Some(sinf) = optional(&stsd.children[0].children, *b"sinf")? {
            allowed.extend(
                mp4::walk(std::slice::from_ref(sinf))
                    .iter()
                    .map(|node| node.header.offset),
            );
        }
    }
    for node in mp4::walk(std::slice::from_ref(moov)) {
        semantic_children(node)?;
        if matches!(
            &node.kind(),
            b"sinf"
                | b"frma"
                | b"schm"
                | b"schi"
                | b"tenc"
                | b"senc"
                | b"saiz"
                | b"saio"
                | b"sgpd"
                | b"sbgp"
                | b"uuid"
                | b"trex"
                | b"tfhd"
                | b"tfdt"
                | b"trun"
                | b"mfhd"
                | b"edts"
                | b"elst"
        ) && !allowed.contains(&node.header.offset)
        {
            return Err(UNSUPPORTED);
        }
    }
    Ok(())
}
pub(super) fn parse(layout: &Layout) -> Result<Initialization, ConversionError> {
    let data = &layout.movie;
    let nodes = mp4::parse_boxes_with_limit(data, super::MAX_PARSED_NODES).map_err(|_| BAD)?;
    let moov = one(&nodes, *b"moov")?;
    if nodes.len() != 1 {
        return Err(BAD);
    }
    let mvhd = one(&moov.children, *b"mvhd")?.header.payload(data);
    if !matches!((mvhd.first(), mvhd.len()), (Some(0), 100) | (Some(1), 112)) {
        return Err(BAD);
    }
    let (timescale, duration) = clock(mvhd)?;
    let extra = if mvhd[0] == 0 { 0 } else { 12 };
    let mut movie = Movie {
        timescale,
        duration,
        rate: integer(mvhd, 20 + extra)?.cast_signed(),
        volume: signed16(mvhd, 24 + extra)?,
        matrix: matrix(mvhd, 36 + extra)?,
        fragment_duration: fragment_duration(moov, data)?,
        tracks: Vec::new(),
        fragmented: optional(&moov.children, *b"mvex")?.is_some(),
    };
    if !layout.moofs.is_empty() && !movie.fragmented {
        return Err(BAD);
    }
    let mut defaults = defaults(moov, data)?;
    let mut tracks = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for (index, node) in moov
        .children
        .iter()
        .filter(|node| node.kind() == *b"trak")
        .enumerate()
    {
        let track = track(index, node, &mut defaults, movie.fragmented, data)?;
        if !ids.insert(track.descriptor.track_id) {
            return Err(BAD);
        }
        movie.tracks.push(track.descriptor.clone());
        tracks.push(track);
    }
    if tracks.is_empty() || !defaults.is_empty() {
        return Err(BAD);
    }
    census(moov, &tracks)?;
    Ok(Initialization { movie, tracks })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = u32::try_from(payload.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        bytes.extend(kind);
        bytes.extend(payload);
        bytes
    }
    fn duration(payload: &[u8], duplicate: bool) -> Result<Option<u64>, ConversionError> {
        let mehd = boxed(*b"mehd", payload);
        let mvex = boxed(
            *b"mvex",
            &if duplicate {
                [mehd.clone(), mehd].concat()
            } else {
                mehd
            },
        );
        let bytes = boxed(*b"moov", &mvex);
        let nodes = mp4::parse_boxes(&bytes).unwrap();
        fragment_duration(&nodes[0], &bytes)
    }
    #[test]
    fn movie_fragment_duration_preserves_both_versions_and_rejects_ambiguous_shapes() {
        assert_eq!(
            duration(&[0, 0, 0, 0, 0, 0, 0, 10], false).unwrap(),
            Some(10)
        );
        assert_eq!(
            duration(&[1, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0], false).unwrap(),
            Some(1_u64 << 32)
        );
        for payload in [
            &[0, 0, 0, 0, 0, 0, 0][..],
            &[0, 0, 0, 1, 0, 0, 0, 10],
            &[2, 0, 0, 0, 0, 0, 0, 10],
        ] {
            assert!(duration(payload, false).is_err());
        }
        assert!(duration(&[0, 0, 0, 0, 0, 0, 0, 10], true).is_err());
    }

    #[test]
    fn track_duration_keeps_the_declared_movie_clock_without_rescaling_or_zero_defaults() {
        let mut v0 = vec![0; 84];
        v0[3] = 3;
        v0[12..16].copy_from_slice(&7_u32.to_be_bytes());
        v0[20..24].copy_from_slice(&10_000_u32.to_be_bytes());
        assert_eq!(track_header(&v0).unwrap(), (7, 10_000));
        let mut v1 = vec![0; 96];
        v1[0] = 1;
        v1[3] = 3;
        v1[20..24].copy_from_slice(&7_u32.to_be_bytes());
        v1[28..36].copy_from_slice(&(1_u64 << 40).to_be_bytes());
        assert_eq!(track_header(&v1).unwrap(), (7, 1_u64 << 40));
        v0[20..24].copy_from_slice(&0_u32.to_be_bytes());
        assert_eq!(track_header(&v0).unwrap().1, 0);
        v1[28..36].copy_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(track_header(&v1).unwrap().1, u64::MAX);
        for end in [0, 3, 20, 83] {
            assert!(track_header(&v0[..end]).is_err());
        }
        v0[3] = 16;
        assert!(track_header(&v0).is_err());
    }

    fn header(
        kind: TrackKind,
        children: &[Vec<u8>],
    ) -> Result<super::super::MediaHeader, ConversionError> {
        let data = boxed(*b"minf", &children.concat());
        let nodes = mp4::parse_boxes(&data).unwrap();
        media_header(&nodes[0], kind, &data)
    }

    #[test]
    fn media_headers_retain_sound_balance_and_video_compositing_controls() {
        let sound = boxed(*b"smhd", &[0, 0, 0, 0, 0xff, 0, 0, 0]);
        assert_eq!(
            header(TrackKind::Audio, &[sound]).unwrap(),
            super::super::MediaHeader::Audio { balance: -256 }
        );
        let video = boxed(*b"vmhd", &[0, 0, 0, 1, 0, 64, 0, 1, 0, 2, 0, 3]);
        assert_eq!(
            header(TrackKind::Video, &[video]).unwrap(),
            super::super::MediaHeader::Video {
                graphics_mode: 64,
                opcolor: [1, 2, 3]
            }
        );
    }

    #[test]
    fn media_header_kind_duplicates_reserved_bytes_flags_versions_and_lengths_are_strict() {
        let sound = boxed(*b"smhd", &[0; 8]);
        let video = boxed(*b"vmhd", &[0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(header(TrackKind::Audio, &[]).is_err());
        assert!(header(TrackKind::Audio, std::slice::from_ref(&video)).is_err());
        assert!(header(TrackKind::Video, std::slice::from_ref(&sound)).is_err());
        assert!(header(TrackKind::Audio, &[sound.clone(), sound.clone()]).is_err());
        assert!(header(TrackKind::Video, &[video.clone(), video.clone()]).is_err());
        assert!(header(TrackKind::Audio, &[sound, video]).is_err());
        for payload in [
            vec![0; 7],
            vec![0; 9],
            vec![0, 0, 0, 0, 0, 0, 0, 1],
            vec![1, 0, 0, 0, 0, 0, 0, 0],
            vec![0, 0, 0, 1, 0, 0, 0, 0],
        ] {
            assert!(header(TrackKind::Audio, &[boxed(*b"smhd", &payload)]).is_err());
        }
        for payload in [
            vec![0; 12],
            vec![0, 0, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0],
            vec![0, 0, 0, 1],
            vec![0; 13],
        ] {
            assert!(header(TrackKind::Video, &[boxed(*b"vmhd", &payload)]).is_err());
        }
    }
    fn container_children(kind: [u8; 4], children: &[[u8; 4]]) -> Result<(), ConversionError> {
        let children: Vec<u8> = children
            .iter()
            .flat_map(|kind| boxed(*kind, if kind == b"meta" { &[0; 4] } else { &[] }))
            .collect();
        let data = boxed(kind, &children);
        let nodes = mp4::parse_boxes(&data).unwrap();
        semantic_children(&nodes[0])
    }

    #[test]
    fn semantic_container_whitelists_accept_recognized_child_kinds_and_padding() {
        for (parent, children) in [
            (
                *b"moov",
                vec![
                    *b"mvhd", *b"trak", *b"mvex", *b"iods", *b"pssh", *b"udta", *b"meta",
                ],
            ),
            (
                *b"trak",
                vec![
                    *b"tkhd", *b"mdia", *b"edts", *b"senc", *b"saiz", *b"saio", *b"udta", *b"meta",
                ],
            ),
            (*b"mdia", vec![*b"mdhd", *b"hdlr", *b"minf"]),
            (*b"minf", vec![*b"smhd", *b"vmhd", *b"dinf", *b"stbl"]),
            (*b"dinf", vec![*b"dref"]),
            (*b"mvex", vec![*b"mehd", *b"trex"]),
        ] {
            container_children(parent, &children).unwrap();
            container_children(parent, &[*b"free", *b"skip"]).unwrap();
        }
    }

    #[test]
    fn unknown_or_misplaced_semantic_children_are_refused_at_each_container_boundary() {
        for parent in [*b"moov", *b"trak", *b"mdia", *b"minf", *b"dinf", *b"mvex"] {
            assert_eq!(
                container_children(parent, &[*b"zzzz"]).unwrap_err(),
                UNSUPPORTED
            );
        }
        for (parent, child) in [
            (*b"trak", *b"tapt"),
            (*b"trak", *b"tref"),
            (*b"mdia", *b"elng"),
            (*b"moov", *b"clef"),
            (*b"minf", *b"tapt"),
            (*b"dinf", *b"pssh"),
            (*b"mvex", *b"leva"),
        ] {
            assert_eq!(
                container_children(parent, &[child]).unwrap_err(),
                UNSUPPORTED
            );
        }
    }
}
