//! Strict admission for the complete progressive counter-mode encryption profile.
use super::{ConversionError, InputKeys, audio};
use crate::{
    inspection::{Inspection, Protection, TrackKind},
    mp4::{self, Mp4Box},
};
use std::collections::BTreeSet;
const UNSUPPORTED: ConversionError = ConversionError("progressive-cenc-profile-not-supported");
fn child(node: &Mp4Box, kind: [u8; 4]) -> Result<&Mp4Box, ConversionError> {
    let mut nodes = node.children.iter().filter(|n| n.kind() == kind);
    let value = nodes.next().ok_or(UNSUPPORTED)?;
    if nodes.next().is_some() {
        return Err(UNSUPPORTED);
    }
    Ok(value)
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, ConversionError> {
    bytes
        .get(at..at.checked_add(4).ok_or(UNSUPPORTED)?)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or(UNSUPPORTED)
}
fn full(bytes: &[u8], version: u8, flags: u8) -> Result<(), ConversionError> {
    if bytes.get(..4) != Some(&[version, 0, 0, flags]) {
        return Err(UNSUPPORTED);
    }
    Ok(())
}
fn mark<'a>(
    node: &'a Mp4Box,
    kind: [u8; 4],
    allowed: &mut BTreeSet<usize>,
) -> Result<&'a Mp4Box, ConversionError> {
    let value = child(node, kind)?;
    allowed.insert(value.header.offset);
    Ok(value)
}
fn sizes(node: &Mp4Box, bytes: &[u8]) -> Result<Vec<u32>, ConversionError> {
    let p = child(node, *b"stsz")?.header.payload(bytes);
    full(p, 0, 0)?;
    let default = u32_at(p, 4)?;
    let count = usize::try_from(u32_at(p, 8)?).map_err(|_| UNSUPPORTED)?;
    if count == 0 {
        return Err(UNSUPPORTED);
    }
    if default != 0 {
        if p.len() != 12 {
            return Err(UNSUPPORTED);
        }
        // A singleton retains the constant size without allocating one value per sample.
        return Ok(vec![default]);
    }
    let length = count
        .checked_mul(4)
        .and_then(|n| n.checked_add(12))
        .ok_or(UNSUPPORTED)?;
    if p.len() != length {
        return Err(UNSUPPORTED);
    }
    p[12..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|v| {
            let n = u32::from_be_bytes(*v);
            if n == 0 { Err(UNSUPPORTED) } else { Ok(n) }
        })
        .collect()
}
fn roll(
    stbl: &Mp4Box,
    data: &[u8],
    audio: bool,
    count: u32,
    allowed: &mut BTreeSet<usize>,
) -> Result<(), ConversionError> {
    let groups: Vec<_> = stbl
        .children
        .iter()
        .filter(|n| matches!(&n.kind(), b"sgpd" | b"sbgp"))
        .collect();
    if groups.is_empty() {
        return Ok(());
    }
    if !audio || groups.len() != 2 {
        return Err(UNSUPPORTED);
    }
    let sgpd = mark(stbl, *b"sgpd", allowed)?.header.payload(data);
    if sgpd
        != [
            &[1, 0, 0, 0][..],
            b"roll",
            &2u32.to_be_bytes(),
            &1u32.to_be_bytes(),
            &[255, 255],
        ]
        .concat()
    {
        return Err(UNSUPPORTED);
    }
    let sbgp = mark(stbl, *b"sbgp", allowed)?.header.payload(data);
    if sbgp
        != [
            &[0, 0, 0, 0][..],
            b"roll",
            &1u32.to_be_bytes(),
            &count.to_be_bytes(),
            &1u32.to_be_bytes(),
        ]
        .concat()
    {
        return Err(UNSUPPORTED);
    }
    Ok(())
}
fn sample_entry(
    stbl: &Mp4Box,
    actual: &crate::inspection::Track,
    data: &[u8],
    allowed: &mut BTreeSet<usize>,
) -> Result<usize, ConversionError> {
    let audio = actual.kind == TrackKind::Audio;
    let chunk_map = child(stbl, *b"stsc")?.header.payload(data);
    full(chunk_map, 0, 0)?;
    let chunks = usize::try_from(u32_at(chunk_map, 4)?).map_err(|_| UNSUPPORTED)?;
    if chunks == 0
        || chunk_map.len()
            != chunks
                .checked_mul(12)
                .and_then(|n| n.checked_add(8))
                .ok_or(UNSUPPORTED)?
    {
        return Err(UNSUPPORTED);
    }
    let mut previous = 0;
    for (i, entry) in chunk_map[8..].as_chunks::<12>().0.iter().enumerate() {
        let first = u32_at(entry, 0)?;
        if (i == 0 && first != 1)
            || first <= previous
            || u32_at(entry, 4)? == 0
            || u32_at(entry, 8)? != 1
        {
            return Err(UNSUPPORTED);
        }
        previous = first;
    }
    let descriptions = child(stbl, *b"stsd")?;
    let p = descriptions.header.payload(data);
    full(p, 0, 0)?;
    if u32_at(p, 4)? != 1 || descriptions.children.len() != 1 {
        return Err(UNSUPPORTED);
    }
    let entry = &descriptions.children[0];
    if entry.kind() != if audio { *b"enca" } else { *b"encv" } {
        return Err(UNSUPPORTED);
    }
    let sinf = mark(entry, *b"sinf", allowed)?;
    if sinf.children.len() != 3 {
        return Err(UNSUPPORTED);
    }
    if mark(sinf, *b"frma", allowed)?.header.payload(data) != if audio { b"mp4a" } else { b"avc1" }
    {
        return Err(UNSUPPORTED);
    }
    let scheme = mark(sinf, *b"schm", allowed)?.header.payload(data);
    if scheme != [0, 0, 0, 0, b'c', b'e', b'n', b'c', 0, 1, 0, 0] {
        return Err(UNSUPPORTED);
    }
    let scheme_info = mark(sinf, *b"schi", allowed)?;
    if scheme_info.children.len() != 1 {
        return Err(UNSUPPORTED);
    }
    let tenc = mark(scheme_info, *b"tenc", allowed)?.header.payload(data);
    full(tenc, 0, 0)?;
    if tenc.len() != 24
        || tenc[4..7] != [0, 0, 1]
        || !matches!(tenc[7], 8 | 16)
        || hex::encode(&tenc[8..24]) != actual.key_ids[0]
    {
        return Err(UNSUPPORTED);
    }
    Ok(usize::from(tenc[7]))
}
fn auxiliary_offset(
    stbl: &Mp4Box,
    senc: &Mp4Box,
    movie: &audio::Movie,
    allowed: &mut BTreeSet<usize>,
) -> Result<(), ConversionError> {
    let data = &movie.bytes;
    let aux_offsets = mark(stbl, *b"saio", allowed)?.header.payload(data);
    let version = *aux_offsets.first().ok_or(UNSUPPORTED)?;
    if !matches!(version, 0 | 1) {
        return Err(UNSUPPORTED);
    }
    full(aux_offsets, version, 0)?;
    let offset = if version == 0 {
        if aux_offsets.len() != 12 {
            return Err(UNSUPPORTED);
        }
        u64::from(u32_at(aux_offsets, 8)?)
    } else {
        if aux_offsets.len() != 16 {
            return Err(UNSUPPORTED);
        }
        u64::from_be_bytes(aux_offsets[8..16].try_into().unwrap())
    };
    let first = senc
        .header
        .offset
        .checked_add(senc.header.header_size)
        .and_then(|v| v.checked_add(8))
        .ok_or(UNSUPPORTED)?;
    if u32_at(aux_offsets, 4)? != 1
        || movie
            .offset
            .checked_add(u64::try_from(first).map_err(|_| UNSUPPORTED)?)
            != Some(offset)
    {
        return Err(UNSUPPORTED);
    }

    Ok(())
}
fn auxiliary_entries(
    p: &[u8],
    aux_sizes: &[u8],
    sample_sizes: &[u32],
    audio: bool,
    iv: usize,
    count: u32,
) -> Result<(), ConversionError> {
    let default = aux_sizes[4];
    let mut at = 8usize;
    for index in 0..usize::try_from(count).map_err(|_| UNSUPPORTED)? {
        let start = at;
        at = at
            .checked_add(iv)
            .filter(|n| *n <= p.len())
            .ok_or(UNSUPPORTED)?;
        let sample = if sample_sizes.len() == 1 {
            sample_sizes[0]
        } else {
            sample_sizes[index]
        };
        if !audio {
            let n = p.get(at..at + 2).ok_or(UNSUPPORTED)?;
            let n = usize::from(u16::from_be_bytes(n.try_into().unwrap()));
            at += 2;
            if n == 0 || n > p.len().saturating_sub(at) / 6 {
                return Err(UNSUPPORTED);
            }
            let mut total = 0u64;
            for _ in 0..n {
                let clear = u16::from_be_bytes(p[at..at + 2].try_into().unwrap());
                let encrypted = u32_at(p, at + 2)?;
                total = total
                    .checked_add(u64::from(clear) + u64::from(encrypted))
                    .ok_or(UNSUPPORTED)?;
                at += 6;
            }
            if total != u64::from(sample) {
                return Err(UNSUPPORTED);
            }
        }
        let encoded = at - start;
        let declared = if default == 0 {
            aux_sizes[9 + index]
        } else {
            default
        };
        if encoded != usize::from(declared) {
            return Err(UNSUPPORTED);
        }
    }
    if at != p.len() {
        return Err(UNSUPPORTED);
    }

    Ok(())
}
/// Validate all tracks before starting a tool, including unselected tracks inspected by demuxers.
pub(super) fn validate(
    movie: &audio::Movie,
    report: &Inspection,
    keys: &InputKeys,
) -> Result<(), ConversionError> {
    let data = &movie.bytes;
    let boxes = mp4::parse_boxes(data).map_err(|_| UNSUPPORTED)?;
    if movie.fragmented || movie.outside_protection || report.protection != Protection::Cenc {
        return Err(UNSUPPORTED);
    }
    let moov = boxes
        .first()
        .filter(|n| boxes.len() == 1 && n.kind() == *b"moov")
        .ok_or(UNSUPPORTED)?;
    let tracks: Vec<_> = moov
        .children
        .iter()
        .filter(|n| n.kind() == *b"trak")
        .collect();
    if tracks.len() != report.tracks.len() || tracks.is_empty() {
        return Err(UNSUPPORTED);
    }
    let mut allowed = BTreeSet::new();
    for (track, actual) in tracks.into_iter().zip(&report.tracks) {
        if actual.protection != Protection::Cenc || actual.key_ids.len() != 1 {
            return Err(UNSUPPORTED);
        }
        let stbl = child(child(child(track, *b"mdia")?, *b"minf")?, *b"stbl")?;
        let audio = actual.kind == TrackKind::Audio;
        let iv = sample_entry(stbl, actual, data, &mut allowed)?;
        // GPAC places senc directly in trak; FFmpeg places it directly in stbl.
        let enc: Vec<_> = track
            .children
            .iter()
            .chain(stbl.children.iter())
            .filter(|n| n.kind() == *b"senc")
            .collect();
        if enc.len() != 1 {
            return Err(UNSUPPORTED);
        }
        let senc = enc[0];
        allowed.insert(senc.header.offset);
        let p = senc.header.payload(data);
        full(p, 0, if audio { 0 } else { 2 })?;
        let count = u32_at(p, 4)?;
        if count == 0 {
            return Err(UNSUPPORTED);
        }
        let sample_sizes = sizes(stbl, data)?;
        let declared = u32_at(child(stbl, *b"stsz")?.header.payload(data), 8)?;
        if count != declared
            || usize::try_from(count).map_err(|_| UNSUPPORTED)? > p.len().saturating_sub(8) / iv
        {
            return Err(UNSUPPORTED);
        }
        let aux_sizes = mark(stbl, *b"saiz", &mut allowed)?.header.payload(data);
        full(aux_sizes, 0, 0)?;
        let default = *aux_sizes.get(4).ok_or(UNSUPPORTED)?;
        if u32_at(aux_sizes, 5)? != count
            || aux_sizes.len()
                != if default == 0 {
                    9 + usize::try_from(count).map_err(|_| UNSUPPORTED)?
                } else {
                    9
                }
        {
            return Err(UNSUPPORTED);
        }
        auxiliary_offset(stbl, senc, movie, &mut allowed)?;
        auxiliary_entries(p, aux_sizes, &sample_sizes, audio, iv, count)?;
        roll(stbl, data, audio, count, &mut allowed)?;
    }
    // Census every opened metadata container: no duplicate, misplaced or alternate protection/group box.
    for node in mp4::walk(&boxes) {
        if matches!(
            &node.kind(),
            b"pssh"
                | b"uuid"
                | b"sinf"
                | b"frma"
                | b"schm"
                | b"schi"
                | b"tenc"
                | b"senc"
                | b"saiz"
                | b"saio"
                | b"sgpd"
                | b"sbgp"
        ) && !allowed.contains(&node.header.offset)
        {
            return Err(UNSUPPORTED);
        }
    }
    if report.tracks.iter().any(|t| !keys.contains(&t.key_ids[0])) {
        return Err(ConversionError("input-key-required"));
    }
    Ok(())
}
/// A clear output must not retain encryption or initialization information, even for unknown systems.
pub(super) fn clear(movie: &audio::Movie) -> Result<(), ConversionError> {
    let boxes = mp4::parse_boxes(&movie.bytes).map_err(|_| UNSUPPORTED)?;
    if movie.outside_protection
        || mp4::walk(&boxes).into_iter().any(|n| {
            matches!(
                &n.kind(),
                b"pssh"
                    | b"uuid"
                    | b"encv"
                    | b"enca"
                    | b"sinf"
                    | b"tenc"
                    | b"senc"
                    | b"saiz"
                    | b"saio"
            )
        })
    {
        return Err(UNSUPPORTED);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{inspection, mp4};
    use std::path::PathBuf;
    fn fixture(width: u8) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../drmr/tests/fixtures/bbb/cenc-iv{width}.mp4"))
    }
    fn keys() -> InputKeys {
        InputKeys::new(vec![
            super::super::ContentKey::from_hex(
                "000102030405060708090a0b0c0d0e0f",
                "101112131415161718191a1b1c1d1e1f",
            )
            .unwrap(),
            super::super::ContentKey::from_hex(
                "303132333435363738393a3b3c3d3e3f",
                "202122232425262728292a2b2c2d2e2f",
            )
            .unwrap(),
        ])
        .unwrap()
    }
    #[test]
    fn every_auxiliary_header_count_offset_and_sample_extent_is_checked() {
        for width in [8, 16] {
            let path = fixture(width);
            let report = inspection::inspect(&path).unwrap();
            let movie = audio::movie(&path).unwrap();
            validate(&movie, &report, &keys()).unwrap();
            let boxes = mp4::parse_boxes(&movie.bytes).unwrap();
            for node in mp4::walk(&boxes) {
                let positions: Vec<usize> = match &node.kind() {
                    b"tenc" => vec![0, 1, 4, 5, 6, 7],
                    b"senc" | b"saio" => vec![0, 1, 7, node.header.payload(&movie.bytes).len() - 1],
                    b"saiz" => vec![0, 1, 4, 8],
                    b"sgpd" | b"sbgp" => vec![0, 1, node.header.payload(&movie.bytes).len() - 1],
                    _ => continue,
                };
                for position in positions {
                    let mut changed = audio::movie(&path).unwrap();
                    let at = node.header.offset + node.header.header_size + position;
                    changed.bytes[at] ^= 1;
                    // IV bytes may change without altering framing, so only the video subsample total is mutated.
                    if node.kind() == *b"senc"
                        && position == node.header.payload(&movie.bytes).len() - 1
                        && node.header.payload(&movie.bytes)[3] == 0
                    {
                        continue;
                    }
                    assert_eq!(
                        validate(&changed, &report, &keys()).unwrap_err(),
                        UNSUPPORTED,
                        "{:?} at {position}",
                        node.kind()
                    );
                }
            }
        }
    }
    #[test]
    fn metadata_plan_marks_two_key_finite_candidate_for_fresh_validation() {
        let report = inspection::inspect(&fixture(8)).unwrap();
        let plan = inspection::plan(
            report,
            inspection::TargetProtection::Clear,
            vec![inspection::OutputFormat::Mp4],
        );
        assert!(plan.execution_available);
        assert_eq!(plan.keys[0].known_key_ids.len(), 2);
        assert!(
            plan.findings
                .iter()
                .any(|f| f.code == "progressive-mp4-profile-validation-required")
        );
    }
    #[test]
    fn all_declared_key_ids_are_required() {
        let path = fixture(8);
        let report = inspection::inspect(&path).unwrap();
        assert_eq!(
            validate(
                &audio::movie(&path).unwrap(),
                &report,
                &InputKeys::default()
            )
            .unwrap_err(),
            ConversionError("input-key-required")
        );
    }
}
