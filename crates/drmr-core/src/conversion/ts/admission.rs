//! Strict transport admission before a demuxer can discard or repair input data.
use super::super::ConversionError;
use crate::{concurrency::Stop, inspection::TrackKind};
use std::{collections::BTreeMap, fs::File, io::Read, path::Path};

const BAD: ConversionError = ConversionError("ts-transport-not-supported");
const CONFIG: ConversionError = ConversionError("ts-program-or-configuration-changed");
const CLOCK: ConversionError = ConversionError("ts-clock-wrap-or-discontinuity-not-supported");

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Track {
    pub pid: u16,
    pub kind: TrackKind,
}

#[derive(Default)]
struct Section {
    bytes: Vec<u8>,
}
impl Section {
    fn push(&mut self, payload: &[u8], start: bool) -> Result<Vec<Vec<u8>>, ConversionError> {
        let mut completed = Vec::new();
        let mut data = payload;
        if start {
            let (&pointer, rest) = data.split_first().ok_or(BAD)?;
            let (tail, rest) = rest.split_at_checked(usize::from(pointer)).ok_or(BAD)?;
            if !self.bytes.is_empty() {
                self.consume(tail, &mut completed)?;
                if !self.bytes.is_empty() || completed.len() > 1 {
                    return Err(BAD);
                }
            } else if tail.iter().any(|&b| b != 0xff) {
                return Err(BAD);
            }
            data = rest;
        } else if self.bytes.is_empty() {
            if data.iter().any(|&b| b != 0xff) {
                return Err(BAD);
            }
            return Ok(completed);
        }
        self.consume(data, &mut completed)?;
        Ok(completed)
    }
    fn consume(
        &mut self,
        mut data: &[u8],
        output: &mut Vec<Vec<u8>>,
    ) -> Result<(), ConversionError> {
        while !data.is_empty() {
            if self.bytes.is_empty() && data[0] == 0xff {
                if data.iter().any(|&b| b != 0xff) {
                    return Err(BAD);
                }
                break;
            }
            let target = if self.bytes.len() < 3 {
                3
            } else {
                let size = (usize::from(self.bytes[1] & 15) << 8) | usize::from(self.bytes[2]);
                if !(9..=1021).contains(&size) {
                    return Err(BAD);
                }
                size + 3
            };
            let n = (target - self.bytes.len()).min(data.len());
            self.bytes.extend_from_slice(&data[..n]);
            if self.bytes.len() == 3 {
                let size = (usize::from(self.bytes[1] & 15) << 8) | usize::from(self.bytes[2]);
                if !(9..=1021).contains(&size) {
                    return Err(BAD);
                }
            }
            data = &data[n..];
            if self.bytes.len() == target && target > 3 {
                if crc(&self.bytes) != 0 {
                    return Err(ConversionError("ts-section-crc-mismatch"));
                }
                output.push(std::mem::take(&mut self.bytes));
            }
        }
        Ok(())
    }
}

fn crc(bytes: &[u8]) -> u32 {
    let mut value = u32::MAX;
    for &byte in bytes {
        value ^= u32::from(byte) << 24;
        for _ in 0..8 {
            value = (value << 1)
                ^ if value & 0x8000_0000 != 0 {
                    0x04c1_1db7
                } else {
                    0
                };
        }
    }
    value
}
fn pid(bytes: &[u8]) -> u16 {
    (u16::from(bytes[0] & 31) << 8) | u16::from(bytes[1])
}
fn section_header(data: &[u8], table: u8) -> Result<(), ConversionError> {
    if data.len() < 12
        || data[0] != table
        || data[1] & 0xf0 != if table == 0x42 { 0xf0 } else { 0xb0 }
        || data[5] & 0xc1 != 0xc1
        || data[6] != 0
        || data[7] != 0
    {
        return Err(BAD);
    }
    Ok(())
}

#[derive(Default)]
struct Pes {
    header: Vec<u8>,
    waiting: bool,
    previous_dts: Option<u64>,
    active: bool,
    consumed: usize,
    declared: Option<usize>,
    seen: bool,
}
impl Pes {
    fn push(&mut self, data: &[u8], start: bool, kind: TrackKind) -> Result<(), ConversionError> {
        if start {
            if self.waiting || self.declared.is_some_and(|n| self.consumed != n) {
                return Err(BAD);
            }
            self.header.clear();
            self.waiting = true;
            self.active = true;
            self.consumed = 0;
            self.declared = None;
        }
        if !self.active {
            return Err(BAD);
        }
        self.consumed = self.consumed.checked_add(data.len()).ok_or(BAD)?;
        if self.declared.is_some_and(|n| self.consumed > n) {
            return Err(BAD);
        }
        if !self.waiting {
            return Ok(());
        }
        let n = (264 - self.header.len()).min(data.len());
        self.header.extend_from_slice(&data[..n]);
        if self.header.len() < 9 {
            return Ok(());
        }
        let head = &self.header;
        if head[..3] != [0, 0, 1]
            || head[6] & 0xf0 != 0x80
            || !match kind {
                TrackKind::Video => (0xe0..=0xef).contains(&head[3]),
                TrackKind::Audio => (0xc0..=0xdf).contains(&head[3]),
                _ => false,
            }
        {
            return Err(BAD);
        }
        let size = usize::from(u16::from_be_bytes([head[4], head[5]]));
        if size == 0 && kind != TrackKind::Video {
            return Err(BAD);
        }
        self.declared = (size != 0).then_some(size + 6);
        if self.declared.is_some_and(|n| self.consumed > n) {
            return Err(BAD);
        }
        let flags = head[7] >> 6;
        if !matches!(flags, 2 | 3) {
            return Err(ConversionError("ts-pes-timestamps-required"));
        }
        // Unsupported optional PES metadata must not disappear during remuxing.
        if head[7] & 0x3f != 0 {
            return Err(ConversionError("ts-pes-metadata-not-supported"));
        }
        let length = usize::from(head[8]);
        let required = if flags == 3 { 10 } else { 5 };
        if length < required {
            return Err(BAD);
        }
        if head.len() < 9 + length {
            return Ok(());
        }
        if head[9 + required..9 + length].iter().any(|&b| b != 0xff) {
            return Err(BAD);
        }
        let pts = timestamp(&head[9..14], if flags == 3 { 3 } else { 2 })?;
        let dts = if flags == 3 {
            timestamp(&head[14..19], 1)?
        } else {
            pts
        };
        if pts < dts || self.previous_dts.is_some_and(|old| dts <= old) {
            return Err(CLOCK);
        }
        self.previous_dts = Some(dts);
        self.seen = true;
        self.waiting = false;
        self.header.clear();
        Ok(())
    }
}
fn timestamp(data: &[u8], prefix: u8) -> Result<u64, ConversionError> {
    if data[0] >> 4 != prefix || data[0] & 1 != 1 || data[2] & 1 != 1 || data[4] & 1 != 1 {
        return Err(BAD);
    }
    Ok((u64::from(data[0] & 0x0e) << 29)
        | (u64::from(data[1]) << 22)
        | (u64::from(data[2] & 0xfe) << 14)
        | (u64::from(data[3]) << 7)
        | u64::from(data[4] >> 1))
}

/// State spans all resources: segment boundaries cannot reset counters or the program.
#[derive(Default)]
pub(in crate::conversion) struct Admission {
    counters: BTreeMap<u16, u8>,
    sections: BTreeMap<u16, Section>,
    pat: Option<Vec<u8>>,
    pmt: Option<Vec<u8>>,
    pmt_pid: Option<u16>,
    program: Option<u16>,
    pcr_pid: Option<u16>,
    pcr: Option<u64>,
    pub tracks: Vec<Track>,
    pes: BTreeMap<u16, Pes>,
    segment_pat: bool,
    segment_pmt: bool,
}
impl Admission {
    pub fn segment(&mut self, path: &Path, stop: &Stop) -> Result<(), ConversionError> {
        self.segment_pat = false;
        self.segment_pmt = false;
        let mut file = File::open(path).map_err(|_| BAD)?;
        let size = file.metadata().map_err(|_| BAD)?.len();
        if size == 0 || size % 188 != 0 {
            return Err(BAD);
        }
        let mut reader = std::io::BufReader::new(&mut file);
        let mut packet = [0; 188];
        for _ in 0..size / 188 {
            if stop.is_set() {
                return Err(ConversionError("cancelled"));
            }
            reader.read_exact(&mut packet).map_err(|_| BAD)?;
            self.packet(&packet)?;
        }
        if !self.segment_pat
            || !self.segment_pmt
            || self.sections.values().any(|s| !s.bytes.is_empty())
            || self
                .pes
                .values()
                .any(|p| p.waiting || p.declared.is_some_and(|n| p.consumed != n))
        {
            return Err(BAD);
        }
        Ok(())
    }
    fn packet(&mut self, bytes: &[u8; 188]) -> Result<(), ConversionError> {
        if bytes[0] != 0x47 || bytes[1] & 0xa0 != 0 || bytes[3] & 0xc0 != 0 {
            return Err(BAD);
        }
        let id = pid(&bytes[1..3]);
        if id == 0x1fff {
            return Ok(());
        }
        let mode = (bytes[3] >> 4) & 3;
        if mode == 0 {
            return Err(BAD);
        }
        let mut at = 4;
        if mode & 2 != 0 {
            let size = usize::from(bytes[4]);
            at = 5 + size;
            if at > 188 || mode == 2 && at != 188 {
                return Err(BAD);
            }
            if size > 0 {
                let flags = bytes[5];
                if flags & 0x80 != 0 {
                    return Err(CLOCK);
                }
                if flags & 0x0f != 0 {
                    return Err(ConversionError("ts-adaptation-metadata-not-supported"));
                }
                if flags & 0x10 != 0 {
                    if size < 7 || self.pcr_pid != Some(id) {
                        return Err(BAD);
                    }
                    let p = &bytes[6..12];
                    if p[4] & 0x7e != 0x7e || ((u16::from(p[4] & 1) << 8) | u16::from(p[5])) >= 300
                    {
                        return Err(BAD);
                    }
                    let base = (u64::from(p[0]) << 25)
                        | (u64::from(p[1]) << 17)
                        | (u64::from(p[2]) << 9)
                        | (u64::from(p[3]) << 1)
                        | u64::from(p[4] >> 7);
                    let clock = base * 300 + ((u64::from(p[4] & 1) << 8) | u64::from(p[5]));
                    if self.pcr.is_some_and(|old| clock < old) {
                        return Err(CLOCK);
                    }
                    self.pcr = Some(clock);
                }
                let stuffing = 6 + if flags & 0x10 != 0 { 6 } else { 0 };
                if bytes
                    .get(stuffing..at)
                    .is_none_or(|s| s.iter().any(|&b| b != 0xff))
                {
                    return Err(BAD);
                }
            }
        }
        if mode & 1 == 0 {
            if self
                .counters
                .get(&id)
                .is_some_and(|&old| old != bytes[3] & 15)
            {
                return Err(ConversionError("ts-continuity-mismatch"));
            }
            if !self.tracks.iter().any(|t| t.pid == id) {
                return Err(ConversionError("ts-undeclared-stream-not-supported"));
            }
            return Ok(());
        }
        if at >= 188 {
            return Err(BAD);
        }
        let counter = bytes[3] & 15;
        if self
            .counters
            .insert(id, counter)
            .is_some_and(|old| (old + 1) & 15 != counter)
        {
            return Err(ConversionError("ts-continuity-mismatch"));
        }
        let start = bytes[1] & 0x40 != 0;
        if id == 0 || id == 0x11 || self.pmt_pid == Some(id) {
            for section in self
                .sections
                .entry(id)
                .or_default()
                .push(&bytes[at..], start)?
            {
                self.section(id, section)?;
            }
        } else if let Some(track) = self.tracks.iter().find(|t| t.pid == id) {
            self.pes
                .entry(id)
                .or_default()
                .push(&bytes[at..], start, track.kind)?;
        } else {
            return Err(ConversionError("ts-undeclared-stream-not-supported"));
        }
        Ok(())
    }
    fn section(&mut self, id: u16, data: Vec<u8>) -> Result<(), ConversionError> {
        if id == 0x11 {
            section_header(&data, 0x42)?;
            return Ok(()); // DVB service descriptions do not select presentation tracks.
        }
        if id == 0 {
            section_header(&data, 0)?;
            if data.len() != 16 {
                return Err(ConversionError("ts-single-program-required"));
            }
            let program = u16::from_be_bytes([data[8], data[9]]);
            let pmt = pid(&data[10..12]);
            if program == 0 || data[10] & 0xe0 != 0xe0 || !(0x20..0x1fff).contains(&pmt) {
                return Err(BAD);
            }
            if self.pat.as_ref().is_some_and(|old| *old != data) {
                return Err(CONFIG);
            }
            self.program = Some(program);
            self.pmt_pid = Some(pmt);
            self.pat = Some(data);
            self.segment_pat = true;
        } else {
            section_header(&data, 2)?;
            if data.len() < 16 || Some(u16::from_be_bytes([data[3], data[4]])) != self.program {
                return Err(BAD);
            }
            if self.pmt.as_ref().is_some_and(|old| *old != data) {
                return Err(CONFIG);
            }
            let pcr = pid(&data[8..10]);
            if data[8] & 0xe0 != 0xe0 || data[10] & 0xf0 != 0xf0 {
                return Err(BAD);
            }
            if data[10] & 15 != 0 || data[11] != 0 {
                return Err(ConversionError("ts-program-descriptors-not-supported"));
            }
            let mut at = 12;
            let mut tracks = Vec::new();
            while at < data.len() - 4 {
                let head = data.get(at..at + 5).ok_or(BAD)?;
                at += 5;
                let kind = match head[0] {
                    0x1b => TrackKind::Video,
                    0x0f => TrackKind::Audio,
                    _ => return Err(ConversionError("ts-stream-type-not-supported")),
                };
                let id = pid(&head[1..3]);
                if head[1] & 0xe0 != 0xe0 || head[3] & 0xf0 != 0xf0 {
                    return Err(BAD);
                }
                if !(0x20..0x1fff).contains(&id)
                    || Some(id) == self.pmt_pid
                    || tracks.iter().any(|t: &Track| t.pid == id || t.kind == kind)
                {
                    return Err(BAD);
                }
                let n = (usize::from(head[3] & 15) << 8) | usize::from(head[4]);
                let descriptors = data.get(at..at + n).ok_or(BAD)?;
                at += n;
                if !descriptors.is_empty() && descriptors != b"\x0a\x04und\x00" {
                    return Err(ConversionError("ts-stream-descriptors-not-supported"));
                }
                tracks.push(Track { pid: id, kind });
            }
            if at != data.len() - 4
                || tracks.is_empty()
                || tracks.len() > 2
                || !tracks.iter().any(|t| t.pid == pcr)
            {
                return Err(BAD);
            }
            self.tracks = tracks;
            self.pcr_pid = Some(pcr);
            self.pmt = Some(data);
            self.segment_pmt = true;
        }
        Ok(())
    }
    pub fn finish(&self) -> Result<(), ConversionError> {
        if self.tracks.is_empty()
            || self.pcr.is_none()
            || self
                .tracks
                .iter()
                .any(|t| !self.pes.get(&t.pid).is_some_and(|p| p.seen))
        {
            return Err(BAD);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn section(mut bytes: Vec<u8>) -> Vec<u8> {
        let n = bytes.len() + 1;
        bytes[1] = (bytes[1] & 0xf0) | u8::try_from(n >> 8).unwrap();
        bytes[2] = u8::try_from(n & 255).unwrap();
        bytes.extend(crc(&bytes).to_be_bytes());
        bytes
    }
    fn pat() -> Vec<u8> {
        section(vec![0, 0xb0, 0, 0, 1, 0xc1, 0, 0, 0, 1, 0xf0, 0])
    }
    fn pmt() -> Vec<u8> {
        section(vec![
            2, 0xb0, 0, 0, 1, 0xc1, 0, 0, 0xe1, 0, 0xf0, 0, 0x1b, 0xe1, 0, 0xf0, 0,
        ])
    }
    #[test]
    fn psi_sections_require_crc_and_complete_bounded_reassembly() {
        let valid = pat();
        let mut parser = Section::default();
        let mut first = vec![0];
        first.extend(&valid[..8]);
        assert_eq!(parser.push(&first, true).unwrap(), [] as [Vec<u8>; 0]);
        assert_eq!(
            parser.push(&valid[8..], false).unwrap().as_slice(),
            std::slice::from_ref(&valid)
        );
        let mut bad = valid;
        *bad.last_mut().unwrap() ^= 1;
        let mut input = vec![0];
        input.extend(bad);
        assert_eq!(
            Section::default().push(&input, true).unwrap_err(),
            ConversionError("ts-section-crc-mismatch")
        );
        assert!(Section::default().push(&[0, 0, 0xbf, 0xff], true).is_err());
        assert!(Section::default().push(&[2, 0, 0], true).is_err());
    }
    #[test]
    fn additional_programs_unknown_media_and_configuration_changes_are_not_ignored() {
        let mut state = Admission::default();
        state.section(0, pat()).unwrap();
        state.section(4096, pmt()).unwrap();
        let mut changed = pmt();
        changed[5] ^= 2;
        assert_eq!(state.section(4096, changed).unwrap_err(), CONFIG);
        let mut multi = pat();
        multi.truncate(multi.len() - 4);
        multi.extend([0, 2, 0xf0, 1]);
        assert_eq!(
            Admission::default().section(0, section(multi)).unwrap_err(),
            ConversionError("ts-single-program-required")
        );
        let mut unsupported = pmt();
        unsupported[12] = 0x15;
        let mut state = Admission::default();
        state.section(0, pat()).unwrap();
        assert_eq!(
            state.section(4096, unsupported).unwrap_err(),
            ConversionError("ts-stream-type-not-supported")
        );
    }
    #[test]
    fn continuity_checks_also_cover_repeated_system_tables() {
        let sdt = section(vec![0x42, 0xf0, 0, 0, 1, 0xc1, 0, 0, 0, 1, 0xff]);
        let mut packet = [0xff; 188];
        packet[..5].copy_from_slice(&[0x47, 0x40, 0x11, 0x10, 0]);
        packet[5..5 + sdt.len()].copy_from_slice(&sdt);
        let mut state = Admission::default();
        state.packet(&packet).unwrap();
        assert_eq!(
            state.packet(&packet).unwrap_err(),
            ConversionError("ts-continuity-mismatch")
        );
    }
    #[test]
    fn pcr_wrap_and_pes_length_errors_cannot_be_repaired_by_the_demuxer() {
        let mut state = Admission {
            tracks: vec![Track {
                pid: 256,
                kind: TrackKind::Video,
            }],
            pcr_pid: Some(256),
            pcr: Some(1000),
            ..Default::default()
        };
        let mut packet = [0xff; 188];
        packet[..6].copy_from_slice(&[0x47, 1, 0, 0x20, 183, 0x10]);
        packet[6..12].copy_from_slice(&[0, 0, 0, 0, 0x7e, 0]);
        assert_eq!(state.packet(&packet).unwrap_err(), CLOCK);
        let data = [0, 0, 1, 0xc0, 0, 1, 0x80, 0x80, 5, 0x21, 0, 1, 0, 1, 0x80];
        assert_eq!(
            Pes::default()
                .push(&data, true, TrackKind::Audio)
                .unwrap_err(),
            BAD
        );
    }
    #[test]
    fn pcr_extension_cannot_move_backwards_within_one_base_tick() {
        let mut state = Admission {
            tracks: vec![Track {
                pid: 256,
                kind: TrackKind::Video,
            }],
            pcr_pid: Some(256),
            ..Default::default()
        };
        let mut packet = [0xff; 188];
        packet[..6].copy_from_slice(&[0x47, 1, 0, 0x20, 183, 0x10]);
        packet[6..12].copy_from_slice(&[0, 0, 0, 0, 0x7e, 2]);
        state.packet(&packet).unwrap();
        packet[11] = 1;
        assert_eq!(state.packet(&packet).unwrap_err(), CLOCK);
    }

    #[test]
    fn pcr_on_the_first_pat_cannot_replace_evidence_from_the_declared_pid() {
        let mut clocked_pat = [0xff; 188];
        clocked_pat[..6].copy_from_slice(&[0x47, 0x40, 0, 0x30, 7, 0x10]);
        clocked_pat[6..12].copy_from_slice(&[0, 0, 0, 0, 0x7e, 0]);
        clocked_pat[12] = 0;
        let pat = pat();
        clocked_pat[13..13 + pat.len()].copy_from_slice(&pat);
        let mut program_map = [0xff; 188];
        program_map[..5].copy_from_slice(&[0x47, 0x50, 0, 0x10, 0]);
        let pmt = pmt();
        program_map[5..5 + pmt.len()].copy_from_slice(&pmt);
        let mut video_packet = [0xff; 188];
        video_packet[..4].copy_from_slice(&[0x47, 0x41, 0, 0x10]);
        let pes = [
            0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5, 0x21, 0, 1, 0, 1, 0x65, 0x80,
        ];
        video_packet[4..4 + pes.len()].copy_from_slice(&pes);
        let root = crate::native::private_tempdir().unwrap();
        let path = root.path().join("source.ts");
        let admit = |packets: &[[u8; 188]]| {
            std::fs::write(&path, packets.concat()).unwrap();
            let mut state = Admission::default();
            state
                .segment(&path, &Stop::new())
                .and_then(|()| state.finish())
        };
        assert_eq!(
            admit(&[clocked_pat, program_map, video_packet]).unwrap_err(),
            BAD
        );
        let mut plain_pat = [0xff; 188];
        plain_pat[..5].copy_from_slice(&[0x47, 0x40, 0, 0x10, 0]);
        plain_pat[5..5 + pat.len()].copy_from_slice(&pat);
        assert_eq!(
            admit(&[plain_pat, program_map, video_packet]).unwrap_err(),
            BAD
        );
        // Even after a valid declaration, a system-table PID is not the PCR PID.
        clocked_pat[3] |= 1;
        assert_eq!(
            admit(&[plain_pat, program_map, clocked_pat, video_packet]).unwrap_err(),
            BAD
        );
        let mut clocked_video = [0xff; 188];
        clocked_video[..6].copy_from_slice(&[0x47, 0x41, 0, 0x30, 7, 0x10]);
        clocked_video[6..12].copy_from_slice(&[0, 0, 0, 0, 0x7e, 0]);
        clocked_video[12..12 + pes.len()].copy_from_slice(&pes);
        admit(&[plain_pat, program_map, clocked_video]).unwrap();
    }
}
