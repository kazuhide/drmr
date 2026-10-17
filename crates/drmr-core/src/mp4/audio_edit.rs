//! Preserve a single open-ended audio presentation trim across fragmented remuxes.
//!
//! Audio encoders can retain leading decoder samples and use an edit list to trim them
//! from presentation. Remuxing can keep every compressed sample while losing that trim.
//! Restore the trim and compensate fragment decode times by the same amount, preserving
//! packet presentation times. An `FFmpeg` normalization gap is folded into that compensation.
//! Only rate-one, open-ended trims are supported; finite or multiple presentation intervals,
//! fractional scale conversion, absolute media offsets, and timestamp overflow are refused.

use super::{Mp4Box, Mp4Error, parse_boxes, walk};
use std::path::Path;

const UNSUPPORTED: Mp4Error = Mp4Error("unsupported-audio-edit-list");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AudioEdit {
    media_time: u64,
    timescale: u32,
}

fn child(node: &Mp4Box, kind: [u8; 4]) -> Result<&Mp4Box, Mp4Error> {
    let mut matching = node.children.iter().filter(|node| node.kind() == kind);
    let found = matching.next().ok_or(UNSUPPORTED)?;
    if matching.next().is_some() {
        return Err(UNSUPPORTED);
    }
    Ok(found)
}

fn scale(node: &Mp4Box, data: &[u8]) -> Result<u32, Mp4Error> {
    let payload = node.header.payload(data);
    let offset = match payload.first() {
        Some(0) => 12,
        Some(1) => 20,
        _ => return Err(UNSUPPORTED),
    };
    let value = u32::from_be_bytes(
        payload
            .get(offset..offset + 4)
            .ok_or(UNSUPPORTED)?
            .try_into()
            .unwrap(),
    );
    if value == 0 {
        return Err(UNSUPPORTED);
    }
    Ok(value)
}

fn edits(track: &Mp4Box, data: &[u8]) -> Result<Vec<(u64, i64)>, Mp4Error> {
    let mut matching = track.children.iter().filter(|node| node.kind() == *b"edts");
    let Some(edts) = matching.next() else {
        return Ok(Vec::new());
    };
    if matching.next().is_some() || edts.children.len() != 1 {
        return Err(UNSUPPORTED);
    }
    let payload = child(edts, *b"elst")?.header.payload(data);
    let version = *payload.first().ok_or(UNSUPPORTED)?;
    let width = match version {
        0 => 12,
        1 => 20,
        _ => return Err(UNSUPPORTED),
    };
    let count =
        u32::from_be_bytes(payload.get(4..8).ok_or(UNSUPPORTED)?.try_into().unwrap()) as usize;
    if count == 0 || count > 2 || payload.len() != 8 + count * width || payload[1..4] != [0, 0, 0] {
        return Err(UNSUPPORTED);
    }
    let mut out = Vec::new();
    for entry in payload[8..].chunks_exact(width) {
        if entry[width - 4..] != [0, 1, 0, 0] {
            return Err(UNSUPPORTED);
        }
        out.push(if version == 0 {
            (
                u64::from(u32::from_be_bytes(entry[..4].try_into().unwrap())),
                i64::from(i32::from_be_bytes(entry[4..8].try_into().unwrap())),
            )
        } else {
            (
                u64::from_be_bytes(entry[..8].try_into().unwrap()),
                i64::from_be_bytes(entry[8..16].try_into().unwrap()),
            )
        });
    }
    Ok(out)
}

impl AudioEdit {
    pub(crate) fn read(
        path: &Path,
        stream_index: i64,
    ) -> Result<Option<Self>, crate::media::MediaError> {
        Self::from_bytes(&std::fs::read(path)?, stream_index).map_err(Into::into)
    }

    fn from_bytes(data: &[u8], stream_index: i64) -> Result<Option<Self>, Mp4Error> {
        // Transport streams have no MP4 edit list. MP4 inputs have already been probed.
        if !matches!(data.get(4..8), Some(b"ftyp" | b"moov")) {
            return Ok(None);
        }
        let boxes = parse_boxes(data)?;
        let moov = boxes
            .iter()
            .find(|node| node.kind() == *b"moov")
            .ok_or(UNSUPPORTED)?;
        let track = moov
            .children
            .iter()
            .filter(|node| node.kind() == *b"trak")
            .nth(usize::try_from(stream_index).map_err(|_| UNSUPPORTED)?)
            .ok_or(UNSUPPORTED)?;
        let mdia = child(track, *b"mdia")?;
        if child(mdia, *b"hdlr")?.header.payload(data).get(8..12) != Some(b"soun") {
            return Ok(None);
        }
        let list = edits(track, data)?;
        if list.is_empty() {
            return Ok(None);
        }
        let [(0, media_time)] = list.as_slice() else {
            return Err(UNSUPPORTED);
        };
        let media_time = u64::try_from(*media_time).map_err(|_| UNSUPPORTED)?;
        if media_time == 0 {
            return Ok(None);
        }
        Ok(Some(Self {
            media_time,
            timescale: scale(child(mdia, *b"mdhd")?, data)?,
        }))
    }

    /// Replace the trim and compensate decode times, keeping presentation timestamps fixed.
    /// Fragment-relative offsets stay valid because moof sizes do not change. Optional
    /// random-access indexes are omitted; absolute media offsets are rejected.
    pub(crate) fn restore(
        &self,
        init: &Path,
        segments: &[std::path::PathBuf],
    ) -> Result<(), crate::media::MediaError> {
        let data = std::fs::read(init)?;
        let (rewritten, shift) = self.rewrite_init(&data)?;
        let mut rewritten_segments = Vec::new();
        for path in segments {
            let mut bytes = without_indexes(&std::fs::read(path)?)?;
            shift_decode_times(&mut bytes, shift)?;
            rewritten_segments.push((path, bytes));
        }
        std::fs::write(init, rewritten)?;
        for (path, bytes) in rewritten_segments {
            std::fs::write(path, bytes)?;
        }
        Ok(())
    }

    fn rewrite_init(&self, data: &[u8]) -> Result<(Vec<u8>, i128), Mp4Error> {
        let boxes = parse_boxes(data)?;
        let moov = boxes
            .iter()
            .find(|node| node.kind() == *b"moov")
            .ok_or(UNSUPPORTED)?;
        let tracks: Vec<_> = moov
            .children
            .iter()
            .filter(|node| node.kind() == *b"trak")
            .collect();
        let [track] = tracks.as_slice() else {
            return Err(UNSUPPORTED);
        };
        let mdia = child(track, *b"mdia")?;
        if scale(child(mdia, *b"mdhd")?, data)? != self.timescale {
            return Err(UNSUPPORTED);
        }
        child(moov, *b"mvex")?;
        let movie_scale = scale(child(moov, *b"mvhd")?, data)?;
        let list = edits(track, data)?;
        let (gap, time) = match list.as_slice() {
            [] => (0, 0),
            [(0, time)] if *time >= 0 => (0, *time),
            [(gap, -1), (0, time)] if *time >= 0 => (*gap, *time),
            _ => return Err(UNSUPPORTED),
        };
        let gap_ticks = i128::from(gap) * i128::from(self.timescale);
        if gap_ticks % i128::from(movie_scale) != 0 {
            return Err(UNSUPPORTED);
        }
        let shift =
            gap_ticks / i128::from(movie_scale) - i128::from(time) + i128::from(self.media_time);
        let edit = self.edit_box();
        let mut out = rewrite(data, &boxes, &edit)?;
        shift_decode_times(&mut out, shift)?;
        Ok((out, shift))
    }

    fn edit_box(self) -> Vec<u8> {
        let mut payload = vec![1, 0, 0, 0];
        payload.extend(1u32.to_be_bytes());
        payload.extend(0u64.to_be_bytes());
        payload.extend(self.media_time.to_be_bytes());
        payload.extend([0, 1, 0, 0]);
        boxed(*b"edts", &boxed(*b"elst", &payload))
    }
}

fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(payload.len() + 8)
        .unwrap()
        .to_be_bytes()
        .to_vec();
    out.extend(kind);
    out.extend(payload);
    out
}

fn rewrite(data: &[u8], nodes: &[Mp4Box], edit: &[u8]) -> Result<Vec<u8>, Mp4Error> {
    let mut out = Vec::new();
    for node in nodes {
        let kind = node.kind();
        if matches!(&kind, b"edts" | b"mfra" | b"sidx" | b"ssix") {
            continue;
        }
        if matches!(&kind, b"moov" | b"trak") {
            if node.header.header_size != 8 {
                return Err(UNSUPPORTED);
            }
            let mut payload = rewrite(data, &node.children, edit)?;
            if kind == *b"trak" {
                payload.extend(edit);
            }
            if u32::try_from(payload.len().saturating_add(8)).is_err() {
                return Err(UNSUPPORTED);
            }
            out.extend(boxed(kind, &payload));
        } else {
            out.extend(node.header.bytes(data));
        }
    }
    Ok(out)
}

// Optional indexes can contain absolute offsets or pre-edit times. External manifests
// locate these fragments; omit indexes rather than leave stale seek targets behind.
fn without_indexes(data: &[u8]) -> Result<Vec<u8>, Mp4Error> {
    let boxes = parse_boxes(data)?;
    Ok(boxes
        .iter()
        .filter(|node| !matches!(&node.kind(), b"mfra" | b"sidx" | b"ssix"))
        .flat_map(|node| node.header.bytes(data).iter().copied())
        .collect())
}

fn shift_decode_times(data: &mut [u8], shift: i128) -> Result<(), Mp4Error> {
    let boxes = parse_boxes(data)?;
    for node in walk(&boxes) {
        let payload = node.header.payload(data);
        if node.kind() == *b"tfhd" && payload.get(3).is_none_or(|flags| flags & 1 != 0) {
            return Err(UNSUPPORTED);
        }
        if matches!(&node.kind(), b"stco" | b"co64") && payload.get(4..8) != Some(&[0, 0, 0, 0]) {
            return Err(UNSUPPORTED);
        }
        if node.kind() != *b"tfdt" {
            continue;
        }
        let at = node.header.offset + node.header.header_size + 4;
        match payload.first() {
            Some(0) => {
                let value = u32::from_be_bytes(
                    data.get(at..at + 4).ok_or(UNSUPPORTED)?.try_into().unwrap(),
                );
                let adjusted = u32::try_from(i128::from(value) + shift).map_err(|_| UNSUPPORTED)?;
                data[at..at + 4].copy_from_slice(&adjusted.to_be_bytes());
            }
            Some(1) => {
                let value = u64::from_be_bytes(
                    data.get(at..at + 8).ok_or(UNSUPPORTED)?.try_into().unwrap(),
                );
                let adjusted = u64::try_from(i128::from(value) + shift).map_err(|_| UNSUPPORTED)?;
                data[at..at + 8].copy_from_slice(&adjusted.to_be_bytes());
            }
            _ => return Err(UNSUPPORTED),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(kind: [u8; 4], timescale: u32) -> Vec<u8> {
        let mut payload = vec![0; 20];
        payload[12..16].copy_from_slice(&timescale.to_be_bytes());
        boxed(kind, &payload)
    }

    fn movie(edit: &[u8]) -> Vec<u8> {
        let mut handler = vec![0; 12];
        handler[8..12].copy_from_slice(b"soun");
        let mdia = boxed(
            *b"mdia",
            &[header(*b"mdhd", 48000), boxed(*b"hdlr", &handler)].concat(),
        );
        let track = boxed(*b"trak", &[mdia, edit.to_vec()].concat());
        [
            boxed(*b"ftyp", b"isom"),
            boxed(
                *b"moov",
                &[header(*b"mvhd", 48000), track, boxed(*b"mvex", &[])].concat(),
            ),
        ]
        .concat()
    }

    fn fragment(version: u8, decode_time: u64) -> Vec<u8> {
        let mut payload = vec![version, 0, 0, 0];
        if version == 0 {
            payload.extend(u32::try_from(decode_time).unwrap().to_be_bytes());
        } else {
            payload.extend(decode_time.to_be_bytes());
        }
        boxed(*b"moof", &boxed(*b"traf", &boxed(*b"tfdt", &payload)))
    }

    #[test]
    fn restoring_a_trim_keeps_presentations_fixed_and_changes_no_media_bytes() {
        let edit = AudioEdit {
            media_time: 1024,
            timescale: 48000,
        };
        let source = movie(&edit.edit_box());
        assert_eq!(AudioEdit::from_bytes(&source, 0).unwrap(), Some(edit));
        for version in [0, 1] {
            let target = [
                movie(&[]),
                fragment(version, 94976),
                boxed(*b"mdat", b"unchanged AAC bytes"),
            ]
            .concat();
            let (restored, shift) = edit.rewrite_init(&target).unwrap();
            assert_eq!(shift, 1024);
            assert_eq!(AudioEdit::from_bytes(&restored, 0).unwrap(), Some(edit));
            let boxes = parse_boxes(&restored).unwrap();
            let tfdt = walk(&boxes)
                .into_iter()
                .find(|node| node.kind() == *b"tfdt")
                .unwrap();
            assert_eq!(
                super::super::Tfdt::read(&restored, &tfdt.header)
                    .unwrap()
                    .base_media_decode_time,
                96000
            );
            assert!(restored.ends_with(&boxed(*b"mdat", b"unchanged AAC bytes")));
            assert_eq!(edit.rewrite_init(&restored).unwrap().0, restored);
        }
    }

    #[test]
    fn normalization_gap_is_folded_into_decode_time_before_restoring_trim() {
        let edit = AudioEdit {
            media_time: 1024,
            timescale: 48000,
        };
        let mut payload = vec![0, 0, 0, 0];
        payload.extend(2u32.to_be_bytes());
        payload.extend(94976u32.to_be_bytes());
        payload.extend((-1i32).to_be_bytes());
        payload.extend([0, 1, 0, 0]);
        payload.extend([0; 8]);
        payload.extend([0, 1, 0, 0]);
        let target = [
            movie(&boxed(*b"edts", &boxed(*b"elst", &payload))),
            fragment(1, 0),
        ]
        .concat();
        let (restored, shift) = edit.rewrite_init(&target).unwrap();
        assert_eq!(shift, 96000);
        assert_eq!(AudioEdit::from_bytes(&restored, 0).unwrap(), Some(edit));
    }

    #[test]
    fn unsupported_edits_and_decode_time_overflow_fail_explicitly() {
        let edit = AudioEdit {
            media_time: 1024,
            timescale: 48000,
        };
        let mut complex = edit.edit_box();
        complex[31] = 1; // A finite edit is not an open-ended priming trim.
        assert_eq!(AudioEdit::from_bytes(&movie(&complex), 0), Err(UNSUPPORTED));
        let target = [movie(&[]), fragment(0, u64::from(u32::MAX))].concat();
        assert_eq!(edit.rewrite_init(&target), Err(UNSUPPORTED));
    }

    fn indexed_target() -> Vec<u8> {
        let movie = movie(&[]);
        let fragment = [fragment(0, 94976), boxed(*b"mdat", b"AAC payload")].concat();
        let mut index = vec![0; 4];
        index.extend(1u32.to_be_bytes());
        index.extend(48000u32.to_be_bytes());
        index.extend(94976u32.to_be_bytes());
        index.extend(u32::try_from(movie.len()).unwrap().to_be_bytes());
        index.extend([0, 0, 0, 1]);
        index.extend(u32::try_from(fragment.len()).unwrap().to_be_bytes());
        index.extend(1024u32.to_be_bytes());
        index.extend(0x9000_0000u32.to_be_bytes());
        let index = boxed(*b"sidx", &index);
        let mut tfra = vec![1, 0, 0, 0];
        tfra.extend(1u32.to_be_bytes());
        tfra.extend(0u32.to_be_bytes());
        tfra.extend(1u32.to_be_bytes());
        tfra.extend(94976u64.to_be_bytes());
        tfra.extend(
            u64::try_from(index.len() + movie.len())
                .unwrap()
                .to_be_bytes(),
        );
        tfra.extend([1, 1, 1]);
        let tfra = boxed(*b"tfra", &tfra);
        let mfra_size = u32::try_from(8 + tfra.len() + 16).unwrap();
        let footer = boxed(
            *b"mfro",
            &[vec![0; 4], mfra_size.to_be_bytes().to_vec()].concat(),
        );
        let mfra = boxed(*b"mfra", &[tfra, footer].concat());
        [index, movie, fragment, mfra].concat()
    }

    fn assert_index_offsets_are_valid(data: &[u8]) {
        let boxes = parse_boxes(data).unwrap();
        for node in walk(&boxes) {
            let payload = node.header.payload(data);
            let offset = match &node.kind() {
                b"tfra" => usize::try_from(u64::from_be_bytes(payload[24..32].try_into().unwrap()))
                    .unwrap(),
                b"sidx" => {
                    node.header.offset
                        + node.header.size
                        + usize::try_from(u32::from_be_bytes(payload[16..20].try_into().unwrap()))
                            .unwrap()
                }
                _ => continue,
            };
            assert_eq!(
                data.get(offset + 4..offset + 8),
                Some(b"moof".as_slice()),
                "an index must point at the actual moof offset"
            );
        }
    }

    #[test]
    fn rewritten_audio_never_retains_stale_random_access_indexes() {
        let edit = AudioEdit {
            media_time: 1024,
            timescale: 48000,
        };
        let target = indexed_target();
        assert_index_offsets_are_valid(&target);
        let (restored, _) = edit.rewrite_init(&target).unwrap();
        assert_index_offsets_are_valid(&restored);
        let boxes = parse_boxes(&restored).unwrap();
        assert!(
            walk(&boxes)
                .iter()
                .all(|node| !matches!(&node.kind(), b"mfra" | b"tfra" | b"sidx" | b"ssix"))
        );
        assert!(
            restored
                .windows(b"AAC payload".len())
                .any(|bytes| bytes == b"AAC payload")
        );

        let directory = tempfile::tempdir().unwrap();
        let init = directory.path().join("init.mp4");
        let segment = directory.path().join("segment.m4s");
        std::fs::write(&init, movie(&[])).unwrap();
        // A separately stored segment can carry its own relative segment index.
        let target_boxes = parse_boxes(&target).unwrap();
        let mut bytes = target_boxes[0].header.bytes(&target).to_vec();
        bytes[24..28].copy_from_slice(&0u32.to_be_bytes());
        for node in target_boxes
            .iter()
            .filter(|node| matches!(&node.kind(), b"moof" | b"mdat"))
        {
            bytes.extend(node.header.bytes(&target));
        }
        bytes.extend(boxed(*b"ssix", &[0; 8]));
        assert_index_offsets_are_valid(&bytes);
        std::fs::write(&segment, bytes).unwrap();
        edit.restore(&init, std::slice::from_ref(&segment)).unwrap();
        let bytes = std::fs::read(segment).unwrap();
        assert!(
            walk(&parse_boxes(&bytes).unwrap())
                .iter()
                .all(|node| !matches!(&node.kind(), b"mfra" | b"tfra" | b"sidx" | b"ssix"))
        );
    }
}
