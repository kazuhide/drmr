//! Original clear-transport evidence, before any foreign demuxer or remuxer.
//!
//! Callers retain immutable private snapshots and acquire their shared native
//! CPU/memory permit before constructing this scanner. No tool runs here.
use super::super::ConversionError;
use super::{
    admission::{self, Admission},
    canonical,
};
use crate::{concurrency::Stop, inspection::TrackKind};
use std::{collections::BTreeMap, io::Read as _, path::Path};
use zeroize::Zeroizing;

const BAD: ConversionError = ConversionError("ts-original-evidence-invalid");
const LIMIT: ConversionError = ConversionError("ts-original-pending-limit");
const CLOCK: ConversionError = ConversionError("ts-clock-wrap-or-discontinuity-not-supported");
const MAX_PES: usize = 8 * 1024 * 1024;
struct Pending {
    first: u64,
    slots: usize,
    bytes: Zeroizing<Vec<u8>>,
}
impl Pending {
    fn append(&mut self, bytes: &[u8]) -> Result<(), ConversionError> {
        let slots = self
            .slots
            .checked_add(1)
            .filter(|n| *n <= 65_536)
            .ok_or(LIMIT)?;
        let needed = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_PES)
            .ok_or(LIMIT)?;
        if needed > self.bytes.capacity() {
            let capacity = needed
                .checked_next_power_of_two()
                .filter(|n| *n <= MAX_PES)
                .ok_or(LIMIT)?;
            let mut replacement = Zeroizing::new(Vec::new());
            replacement.try_reserve_exact(capacity).map_err(|_| LIMIT)?;
            replacement.extend_from_slice(&self.bytes);
            // Never reallocate a plaintext-bearing Vec: the old allocation is
            // wiped by Zeroizing before it is released.
            self.bytes = replacement;
        }
        self.bytes.extend_from_slice(bytes);
        self.slots = slots;
        Ok(())
    }
}

pub(in crate::conversion) struct NativeScanner {
    admission: Admission,
    native: BTreeMap<u16, canonical::Native>,
    next_audio: BTreeMap<u16, (i64, i64)>,
    failed: Option<ConversionError>,
}
impl NativeScanner {
    pub fn new() -> Self {
        Self {
            admission: Admission::default(),
            native: BTreeMap::new(),
            next_audio: BTreeMap::new(),
            failed: None,
        }
    }
    pub fn segment(&mut self, path: &Path, stop: &Stop) -> Result<(), ConversionError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = self.scan(path, stop);
        if let Err(error) = result {
            self.failed = Some(error);
        }
        result
    }
    fn scan(&mut self, path: &Path, stop: &Stop) -> Result<(), ConversionError> {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        let file = super::super::files::regular_input(path)?;
        let length = file.metadata().map_err(|_| BAD)?.len();
        // Each segment is independently admitted before its bytes can seed an
        // anchor. Admission retains program, counters and raw PES clock state.
        self.admission.segment(path, stop)?;
        let mut input = std::io::BufReader::new(file);
        let mut pending = BTreeMap::<u16, Pending>::new();
        let mut packet = [0; 188];
        for slot in 0..length / 188 {
            if stop.is_set() {
                return Err(ConversionError("cancelled"));
            }
            if pending.values().any(|p| slot - p.first >= 131_072) {
                return Err(LIMIT);
            }
            input.read_exact(&mut packet).map_err(|_| BAD)?;
            if packet[3] & 0x10 == 0 {
                continue;
            }
            let id = (u16::from(packet[1] & 31) << 8) | u16::from(packet[2]);
            let Some(kind) = self
                .admission
                .tracks
                .iter()
                .find(|t| t.pid == id)
                .map(|t| t.kind)
            else {
                continue;
            };
            let at = if packet[3] & 0x20 == 0 {
                4
            } else {
                5 + usize::from(packet[4])
            };
            if at >= 188 {
                return Err(BAD);
            }
            if packet[1] & 0x40 != 0 {
                if let Some(previous) = pending.remove(&id) {
                    self.record(id, kind, &previous.bytes, stop)?;
                }
                pending.insert(
                    id,
                    Pending {
                        first: slot,
                        slots: 0,
                        bytes: Zeroizing::new(Vec::new()),
                    },
                );
            }
            let pes = pending
                .get_mut(&id)
                .ok_or(ConversionError("ts-pes-incomplete-at-segment-boundary"))?;
            pes.append(&packet[at..])?;
            if pending.len() > 2
                || pending.values().map(|p| p.bytes.len()).sum::<usize>() > 16 * 1024 * 1024
            {
                return Err(LIMIT);
            }
        }
        for (id, pes) in pending {
            let kind = self
                .admission
                .tracks
                .iter()
                .find(|t| t.pid == id)
                .ok_or(BAD)?
                .kind;
            self.record(id, kind, &pes.bytes, stop)?;
        }
        Ok(())
    }
    fn record(
        &mut self,
        id: u16,
        kind: TrackKind,
        bytes: &[u8],
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        let (at, pts, dts) = admission::pes_content(bytes, kind)?;
        let payload = bytes.get(at..).filter(|b| !b.is_empty()).ok_or(BAD)?;
        if let std::collections::btree_map::Entry::Vacant(entry) = self.native.entry(id) {
            let config = match kind {
                TrackKind::Video => canonical::Config::video_access_unit(payload)?,
                TrackKind::Audio => {
                    let head = payload.get(..7).ok_or(BAD)?;
                    if (head[2] >> 2) & 15 != 3 {
                        return Err(ConversionError("ts-aac-sample-rate-not-supported"));
                    }
                    let channels = ((head[2] & 1) << 2) | (head[3] >> 6);
                    canonical::Config::audio(&[0x11, 0x80 | (channels << 3)])?
                }
                _ => return Err(BAD),
            };
            entry.insert(canonical::Native::new(config));
        }
        let anchor = self.native.get_mut(&id).ok_or(BAD)?;
        match kind {
            TrackKind::Video => anchor.record(payload, pts, dts)?,
            TrackKind::Audio => {
                if self
                    .next_audio
                    .get(&id)
                    .is_some_and(|expected| *expected != (pts, dts))
                {
                    return Err(CLOCK);
                }
                let mut position = 0;
                let mut tick = 0_i64;
                while position < payload.len() {
                    if stop.is_set() {
                        return Err(ConversionError("cancelled"));
                    }
                    let head = payload.get(position..position + 7).ok_or(BAD)?;
                    if (head[2] >> 2) & 15 != 3 {
                        return Err(ConversionError("ts-aac-sample-rate-not-supported"));
                    }
                    let length = (usize::from(head[3] & 3) << 11)
                        | (usize::from(head[4]) << 3)
                        | usize::from(head[5] >> 5);
                    if length < 8 {
                        return Err(BAD);
                    }
                    let frame = payload
                        .get(position..position.checked_add(length).ok_or(BAD)?)
                        .ok_or(BAD)?;
                    anchor.record(
                        frame,
                        pts.checked_add(tick).ok_or(CLOCK)?,
                        dts.checked_add(tick).ok_or(CLOCK)?,
                    )?;
                    tick = tick.checked_add(1920).ok_or(CLOCK)?;
                    position += length;
                }
                self.next_audio.insert(
                    id,
                    (
                        pts.checked_add(tick).ok_or(CLOCK)?,
                        dts.checked_add(tick).ok_or(CLOCK)?,
                    ),
                );
            }
            _ => return Err(BAD),
        }
        Ok(())
    }
    pub fn finish(self) -> Result<(Admission, BTreeMap<u16, canonical::Native>), ConversionError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        self.admission.finish()?;
        if self.native.len() != self.admission.tracks.len()
            || self
                .admission
                .tracks
                .iter()
                .any(|track| self.native.get(&track.pid).is_none_or(|n| n.count == 0))
        {
            return Err(BAD);
        }
        Ok((self.admission, self.native))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspection::TrackKind;
    use std::{fmt::Write as _, io::Write as _, path::PathBuf};
    fn section(mut bytes: Vec<u8>) -> Vec<u8> {
        let size = bytes.len() + 1;
        bytes[1] = (bytes[1] & 0xf0) | u8::try_from(size >> 8).unwrap();
        bytes[2] = u8::try_from(size & 255).unwrap();
        bytes.extend(super::super::admission::crc(&bytes).to_be_bytes());
        bytes
    }
    fn packet(id: u16, start: bool, counter: u8, bytes: &[u8], pcr: bool) -> [u8; 188] {
        assert!(bytes.len() <= if pcr { 176 } else { 182 });
        let mut out = [0xff; 188];
        out[..6].copy_from_slice(&[
            0x47,
            u8::try_from(id >> 8).unwrap() | if start { 0x40 } else { 0 },
            u8::try_from(id & 255).unwrap(),
            0x30 | counter,
            u8::try_from(183 - bytes.len()).unwrap(),
            if pcr { 0x10 } else { 0 },
        ]);
        if pcr {
            out[6..12].copy_from_slice(&[0, 0, 0, 0, 0x7e, 0]);
        }
        out[188 - bytes.len()..].copy_from_slice(bytes);
        out
    }
    fn frame(marker: u8) -> Vec<u8> {
        vec![0xff, 0xf1, 0x4c, 0x80, 1, 0x5f, 0xfc, marker, 0x34, 0x80]
    }
    fn pes(pts: u64, content: &[u8], audio: bool) -> Vec<u8> {
        let mut out = vec![
            0,
            0,
            1,
            if audio { 0xc0 } else { 0xe0 },
            0,
            0,
            0x80,
            0x80,
            5,
            0x21 | u8::try_from((pts >> 29) & 0x0e).unwrap(),
            u8::try_from((pts >> 22) & 255).unwrap(),
            u8::try_from((pts >> 14) & 0xfe).unwrap() | 1,
            u8::try_from((pts >> 7) & 255).unwrap(),
            u8::try_from((pts << 1) & 0xfe).unwrap() | 1,
        ];
        out.extend_from_slice(content);
        if audio {
            let n = u16::try_from(out.len() - 6).unwrap().to_be_bytes();
            out[4..6].copy_from_slice(&n);
        }
        out
    }
    fn resource(root: &Path, name: &str, clock: u64, frames: &[Vec<u8>], sequence: u8) -> PathBuf {
        track_resource(root, name, clock, &frames.concat(), sequence, true)
    }
    fn track_resource(
        root: &Path,
        name: &str,
        clock: u64,
        content: &[u8],
        sequence: u8,
        audio: bool,
    ) -> PathBuf {
        let pat = section(vec![0, 0xb0, 0, 0, 1, 0xc1, 0, 0, 0, 1, 0xf0, 0]);
        let pmt = section(vec![
            2,
            0xb0,
            0,
            0,
            1,
            0xc1,
            0,
            0,
            0xe1,
            0,
            0xf0,
            0,
            if audio { 0x0f } else { 0x1b },
            0xe1,
            0,
            0xf0,
            0,
        ]);
        let mut association = vec![0];
        association.extend(pat);
        let mut program = vec![0];
        program.extend(pmt);
        let payload = pes(clock, content, audio);
        let mut packets = vec![
            packet(0, true, sequence, &association, false),
            packet(4096, true, sequence, &program, false),
        ];
        for (index, chunk) in payload.chunks(176).enumerate() {
            packets.push(packet(
                256,
                index == 0,
                (sequence + u8::try_from(index % 16).unwrap()) & 15,
                chunk,
                index == 0,
            ));
        }
        let path = root.join(name);
        crate::native::create_media_new(&path)
            .unwrap()
            .write_all(&packets.concat())
            .unwrap();
        path
    }
    fn root() -> (crate::native::PrivateTempDir, PathBuf) {
        let owner = crate::native::private_tempdir().unwrap();
        let path = owner.path().join("media");
        crate::native::create_media_dir_all(&path).unwrap();
        (owner, path)
    }
    fn evidence(frames: &[(Vec<u8>, i64)]) -> canonical::Evidence {
        evidence_with(frames, canonical::Config::Audio { channels: 2 }, 1920)
    }
    fn evidence_with(
        frames: &[(Vec<u8>, i64)],
        config: canonical::Config,
        duration: i64,
    ) -> canonical::Evidence {
        let mut parser = canonical::Parser::new(0, frames.len() as u64, true, config);
        for (data, pts) in frames {
            let mut dump = String::new();
            for (i, chunk) in data.chunks(16).enumerate() {
                let hex: Vec<_> = chunk.iter().map(|b| format!("{b:02x}")).collect();
                write!(dump, "\\n{:08x}: {}  ignored", i * 16, hex.join(" ")).unwrap();
            }
            let row = format!(
                "packet|stream_index=0|pts={pts}|dts={pts}|duration={duration}|data={dump}\\n|\n"
            );
            parser.push(row.as_bytes()).unwrap();
        }
        parser.finish().unwrap()
    }
    fn video(marker: u8) -> Vec<u8> {
        [
            vec![
                0, 0, 1, 0x67, 0x80, 0, 0, 1, 0x68, 0x80, 0, 0, 1, 6, 5, marker, 0x80,
            ],
            vec![0, 0, 1, 0x65, 0x80],
        ]
        .concat()
    }
    #[test]
    fn every_audio_frame_in_a_multi_packet_pes_is_counted_and_bound() {
        let (_owner, root) = root();
        let frames: Vec<_> = (0..40).map(frame).collect();
        let a = resource(&root, "many.ts", 90_000, &frames, 0);
        let b = resource(&root, "next.ts", 166_800, &[frame(40)], 1);
        // The PSI tables consumed one packet each; the preceding audio PES
        // consumed three. Their continuity counters advance independently.
        let mut next = std::fs::read(&b).unwrap();
        next[2 * 188 + 3] = 0x33;
        std::fs::write(&b, next).unwrap();
        let mut scanner = NativeScanner::new();
        scanner.segment(&a, &Stop::new()).unwrap();
        scanner.segment(&b, &Stop::new()).unwrap();
        let (_, native) = scanner.finish().unwrap();
        let mut expected: Vec<_> = frames
            .into_iter()
            .enumerate()
            .map(|(i, f)| (f, 90_000 + i64::try_from(i).unwrap() * 1920))
            .collect();
        expected.push((frame(40), 166_800));
        assert_eq!(native[&256].count, 41);
        assert!(native[&256].matches(
            &canonical::Config::Audio { channels: 2 },
            &evidence(&expected)
        ));
    }
    #[test]
    fn missing_extra_or_changed_audio_frames_cannot_preserve_an_anchor() {
        let (_owner, root) = root();
        for (i, frames) in [vec![frame(1)], vec![frame(1), frame(2), frame(3)]]
            .iter()
            .enumerate()
        {
            let a = resource(&root, &format!("count-{i}.ts"), 90_000, frames, 0);
            let b = resource(&root, &format!("next-{i}.ts"), 93_840, &[frame(4)], 1);
            let mut scanner = NativeScanner::new();
            scanner.segment(&a, &Stop::new()).unwrap();
            assert_eq!(scanner.segment(&b, &Stop::new()).unwrap_err(), CLOCK);
            assert!(scanner.finish().is_err());
        }
        let mut changed = frame(2);
        changed[3] = 0xc0;
        let a = resource(&root, "changed.ts", 90_000, &[frame(1), changed.clone()], 0);
        let mut scanner = NativeScanner::new();
        assert!(scanner.segment(&a, &Stop::new()).is_err());
        assert!(scanner.finish().is_err());
        let a = resource(&root, "first.ts", 90_000, &[frame(1)], 0);
        let b = resource(&root, "channel.ts", 91_920, &[changed], 1);
        let mut scanner = NativeScanner::new();
        scanner.segment(&a, &Stop::new()).unwrap();
        assert!(scanner.segment(&b, &Stop::new()).is_err());
    }
    #[test]
    fn original_video_preserves_parameter_sets_nonframing_units_and_raw_clocks() {
        let (_owner, root) = root();
        let a = track_resource(&root, "video-a.ts", 90_000, &video(1), 0, false);
        let b = track_resource(&root, "video-b.ts", 93_750, &video(2), 1, false);
        let before = std::fs::read(&a).unwrap();
        let mut scanner = NativeScanner::new();
        scanner.segment(&a, &Stop::new()).unwrap();
        scanner.segment(&b, &Stop::new()).unwrap();
        let (_, native) = scanner.finish().unwrap();
        let config = canonical::Config::video_access_unit(&video(1)).unwrap();
        assert_eq!(native[&256].count, 2);
        assert!(native[&256].matches(
            &config,
            &evidence_with(
                &[(video(1), 90_000), (video(2), 93_750)],
                config.clone(),
                3750
            )
        ));
        assert!(!native[&256].matches(
            &config,
            &evidence_with(
                &[(video(1), 90_000), (video(3), 93_750)],
                config.clone(),
                3750
            )
        ));
        assert!(!native[&256].matches(
            &config,
            &evidence_with(
                &[(video(1), 90_001), (video(2), 93_751)],
                config.clone(),
                3750
            )
        ));
        let mut changed = video(2);
        changed[4] = 0x81;
        let c = track_resource(&root, "changed-video.ts", 93_750, &changed, 1, false);
        let mut scanner = NativeScanner::new();
        scanner.segment(&a, &Stop::new()).unwrap();
        assert!(scanner.segment(&c, &Stop::new()).is_err());
        assert_eq!(std::fs::read(a).unwrap(), before);
    }
    #[test]
    fn pending_bytes_slots_and_packet_span_are_bounded() {
        let mut pending = Pending {
            first: 0,
            slots: 65_536,
            bytes: Zeroizing::new(vec![1]),
        };
        assert_eq!(pending.append(&[2]).unwrap_err(), LIMIT);
        assert_eq!(&*pending.bytes, &[1]);
        let mut pending = Pending {
            first: 0,
            slots: 0,
            bytes: Zeroizing::new(vec![0; MAX_PES]),
        };
        assert_eq!(pending.append(&[1]).unwrap_err(), LIMIT);
        assert_eq!(pending.bytes.len(), MAX_PES);
        let (_owner, root) = root();
        let p = resource(&root, "span.ts", 90_000, &[frame(1)], 0);
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        let null = packet(8191, false, 0, &[0], false);
        for _ in 0..131_072 {
            f.write_all(&null).unwrap();
        }
        drop(f);
        let mut scanner = NativeScanner::new();
        assert_eq!(scanner.segment(&p, &Stop::new()).unwrap_err(), LIMIT);
        assert!(scanner.finish().is_err());
    }
    #[test]
    fn original_audio_frames_and_absolute_clocks_span_every_resource() {
        let (_owner, root) = root();
        let a = resource(&root, "a.ts", 90_000, &[frame(0x12), frame(0x13)], 0);
        let b = resource(&root, "b.ts", 93_840, &[frame(0x14)], 1);
        let before = std::fs::read(&a).unwrap();
        let mut scanner = NativeScanner::new();
        scanner.segment(&a, &Stop::new()).unwrap();
        scanner.segment(&b, &Stop::new()).unwrap();
        let (admission, native) = scanner.finish().unwrap();
        assert_eq!(admission.tracks[0].kind, TrackKind::Audio);
        assert_eq!(native[&256].count, 3);
        let config = canonical::Config::Audio { channels: 2 };
        assert!(native[&256].matches(
            &config,
            &evidence(&[
                (frame(0x12), 90_000),
                (frame(0x13), 91_920),
                (frame(0x14), 93_840)
            ])
        ));
        assert!(!native[&256].matches(
            &config,
            &evidence(&[
                (frame(0x22), 90_000),
                (frame(0x13), 91_920),
                (frame(0x14), 93_840)
            ])
        ));
        assert_eq!(std::fs::read(a).unwrap(), before);
    }
    #[test]
    fn next_audio_pes_clock_cannot_repeat_drift_gap_overlap_or_wrap() {
        let (_owner, root) = root();
        let a = resource(&root, "a.ts", 90_000, &[frame(0x12), frame(0x13)], 0);
        for (i, pts) in [90_000, 91_920, 93_839, 93_841, 95_760, 0]
            .into_iter()
            .enumerate()
        {
            let b = resource(&root, &format!("b-{i}.ts"), pts, &[frame(0x14)], 1);
            let mut scanner = NativeScanner::new();
            scanner.segment(&a, &Stop::new()).unwrap();
            assert!(scanner.segment(&b, &Stop::new()).is_err());
            assert!(scanner.finish().is_err());
        }
    }
    #[test]
    fn malformed_audio_configuration_or_incomplete_frame_is_never_anchored() {
        let (_owner, root) = root();
        let mut cases = Vec::new();
        for (at, value) in [
            (0, 0),
            (1, 0xf0),
            (2, 0x50),
            (2, 0x0c),
            (3, 0x00),
            (6, 0xfd),
        ] {
            let mut f = frame(0x12);
            f[at] = value;
            cases.push(f);
        }
        let mut f = frame(0x12);
        f.pop();
        cases.push(f);
        for (i, bytes) in cases.iter().enumerate() {
            let p = resource(
                &root,
                &format!("bad-{i}.ts"),
                90_000,
                std::slice::from_ref(bytes),
                0,
            );
            let mut scanner = NativeScanner::new();
            assert!(scanner.segment(&p, &Stop::new()).is_err());
            assert!(scanner.finish().is_err());
        }
    }
    #[test]
    fn unsupported_audio_rate_keeps_its_explicit_profile_reason() {
        let (_owner, root) = root();
        let mut bytes = frame(0x12);
        bytes[2] = 0x50;
        let p = resource(&root, "rate.ts", 90_000, &[bytes], 0);
        let mut scanner = NativeScanner::new();
        assert_eq!(
            scanner.segment(&p, &Stop::new()).unwrap_err(),
            ConversionError("ts-aac-sample-rate-not-supported")
        );
        assert!(scanner.finish().is_err());
    }
    #[test]
    fn audio_discovery_requires_its_own_nonempty_audio_anchor() {
        use std::sync::Arc;
        let (_owner, root) = root();
        let p = resource(&root, "audio.ts", 90_000, &[frame(0x12)], 0);
        let mut scanner = NativeScanner::new();
        scanner.segment(&p, &Stop::new()).unwrap();
        let (admission, native) = scanner.finish().unwrap();
        let mut map: BTreeMap<_, _> = native
            .into_iter()
            .map(|(id, n)| (id, Arc::new(n)))
            .collect();
        super::super::require_audio_source_evidence(&admission, &map).unwrap();
        let anchor = map.remove(&256).unwrap();
        assert!(super::super::require_audio_source_evidence(&admission, &map).is_err());
        map.insert(257, anchor);
        assert!(super::super::require_audio_source_evidence(&admission, &map).is_err());
        map.insert(
            256,
            Arc::new(canonical::Native::new(canonical::Config::Audio {
                channels: 2,
            })),
        );
        assert!(super::super::require_audio_source_evidence(&admission, &map).is_err());
        let mut video = canonical::Native::new(canonical::Config::Video {
            sps: vec![0x67, 0x80],
            pps: vec![0x68, 0x80],
        });
        video
            .record(&[0, 0, 1, 0x65, 0x80], 90_000, 90_000)
            .unwrap();
        map.insert(256, Arc::new(video));
        assert!(super::super::require_audio_source_evidence(&admission, &map).is_err());
    }
    #[test]
    fn empty_cancelled_or_invalid_transport_never_returns_partial_evidence() {
        assert!(NativeScanner::new().finish().is_err());
        let (_owner, root) = root();
        let p = resource(&root, "a.ts", 90_000, &[frame(0x12)], 0);
        let stop = Stop::new();
        stop.set();
        let mut scanner = NativeScanner::new();
        assert_eq!(
            scanner.segment(&p, &stop).unwrap_err(),
            ConversionError("cancelled")
        );
        assert!(scanner.finish().is_err());
        let broken = root.join("broken.ts");
        crate::native::create_media_new(&broken)
            .unwrap()
            .write_all(&[0; 188])
            .unwrap();
        let mut scanner = NativeScanner::new();
        assert!(scanner.segment(&broken, &Stop::new()).is_err());
        assert!(scanner.finish().is_err());
    }
}
