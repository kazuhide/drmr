//! Bounded metadata scanning; encoded sample bytes are never read or moved.
use super::{BAD, CHANGED, ConversionError, LIMIT, OVERFLOW, owned::BoundFile};
use crate::{
    concurrency::Stop,
    mp4::{self, Mp4Box},
};
use sha2::{Digest as _, Sha256};
use std::{
    fs::File,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
};
use zeroize::Zeroizing;

const MAX_METADATA: usize = 16 * 1024 * 1024;
const MAX_BOXES: usize = 200_000;
const MAX_PATCHES: usize = 100_000;

#[derive(Default)]
pub(super) struct Budget {
    metadata: usize,
    nodes: usize,
    patches: usize,
    retained: usize,
}
impl Budget {
    pub(super) fn retain(&mut self, bytes: usize) -> Result<(), ConversionError> {
        self.retained = self
            .retained
            .checked_add(bytes)
            .filter(|n| *n <= MAX_METADATA)
            .ok_or(LIMIT)?;
        Ok(())
    }
    fn metadata(&mut self, bytes: usize) -> Result<(), ConversionError> {
        self.metadata = self
            .metadata
            .checked_add(bytes)
            .filter(|n| *n <= MAX_METADATA)
            .ok_or(LIMIT)?;
        Ok(())
    }
    fn nodes(&mut self, count: usize) -> Result<(), ConversionError> {
        self.nodes = self
            .nodes
            .checked_add(count)
            .filter(|n| *n <= MAX_BOXES)
            .ok_or(LIMIT)?;
        Ok(())
    }
    fn patch(&mut self) -> Result<(), ConversionError> {
        self.retain(std::mem::size_of::<Patch>())?;
        self.patches = self
            .patches
            .checked_add(1)
            .filter(|n| *n <= MAX_PATCHES)
            .ok_or(LIMIT)?;
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) struct Track {
    pub id: u32,
    pub scale: u32,
    pub first: u64,
}

pub(super) struct Patch {
    at: u64,
    before: [u8; 8],
    after: [u8; 8],
    length: usize,
}
impl Patch {
    fn new(
        at: u64,
        before: &[u8],
        after: &[u8],
        budget: &mut Budget,
    ) -> Result<Self, ConversionError> {
        if before.len() != after.len() || before.is_empty() || before.len() > 8 {
            return Err(BAD);
        }
        budget.patch()?;
        let mut patch = Self {
            at,
            before: [0; 8],
            after: [0; 8],
            length: before.len(),
        };
        patch.before[..before.len()].copy_from_slice(before);
        patch.after[..after.len()].copy_from_slice(after);
        Ok(patch)
    }
}

pub(super) struct Plan {
    pub track: Track,
    pub patches: Vec<Patch>,
    pub digest: [u8; 32],
}

pub(super) fn checkpoint(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, ConversionError> {
    Ok(u32::from_be_bytes(
        bytes
            .get(at..at + 4)
            .ok_or(BAD)?
            .try_into()
            .map_err(|_| BAD)?,
    ))
}
fn number(bytes: &[u8], at: usize, width: usize) -> Result<u64, ConversionError> {
    match width {
        4 => Ok(u64::from(u32_at(bytes, at)?)),
        8 => Ok(u64::from_be_bytes(
            bytes
                .get(at..at + 8)
                .ok_or(BAD)?
                .try_into()
                .map_err(|_| BAD)?,
        )),
        _ => Err(BAD),
    }
}
fn one(nodes: &[Mp4Box], kind: [u8; 4]) -> Result<&Mp4Box, ConversionError> {
    let mut it = nodes.iter().filter(|n| n.kind() == kind);
    let node = it.next().ok_or(BAD)?;
    if it.next().is_some() {
        return Err(BAD);
    }
    Ok(node)
}
fn parse(bytes: &[u8], budget: &mut Budget) -> Result<Vec<Mp4Box>, ConversionError> {
    let nodes = mp4::parse_boxes_with_limit(bytes, MAX_BOXES.saturating_sub(budget.nodes))
        .map_err(|error| {
            if error.0 == "mp4-box-count-limit" {
                LIMIT
            } else {
                BAD
            }
        })?;
    budget.nodes(mp4::walk(&nodes).len())?;
    Ok(nodes)
}
fn version(bytes: &[u8]) -> Result<usize, ConversionError> {
    if bytes.get(1..4) != Some(&[0, 0, 0]) {
        return Err(BAD);
    }
    match bytes.first() {
        Some(0) => Ok(4),
        Some(1) => Ok(8),
        _ => Err(BAD),
    }
}
fn track_version(bytes: &[u8]) -> Result<usize, ConversionError> {
    if u32_at(bytes, 0)? & 0x00ff_fff8 != 0 {
        return Err(BAD);
    }
    match bytes.first() {
        Some(0) => Ok(4),
        Some(1) => Ok(8),
        _ => Err(BAD),
    }
}

fn identity(bytes: &[u8], nodes: &[Mp4Box], clear: bool) -> Result<Track, ConversionError> {
    let moov = one(nodes, *b"moov")?;
    let track = one(&moov.children, *b"trak")?;
    let media = one(&track.children, *b"mdia")?;
    let header = one(&track.children, *b"tkhd")?.header.payload(bytes);
    track_version(header)?;
    let id = u32_at(header, if header[0] == 0 { 12 } else { 20 })?;
    let mdhd = one(&media.children, *b"mdhd")?.header.payload(bytes);
    version(mdhd)?;
    let scale = u32_at(mdhd, if mdhd[0] == 0 { 12 } else { 20 })?;
    let handler = one(&media.children, *b"hdlr")?.header.payload(bytes);
    if id == 0
        || scale == 0
        || handler.get(..4) != Some(&[0; 4])
        || handler.get(8..12) != Some(b"soun")
    {
        return Err(BAD);
    }
    let trex = one(&one(&moov.children, *b"mvex")?.children, *b"trex")?
        .header
        .payload(bytes);
    if trex.len() != 24 || u32_at(trex, 0)? != 0 || u32_at(trex, 4)? != id {
        return Err(BAD);
    }
    let table = one(&one(&media.children, *b"minf")?.children, *b"stbl")?;
    let descriptions = one(&table.children, *b"stsd")?;
    if descriptions.header.payload(bytes).get(..8) != Some(&[0, 0, 0, 0, 0, 0, 0, 1])
        || descriptions.children.len() != 1
    {
        return Err(BAD);
    }
    if !matches!(&descriptions.children[0].kind(), b"mp4a" | b"enca")
        || (clear && descriptions.children[0].kind() != *b"mp4a")
    {
        return Err(BAD);
    }
    Ok(Track {
        id,
        scale,
        first: 0,
    })
}

struct BoxBytes {
    at: u64,
    header: usize,
    kind: [u8; 4],
    data: Zeroizing<Vec<u8>>,
}
fn scan(
    file: &mut (impl std::io::Read + std::io::Seek),
    length: u64,
    budget: &mut Budget,
    stop: &Stop,
    mut visit: impl FnMut(&BoxBytes, &mut Budget) -> Result<(), ConversionError>,
) -> Result<[u8; 32], ConversionError> {
    file.seek(SeekFrom::Start(0)).map_err(|_| BAD)?;
    let mut at = 0_u64;
    let mut hash = Sha256::new();
    hash.update(length.to_be_bytes());
    while at < length {
        checkpoint(stop)?;
        budget.nodes(1)?;
        let mut raw = [0; 8];
        file.read_exact(&mut raw).map_err(|_| BAD)?;
        let mut data = Zeroizing::new(raw.to_vec());
        let mut header = 8;
        let kind = raw[4..8].try_into().map_err(|_| BAD)?;
        let size = match u32_at(&raw, 0)? {
            0 => length - at,
            1 => {
                let mut wide = [0; 8];
                file.read_exact(&mut wide).map_err(|_| BAD)?;
                data.extend(wide);
                header = 16;
                u64::from_be_bytes(wide)
            }
            n => u64::from(n),
        };
        if size < header as u64 || size > length - at {
            return Err(BAD);
        }
        hash.update(at.to_be_bytes());
        hash.update(&*data);
        if kind != *b"mdat" {
            if !matches!(
                &kind,
                b"ftyp" | b"styp" | b"moov" | b"moof" | b"sidx" | b"free" | b"skip" | b"pssh"
            ) {
                return Err(BAD);
            }
            let count = usize::try_from(size).map_err(|_| LIMIT)?;
            budget.metadata(count)?;
            data.resize(count, 0);
            file.read_exact(&mut data[header..]).map_err(|_| BAD)?;
            hash.update(&data[header..]);
        }
        visit(
            &BoxBytes {
                at,
                header,
                kind,
                data,
            },
            budget,
        )?;
        at = at.checked_add(size).ok_or(BAD)?;
        file.seek(SeekFrom::Start(at)).map_err(|_| BAD)?;
    }
    checkpoint(stop)?;
    Ok(hash.finalize().into())
}

fn patch_type(
    item: &BoxBytes,
    node: &Mp4Box,
    patches: &mut Vec<Patch>,
    budget: &mut Budget,
) -> Result<(), ConversionError> {
    let at = item
        .at
        .checked_add(node.header.offset as u64)
        .and_then(|n| n.checked_add(4))
        .ok_or(BAD)?;
    patches.push(Patch::new(at, &node.kind(), b"free", budget)?);
    Ok(())
}
fn shifted(
    item: &BoxBytes,
    offset: usize,
    width: usize,
    delta: i128,
    patches: &mut Vec<Patch>,
    budget: &mut Budget,
) -> Result<(), ConversionError> {
    let before = number(&item.data, offset, width)?;
    let after = u64::try_from(i128::from(before).checked_add(delta).ok_or(OVERFLOW)?)
        .map_err(|_| OVERFLOW)?;
    let encoded = if width == 4 {
        u32::try_from(after)
            .map_err(|_| OVERFLOW)?
            .to_be_bytes()
            .to_vec()
    } else {
        after.to_be_bytes().to_vec()
    };
    if after != before {
        patches.push(Patch::new(
            item.at.checked_add(offset as u64).ok_or(BAD)?,
            &item.data[offset..offset + width],
            &encoded,
            budget,
        )?);
    }
    Ok(())
}

fn prepare_movie(
    item: &BoxBytes,
    patches: &mut Vec<Patch>,
    budget: &mut Budget,
) -> Result<Track, ConversionError> {
    let nodes = parse(&item.data, budget)?;
    let track = identity(&item.data, &nodes, true)?;
    let claim = super::super::audio_presentation::guarded_claim(&item.data)?;
    let mut edits = 0;
    for node in mp4::walk(&nodes) {
        let payload = node.header.payload(&item.data);
        match &node.kind() {
            b"edts" => {
                edits += 1;
                if edits > 1 || node.children.len() != 1 || node.children[0].kind() != *b"elst" {
                    return Err(BAD);
                }
                let elst = node.children[0].header.payload(&item.data);
                if claim.is_none() {
                    let width = version(elst)?;
                    if u32_at(elst, 4)? != 1
                        || elst.len() != 12 + 2 * width
                        || elst.get(8 + 2 * width..) != Some(&[0, 1, 0, 0])
                        || number(elst, 8 + width, width)? >> (width * 8 - 1) != 0
                    {
                        return Err(BAD);
                    }
                }
                patch_type(item, node, &mut *patches, budget)?;
            }
            b"mvhd" | b"mdhd" | b"tkhd" => {
                if node.kind() == *b"tkhd" {
                    track_version(payload)?;
                } else {
                    version(payload)?;
                }
                let width = if payload[0] == 0 { 4 } else { 8 };
                let at = if node.kind() == *b"tkhd" {
                    if width == 4 { 20 } else { 28 }
                } else if width == 4 {
                    16
                } else {
                    24
                };
                let offset = node.header.offset + node.header.header_size + at;
                if number(&item.data, offset, width)? != 0 {
                    patches.push(Patch::new(
                        item.at + offset as u64,
                        &item.data[offset..offset + width],
                        &[0; 8][..width],
                        budget,
                    )?);
                }
            }
            b"mehd" => {
                let width = version(payload)?;
                if payload.len() != 4 + width {
                    return Err(BAD);
                }
                patch_type(item, node, patches, budget)?;
            }
            b"prft" | b"tfra" | b"mfra" | b"emsg" | b"uuid" => return Err(BAD),
            _ => {}
        }
    }
    Ok(track)
}

fn fragment(
    item: &BoxBytes,
    track: &mut Track,
    first: &mut Option<u64>,
    delta: Option<i128>,
    patches: &mut Vec<Patch>,
    budget: &mut Budget,
) -> Result<(), ConversionError> {
    let nodes = parse(&item.data, budget)?;
    super::super::audio_presentation::guarded_fragment(&item.data, track.id).map_err(|_| BAD)?;
    let moof = one(&nodes, *b"moof")?;
    let traf = one(&moof.children, *b"traf")?;
    let tfdt = one(&traf.children, *b"tfdt")?;
    let payload = tfdt.header.payload(&item.data);
    let width = version(payload)?;
    if payload.len() != 4 + width {
        return Err(BAD);
    }
    let value = number(payload, 4, width)?;
    let initial = *first.get_or_insert(value);
    track.first = initial;
    shifted(
        item,
        tfdt.header.offset + tfdt.header.header_size + 4,
        width,
        delta.unwrap_or(-i128::from(initial)),
        patches,
        budget,
    )
}

pub(super) fn prepare(
    file: &BoundFile<'_>,
    budget: &mut Budget,
    stop: &Stop,
) -> Result<Plan, ConversionError> {
    let mut track = None;
    let mut patches = Vec::new();
    let mut first = None;
    let mut media = false;
    let mut pending = false;
    let mut file_type = false;
    let digest = scan(
        &mut file.reader()?,
        file.length(),
        budget,
        stop,
        |item, budget| {
            match &item.kind {
                b"ftyp" => {
                    if item.at != 0 || file_type || track.is_some() {
                        return Err(BAD);
                    }
                    file_type = true;
                }
                b"moov" => {
                    if !file_type || track.is_some() || first.is_some() || pending {
                        return Err(BAD);
                    }
                    track = Some(prepare_movie(item, &mut patches, budget)?);
                }
                b"moof" => {
                    if pending {
                        return Err(BAD);
                    }
                    pending = true;
                    fragment(
                        item,
                        track.as_mut().ok_or(BAD)?,
                        &mut first,
                        None,
                        &mut patches,
                        budget,
                    )?;
                }
                b"sidx" => {
                    if pending {
                        return Err(BAD);
                    }
                    super::super::dash::staged_index_scale(
                        &item.data,
                        item.at,
                        file.length(),
                        track.as_ref().ok_or(BAD)?.id,
                    )?;
                    patches.push(Patch::new(item.at + 4, b"sidx", b"free", budget)?);
                }
                b"mdat" => {
                    if !pending {
                        return Err(BAD);
                    }
                    pending = false;
                    media = true;
                }
                _ if pending => return Err(BAD),
                _ => {}
            }
            Ok(())
        },
    )?;
    let mut track = track.ok_or(BAD)?;
    track.first = first.ok_or(BAD)?;
    if !media || pending {
        return Err(BAD);
    }
    if track.first == 0 {
        patches.clear();
    }
    file.check()?;
    Ok(Plan {
        track,
        patches,
        digest,
    })
}

pub(super) fn initialization(
    file: &BoundFile<'_>,
    budget: &mut Budget,
    stop: &Stop,
) -> Result<Track, ConversionError> {
    let mut track = None;
    scan(
        &mut file.reader()?,
        file.length(),
        budget,
        stop,
        |item, budget| {
            match &item.kind {
                b"moov" => {
                    if track.is_some() {
                        return Err(BAD);
                    }
                    let nodes = parse(&item.data, budget)?;
                    if mp4::walk(&nodes)
                        .iter()
                        .any(|n| matches!(&n.kind(), b"edts" | b"prft" | b"tfra" | b"uuid"))
                    {
                        return Err(BAD);
                    }
                    track = Some(identity(&item.data, &nodes, false)?);
                }
                b"mdat" | b"moof" | b"sidx" => return Err(BAD),
                _ => {}
            }
            Ok(())
        },
    )?;
    file.check()?;
    track.ok_or(BAD)
}

pub(super) fn restore(
    file: &BoundFile<'_>,
    mut track: Track,
    delta: u64,
    budget: &mut Budget,
    stop: &Stop,
) -> Result<Plan, ConversionError> {
    let mut first = None;
    let mut patches = Vec::new();
    let mut media = false;
    let mut pending = false;
    let digest = scan(
        &mut file.reader()?,
        file.length(),
        budget,
        stop,
        |item, budget| {
            match &item.kind {
                b"moof" => {
                    if pending {
                        return Err(BAD);
                    }
                    pending = true;
                    fragment(
                        item,
                        &mut track,
                        &mut first,
                        Some(i128::from(delta)),
                        &mut patches,
                        budget,
                    )?;
                }
                b"sidx" => {
                    if pending {
                        return Err(BAD);
                    }
                    let scale = super::super::dash::staged_index_scale(
                        &item.data,
                        item.at,
                        file.length(),
                        track.id,
                    )?;
                    let width = version(&item.data[item.header..])?;
                    let shift = u128::from(delta) * u128::from(scale);
                    let before = number(&item.data, item.header + 12, width)?;
                    let value = (shift % u128::from(track.scale) == 0)
                        .then(|| shift / u128::from(track.scale))
                        .and_then(|n| u64::try_from(n).ok())
                        .and_then(|n| before.checked_add(n));
                    if let Some(value) = value.filter(|n| width == 8 || u32::try_from(*n).is_ok()) {
                        shifted(
                            item,
                            item.header + 12,
                            width,
                            i128::from(value) - i128::from(before),
                            &mut patches,
                            budget,
                        )?;
                    } else {
                        patches.push(Patch::new(item.at + 4, b"sidx", b"free", budget)?);
                    }
                }
                b"mdat" => {
                    if !pending {
                        return Err(BAD);
                    }
                    pending = false;
                    media = true;
                }
                b"moov" | b"ftyp" => return Err(BAD),
                _ if pending => return Err(BAD),
                _ => {}
            }
            Ok(())
        },
    )?;
    if first.is_none() || !media || pending {
        return Err(BAD);
    }
    file.check()?;
    Ok(Plan {
        track,
        patches,
        digest,
    })
}

fn fingerprint(file: &mut File, stop: &Stop) -> Result<[u8; 32], ConversionError> {
    let length = file.metadata().map_err(|_| BAD)?.len();
    scan(file, length, &mut Budget::default(), stop, |_, _| Ok(()))
}
pub(super) fn apply(
    file: &mut BoundFile<'_>,
    patches: &[Patch],
    digest: [u8; 32],
    undo: bool,
    stop: &Stop,
) -> Result<[u8; 32], ConversionError> {
    checkpoint(stop)?;
    let length = file.length();
    file.update(|writer| {
        if fingerprint(writer, stop)? != digest {
            return Err(CHANGED);
        }
        let mut completed = 0;
        let result = (|| {
            for patch in patches {
                checkpoint(stop)?;
                let (before, after) = if undo {
                    (&patch.after, &patch.before)
                } else {
                    (&patch.before, &patch.after)
                };
                let mut actual = [0; 8];
                writer.seek(SeekFrom::Start(patch.at)).map_err(|_| BAD)?;
                writer
                    .read_exact(&mut actual[..patch.length])
                    .map_err(|_| BAD)?;
                if actual[..patch.length] != before[..patch.length] {
                    return Err(CHANGED);
                }
                writer.seek(SeekFrom::Start(patch.at)).map_err(|_| BAD)?;
                writer.write_all(&after[..patch.length]).map_err(|_| BAD)?;
                completed += 1;
            }
            let digest = fingerprint(writer, stop)?;
            if writer.metadata().map_err(|_| BAD)?.len() != length {
                return Err(CHANGED);
            }
            Ok(digest)
        })();
        if result.is_err() {
            for patch in patches[..completed].iter().rev() {
                let before = if undo { &patch.after } else { &patch.before };
                if writer
                    .seek(SeekFrom::Start(patch.at))
                    .and_then(|_| writer.write_all(&before[..patch.length]))
                    .is_err()
                {
                    return Err(CHANGED);
                }
            }
        }
        result
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_large_media_payload_is_sought_over_without_reading_or_allocating_it() {
        let length = 4_u64 * 1024 * 1024 * 1024;
        let mut header = Vec::from([0, 0, 0, 1, b'm', b'd', b'a', b't']);
        header.extend(length.to_be_bytes());
        let mut source = Cursor::new(header);
        let mut budget = Budget::default();
        let mut seen = 0;
        scan(&mut source, length, &mut budget, &Stop::new(), |item, _| {
            assert_eq!(item.kind, *b"mdat");
            assert_eq!(item.data.len(), 16);
            seen += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, 1);
        assert_eq!(source.stream_position().unwrap(), length);
        assert_eq!(budget.metadata, 0);
        assert_eq!(budget.nodes, 1);
    }

    #[test]
    fn budgets_are_aggregate_and_reject_the_first_byte_node_or_patch_over_the_limit() {
        let mut budget = Budget::default();
        budget.metadata(MAX_METADATA).unwrap();
        assert_eq!(budget.metadata(1).unwrap_err(), LIMIT);
        let mut budget = Budget::default();
        budget.nodes(MAX_BOXES).unwrap();
        assert_eq!(budget.nodes(1).unwrap_err(), LIMIT);
        let mut budget = Budget::default();
        budget.retain(MAX_METADATA).unwrap();
        assert_eq!(budget.retain(1).unwrap_err(), LIMIT);
        let mut budget = Budget {
            patches: MAX_PATCHES,
            ..Budget::default()
        };
        assert_eq!(budget.patch().unwrap_err(), LIMIT);
        let mut budget = Budget {
            nodes: MAX_BOXES - 2,
            ..Budget::default()
        };
        let boxes = [0, 0, 0, 8, b'f', b'r', b'e', b'e'].repeat(3);
        assert!(matches!(parse(&boxes, &mut budget), Err(error) if error == LIMIT));
    }

    #[test]
    fn narrow_and_wide_clock_overflow_and_underflow_never_create_a_patch() {
        for (value, width, delta) in [(u64::from(u32::MAX), 4, 1), (u64::MAX, 8, 1), (0, 4, -1)] {
            let raw = value.to_be_bytes();
            let item = BoxBytes {
                at: 0,
                header: 0,
                kind: *b"tfdt",
                data: Zeroizing::new(raw[8 - width..].to_vec()),
            };
            let mut patches = Vec::new();
            assert_eq!(
                shifted(&item, 0, width, delta, &mut patches, &mut Budget::default()).unwrap_err(),
                OVERFLOW
            );
            assert!(patches.is_empty());
        }
    }

    #[test]
    fn a_failed_later_patch_rolls_back_every_preceding_metadata_write() {
        let owner = super::super::files::Stage::new(None).unwrap();
        let path = owner.path().join("rollback.mp4");
        let original = [0, 0, 0, 12, b'f', b'r', b'e', b'e', 1, 2, 3, 4];
        std::fs::write(&path, original).unwrap();
        super::super::files::seal(owner.path()).unwrap();
        let mut file = BoundFile::open(&owner, &path).unwrap();
        let digest = fingerprint(&mut file.reader().unwrap(), &Stop::new()).unwrap();
        let mut budget = Budget::default();
        let patches = [
            Patch::new(8, &[1], &[9], &mut budget).unwrap(),
            Patch::new(9, &[99], &[8], &mut budget).unwrap(),
        ];
        assert_eq!(
            apply(&mut file, &patches, digest, false, &Stop::new()).unwrap_err(),
            CHANGED
        );
        assert_eq!(std::fs::read(path).unwrap(), original);
    }

    #[test]
    fn malformed_box_sizes_unknown_clock_boxes_and_cancelled_scans_are_refused() {
        for data in [
            vec![0, 0, 0, 7, b'm', b'd', b'a', b't'],
            vec![0, 0, 0, 9, b'm', b'd', b'a', b't'],
            vec![0, 0, 0, 8, b'p', b'r', b'f', b't'],
        ] {
            let length = data.len() as u64;
            assert!(
                scan(
                    &mut Cursor::new(data),
                    length,
                    &mut Budget::default(),
                    &Stop::new(),
                    |_, _| Ok(())
                )
                .is_err()
            );
        }
        let stop = Stop::new();
        stop.set();
        assert_eq!(
            scan(
                &mut Cursor::new(Vec::<u8>::new()),
                0,
                &mut Budget::default(),
                &stop,
                |_, _| Ok(())
            )
            .unwrap_err()
            .0,
            "cancelled"
        );
    }
}
