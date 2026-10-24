//! Original clear-transport evidence, before any foreign demuxer or remuxer.
//!
//! Callers retain immutable private snapshots and acquire their shared native
//! CPU/memory permit before constructing this scanner. No tool runs here.
use super::super::ConversionError;
use super::{
    admission::{self, Admission},
    canonical, clock,
};
use crate::{concurrency::Stop, inspection::TrackKind};
use std::{collections::BTreeMap, io::Read as _, path::Path};
use zeroize::Zeroizing;

const BAD: ConversionError = ConversionError("ts-original-evidence-invalid");
const LIMIT: ConversionError = ConversionError("ts-original-pending-limit");
#[cfg(test)]
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
    audio_clocks: BTreeMap<u16, clock::Audio>,
    failed: Option<ConversionError>,
}
impl NativeScanner {
    pub fn new() -> Self {
        Self {
            admission: Admission::default(),
            native: BTreeMap::new(),
            audio_clocks: BTreeMap::new(),
            failed: None,
        }
    }
    pub fn segment(&mut self, path: &Path, stop: &Stop) -> Result<(), ConversionError> {
        self.segment_with(path, stop, &mut |_, _, _, _, _| Ok(()))
    }
    pub fn segment_with<F>(
        &mut self,
        path: &Path,
        stop: &Stop,
        observer: &mut F,
    ) -> Result<(), ConversionError>
    where
        F: FnMut(u16, TrackKind, &[u8], i64, i64) -> Result<(), ConversionError>,
    {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = self.scan(path, stop, observer);
        if let Err(error) = result {
            self.failed = Some(error);
        }
        result
    }
    fn scan<F>(&mut self, path: &Path, stop: &Stop, observer: &mut F) -> Result<(), ConversionError>
    where
        F: FnMut(u16, TrackKind, &[u8], i64, i64) -> Result<(), ConversionError>,
    {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        let file = super::super::files::regular_input(path)?;
        let length = file.metadata().map_err(|_| BAD)?.len();
        // Each segment is independently admitted before its bytes can seed an
        // anchor. Admission retains program, counters and raw PES clock state.
        self.admission.segment(path, stop)?;
        for native in self.native.values_mut() {
            native.begin_segment()?;
        }
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
            // Admission recognized any empty-payload compatibility packet above;
            // it contributes no PES bytes, access unit, sample, or clock.
            if at == 188 {
                continue;
            }
            if at > 188 {
                return Err(BAD);
            }
            if packet[1] & 0x40 != 0 {
                if let Some(previous) = pending.remove(&id) {
                    self.record(id, kind, &previous.bytes, stop, observer)?;
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
            self.record(id, kind, &pes.bytes, stop, observer)?;
        }
        for native in self.native.values_mut() {
            native.end_segment()?;
        }
        Ok(())
    }
    fn record<F>(
        &mut self,
        id: u16,
        kind: TrackKind,
        bytes: &[u8],
        stop: &Stop,
        observer: &mut F,
    ) -> Result<(), ConversionError>
    where
        F: FnMut(u16, TrackKind, &[u8], i64, i64) -> Result<(), ConversionError>,
    {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        let (at, pts, dts) = admission::pes_content(bytes, kind)?;
        let payload = bytes.get(at..).filter(|b| !b.is_empty()).ok_or(BAD)?;
        if let std::collections::btree_map::Entry::Vacant(entry) = self.native.entry(id) {
            let config = match kind {
                TrackKind::Video => canonical::Config::video_access_unit(payload)?,
                TrackKind::Audio => canonical::Config::audio_adts(payload)?,
                _ => return Err(BAD),
            };
            entry.insert(canonical::Native::new(config));
        }
        let anchor = self.native.get_mut(&id).ok_or(BAD)?;
        match kind {
            TrackKind::Video => {
                anchor.record(payload, pts, dts)?;
                observer(id, kind, payload, pts, dts)?;
            }
            TrackKind::Audio => {
                let canonical::Config::Audio { sample_rate, .. } = anchor.config else {
                    return Err(BAD);
                };
                let audio = self
                    .audio_clocks
                    .entry(id)
                    .or_insert(clock::Audio::new(sample_rate)?);
                audio.begin(pts, dts)?;
                let mut position = 0;
                let mut frame_clock = (pts, dts);
                while position < payload.len() {
                    if stop.is_set() {
                        return Err(ConversionError("cancelled"));
                    }
                    let head = payload.get(position..position + 7).ok_or(BAD)?;
                    let length = (usize::from(head[3] & 3) << 11)
                        | (usize::from(head[4]) << 3)
                        | usize::from(head[5] >> 5);
                    if length < 8 {
                        return Err(BAD);
                    }
                    let frame = payload
                        .get(position..position.checked_add(length).ok_or(BAD)?)
                        .ok_or(BAD)?;
                    anchor.record(frame, frame_clock.0, frame_clock.1)?;
                    observer(id, kind, frame, frame_clock.0, frame_clock.1)?;
                    frame_clock = audio.frame(frame_clock.0, frame_clock.1)?;
                    position += length;
                }
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
        evidence_with(
            frames,
            canonical::Config::Audio {
                channels: 2,
                sample_rate: 48_000,
            },
            1920,
        )
    }
    fn evidence_with(
        frames: &[(Vec<u8>, i64)],
        config: canonical::Config,
        duration: i64,
    ) -> canonical::Evidence {
        let mut parser = canonical::Parser::new(0, frames.len() as u64, true, config, 0);
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
    fn fractional_audio_callbacks_bind_every_frame_and_each_original_pes_clock() {
        let (_owner, root) = root();
        let frames: Vec<_> = (0..14)
            .map(|marker| {
                let mut data = frame(marker);
                data[2] = 0x50;
                data
            })
            .collect();
        let first = resource(&root, "first.ts", 126_000, &frames[..7], 0);
        let second = resource(&root, "second.ts", 140_629, &frames[7..], 1);
        let clocks = [
            126_000, 128_090, 130_179, 132_269, 134_359, 136_449, 138_539, 140_629, 142_719,
            144_809, 146_899, 148_989, 151_078, 153_168,
        ];
        let mut scanner = NativeScanner::new();
        let mut captured = Vec::new();
        let mut observer = |pid, kind, data: &[u8], pts, dts| {
            assert_eq!(pid, 256);
            assert_eq!(kind, TrackKind::Audio);
            assert_eq!(pts, dts);
            captured.push((data.to_vec(), pts));
            Ok(())
        };
        scanner
            .segment_with(&first, &Stop::new(), &mut observer)
            .unwrap();
        scanner
            .segment_with(&second, &Stop::new(), &mut observer)
            .unwrap();
        let (_, native) = scanner.finish().unwrap();
        let expected: Vec<_> = frames.into_iter().zip(clocks).collect();
        assert_eq!(captured, expected);
        let config = canonical::Config::Audio {
            channels: 2,
            sample_rate: 44_100,
        };
        assert!(native[&256].matches(&config, &evidence_with(&expected, config.clone(), 2089)));
        let mut original = canonical::Native::new(config);
        for (data, pts) in &expected {
            original.record(data, *pts, *pts).unwrap();
        }
        assert!(native[&256].same_as(&original));
        let mut changed = canonical::Native::new(original.config.clone());
        for (data, pts) in &expected {
            changed.record(data, *pts + 1, *pts + 1).unwrap();
        }
        assert!(!native[&256].same_as(&changed));
    }

    #[test]
    fn observer_failure_poisoning_prevents_partial_original_evidence() {
        let (_owner, root) = root();
        let first = resource(&root, "first.ts", 90_000, &[frame(1), frame(2)], 0);
        let mut scanner = NativeScanner::new();
        let error = ConversionError("observer-stopped");
        assert_eq!(
            scanner
                .segment_with(&first, &Stop::new(), &mut |_, _, _, _, _| Err(error))
                .unwrap_err(),
            error
        );
        assert_eq!(
            scanner
                .segment_with(&first, &Stop::new(), &mut |_, _, _, _, _| panic!(
                    "observer called after failure"
                ))
                .unwrap_err(),
            error
        );
        assert!(scanner.finish().is_err());
    }

    #[test]
    fn empty_payload_padding_cannot_add_frames_or_reset_the_original_audio_clock() {
        let (_owner, root) = root();
        let first = resource(&root, "padded.ts", 90_000, &[frame(1)], 0);
        let mut padded = std::fs::read(&first).unwrap();
        padded.extend(packet(256, false, 1, &[], false));
        padded.extend(packet(256, false, 2, &[], false));
        std::fs::write(&first, padded).unwrap();
        let next = resource(&root, "next.ts", 91_920, &[frame(2)], 1);
        let mut bytes = std::fs::read(&next).unwrap();
        bytes[2 * 188 + 3] = 0x33;
        std::fs::write(&next, bytes).unwrap();
        let mut scanner = NativeScanner::new();
        scanner.segment(&first, &Stop::new()).unwrap();
        scanner.segment(&next, &Stop::new()).unwrap();
        let (admission, native) = scanner.finish().unwrap();
        assert!(admission.has_empty_payload_padding());
        assert_eq!(native[&256].count, 2);
        assert!(native[&256].matches(
            &canonical::Config::Audio {
                channels: 2,
                sample_rate: 48_000
            },
            &evidence(&[(frame(1), 90_000), (frame(2), 91_920)])
        ));
        let first_bytes = std::fs::read(&first).unwrap();
        let next_bytes = std::fs::read(&next).unwrap();
        let combined = [first_bytes.clone(), next_bytes.clone()].concat();
        let source = root.join("combined.ts");
        std::fs::write(&source, &combined).unwrap();
        let view = root.join("demux.ts");
        super::super::compatibility::demux_view(&source, &view, &admission, &Stop::new()).unwrap();
        let mut adjusted_next = next_bytes;
        adjusted_next[2 * 188 + 3] = 0x31;
        assert_eq!(
            std::fs::read(&view).unwrap(),
            [&first_bytes[..first_bytes.len() - 2 * 188], &adjusted_next].concat()
        );
        let mut projected = NativeScanner::new();
        projected.segment(&view, &Stop::new()).unwrap();
        let (projected_admission, projected_native) = projected.finish().unwrap();
        assert!(!projected_admission.has_empty_payload_padding());
        assert_eq!(projected_native[&256].count, native[&256].count);
        assert!(projected_native[&256].matches(
            &canonical::Config::Audio {
                channels: 2,
                sample_rate: 48_000
            },
            &evidence(&[(frame(1), 90_000), (frame(2), 91_920)])
        ));
        assert_eq!(std::fs::read(source).unwrap(), combined);
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
            &canonical::Config::Audio {
                channels: 2,
                sample_rate: 48_000
            },
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
        let config = canonical::Config::Audio {
            channels: 2,
            sample_rate: 48_000,
        };
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
            (2, 0x54),
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
        bytes[2] = 0x54;
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
                sample_rate: 48_000,
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
    #[test]
    fn native_audio_spans_keep_original_boundaries_and_nominal_terminal_duration() {
        for (rate, header_byte, clocks, end) in [
            (48_000, 0x4c, [126_000, 127_920, 129_840], 131_760),
            (44_100, 0x50, [126_000, 128_090, 130_179], 132_268),
        ] {
            let (_owner, root) = root();
            let frames: Vec<_> = (0..3)
                .map(|marker| {
                    let mut f = frame(marker);
                    f[2] = header_byte;
                    f
                })
                .collect();
            let first = resource(
                &root,
                "first.ts",
                u64::try_from(clocks[0]).unwrap(),
                &frames[..2],
                0,
            );
            let last = resource(
                &root,
                "last.ts",
                u64::try_from(clocks[2]).unwrap(),
                &frames[2..],
                1,
            );
            let mut scanner = NativeScanner::new();
            scanner.segment(&first, &Stop::new()).unwrap();
            scanner.segment(&last, &Stop::new()).unwrap();
            let (_, native) = scanner.finish().unwrap();
            let native = &native[&256];
            native.validate().unwrap();
            let timing = native.timing().unwrap();
            assert_eq!(
                (
                    timing.count,
                    timing.first_pts,
                    timing.max_pts,
                    timing.end_pts
                ),
                (3, clocks[0], clocks[2], end)
            );
            assert_eq!(timing.coded_samples, Some(3072));
            assert!(!timing.requires_manifest_duration);
            let segments = native.segment_timings().unwrap();
            assert_eq!(segments.len(), 2);
            assert_eq!((segments[0].count, segments[1].count), (2, 1));
            assert_eq!(segments[1].first_pts, clocks[2]);
            let evidence = native.evidence().unwrap();
            assert_eq!((evidence.first_pts, evidence.end_pts), (clocks[0], end));
            for samples in [1, 1023] {
                let raw = crate::conversion::audio_presentation::AudioEvidence {
                    sample_rate: rate,
                    coded_frames: timing.count,
                    raw_first_pts: timing.first_pts,
                    raw_coded_end: timing.end_pts,
                };
                let declared =
                    crate::conversion::audio_presentation::seconds(samples, u128::from(rate))
                        .unwrap();
                let range = crate::conversion::audio_presentation::AudioPresentation::from_final(
                    raw, 1, &declared,
                )
                .unwrap();
                assert_eq!(u128::from(range.presented_samples), 2048 + samples);
            }
        }
    }
    #[test]
    fn syntax_failure_cannot_yield_native_metadata_or_timing_proof() {
        let (_owner, root) = root();
        let source = track_resource(&root, "invalid-header.ts", 90_000, &video(1), 0, false);
        let mut scanner = NativeScanner::new();
        scanner.segment(&source, &Stop::new()).unwrap();
        let (_, native) = scanner.finish().unwrap();
        let native = &native[&256];
        assert!(native.validate().is_err());
        assert!(native.metadata().is_err());
        assert!(native.timing().is_err());
        assert!(native.segment_timings().is_err());
        assert!(native.evidence().is_err());
    }
    fn short_transport_fixture(root: &Path, variant: &str) -> PathBuf {
        use std::process::Command;
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
        let manifest = root.join("media.m3u8");
        let mut ffmpeg = Command::new("ffmpeg");
        ffmpeg
            .args(["-v", "error", "-threads", "1", "-i"])
            .arg(&fixture);
        if variant == "delayed-audio" {
            ffmpeg.args(["-itsoffset", "0.25", "-i"]).arg(&fixture);
        }
        ffmpeg.args([
            "-map",
            "0:v:0",
            "-map",
            if variant == "delayed-audio" {
                "1:a:0"
            } else {
                "0:a:0"
            },
            "-t",
            "1",
            "-c",
            "copy",
        ]);
        if variant == "bframes" {
            ffmpeg.args(["-c:v", "libx264", "-threads", "1", "-bf", "3", "-g", "12"]);
        }
        if variant == "fractional-audio" {
            ffmpeg.args(["-c:a", "aac", "-ar", "44100"]);
        }
        let output = ffmpeg
            .args([
                "-hls_time",
                "0.5",
                "-hls_playlist_type",
                "vod",
                "-hls_segment_filename",
            ])
            .arg(root.join("seg-%02d.ts"))
            .arg(&manifest)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{variant}: fixture generation failed"
        );
        manifest
    }

    fn assert_oracle_timing(
        variant: &str,
        manifest: &Path,
        admission: &Admission,
        native: &BTreeMap<u16, canonical::Native>,
    ) {
        use std::process::Command;
        let output = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_streams",
                "-show_packets",
                "-show_entries",
                "stream=index,codec_type:packet=stream_index,pts,dts,duration",
                "-of",
                "json",
            ])
            .arg(manifest)
            .output()
            .unwrap();
        assert!(output.status.success());
        let oracle: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        for stream in oracle["streams"].as_array().unwrap() {
            let index = stream["index"].as_u64().unwrap();
            // The HLS demuxer's stream identifier is not the original TS
            // PID. Match the independently selected codec role instead.
            let kind = match stream["codec_type"].as_str().unwrap() {
                "video" => TrackKind::Video,
                "audio" => TrackKind::Audio,
                _ => panic!("unexpected fixture role"),
            };
            let tracks: Vec<_> = admission
                .tracks
                .iter()
                .filter(|track| track.kind == kind)
                .collect();
            assert_eq!(tracks.len(), 1);
            let anchor = &native[&tracks[0].pid];
            let timing = anchor
                .timing()
                .unwrap_or_else(|e| panic!("{variant} {index}: {e:?}"));
            let packets: Vec<_> = oracle["packets"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|packet| packet["stream_index"].as_u64() == Some(index))
                .collect();
            let first = packets
                .iter()
                .map(|p| p["pts"].as_i64().unwrap())
                .min()
                .unwrap();
            let final_packet = packets
                .iter()
                .max_by_key(|p| p["pts"].as_i64().unwrap())
                .unwrap();
            let end =
                final_packet["pts"].as_i64().unwrap() + final_packet["duration"].as_i64().unwrap();
            assert_eq!(
                (timing.count, timing.first_pts, timing.end_pts),
                (u64::try_from(packets.len()).unwrap(), first, end),
                "{variant} {index}"
            );
            assert_eq!(timing.first_dts, packets[0]["dts"].as_i64().unwrap());
            assert_eq!(
                timing.last_dts,
                packets.last().unwrap()["dts"].as_i64().unwrap()
            );
            assert_eq!(
                anchor
                    .segment_timings()
                    .unwrap()
                    .iter()
                    .map(|s| s.count)
                    .sum::<u64>(),
                timing.count
            );
        }
    }

    #[test]
    fn native_metadata_and_packet_spans_match_independent_short_fixture_oracles() {
        for variant in ["copy", "bframes", "fractional-audio", "delayed-audio"] {
            let (_owner, root) = root();
            let manifest = short_transport_fixture(&root, variant);
            let mut scanner = NativeScanner::new();
            for resource in std::fs::read_to_string(&manifest)
                .unwrap()
                .lines()
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
            {
                scanner
                    .segment(&root.join(resource), &Stop::new())
                    .unwrap_or_else(|e| panic!("{variant}: {e:?}"));
            }
            let (admission, native) = scanner.finish().unwrap();
            assert_oracle_timing(variant, &manifest, &admission, &native);
        }
    }
}
