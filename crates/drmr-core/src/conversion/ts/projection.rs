//! Bounded, same-slot transport projection; timestamps and PCR bytes are never regenerated.
use super::super::ConversionError;
use super::{admission, canonical, clock};
use crate::{concurrency::Stop, inspection::TrackKind};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    path::Path,
};
use zeroize::Zeroizing;

const BAD: ConversionError = ConversionError("ts-sample-projection-invalid");
const LIMIT: ConversionError = ConversionError("ts-sample-pending-limit");

struct Pending {
    first: u64,
    slots: Vec<u64>,
    bytes: Zeroizing<Vec<u8>>,
}

pub(in crate::conversion) struct Projector {
    encrypted: admission::Admission,
    pub clear: admission::Admission,
    pub native: BTreeMap<u16, canonical::Native>,
    audio_clocks: BTreeMap<u16, clock::Audio>,
    failed: Option<ConversionError>,
}
impl Projector {
    pub fn new() -> Self {
        Self {
            encrypted: admission::Admission::encrypted_samples(),
            clear: admission::Admission::default(),
            native: BTreeMap::new(),
            audio_clocks: BTreeMap::new(),
            failed: None,
        }
    }
    pub fn encrypted_segment(
        &mut self,
        source: &Path,
        destination: &Path,
        key: &super::super::Aes128Key,
        iv: &super::super::SegmentIv,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        self.segment(source, destination, stop, |kind, bytes| match kind {
            TrackKind::Video => super::sample_aes::decrypt_h264_annex_b(bytes, key, iv),
            TrackKind::Audio => super::sample_aes::decrypt_aac_adts(bytes, key, iv),
            _ => Err(BAD),
        })
    }
    pub fn segment<F>(
        &mut self,
        source: &Path,
        destination: &Path,
        stop: &Stop,
        decrypt: F,
    ) -> Result<(), ConversionError>
    where
        F: FnMut(TrackKind, &[u8]) -> Result<Zeroizing<Vec<u8>>, ConversionError>,
    {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = self.segment_checked(source, destination, stop, decrypt);
        if let Err(error) = result {
            self.failed = Some(error);
        }
        result
    }
    fn segment_checked<F>(
        &mut self,
        source: &Path,
        destination: &Path,
        stop: &Stop,
        mut decrypt: F,
    ) -> Result<(), ConversionError>
    where
        F: FnMut(TrackKind, &[u8]) -> Result<Zeroizing<Vec<u8>>, ConversionError>,
    {
        // Admission precedes any foreign demuxer and bounds even unspecified-length video PES.
        self.encrypted.segment(source, stop)?;
        for native in self.native.values_mut() {
            native.begin_segment()?;
        }
        let mut input = std::io::BufReader::new(File::open(source).map_err(|_| BAD)?);
        let mut original = File::open(source).map_err(|_| BAD)?;
        let mut output = super::super::files::create_new(destination)?;
        let mut buffer = vec![0; 64 * 1024];
        loop {
            if stop.is_set() {
                return Err(ConversionError("cancelled"));
            }
            let n = input.read(&mut buffer).map_err(|_| BAD)?;
            if n == 0 {
                break;
            }
            output.write_all(&buffer[..n]).map_err(|_| BAD)?;
        }
        input.rewind().map_err(|_| BAD)?;
        let length = input.get_ref().metadata().map_err(|_| BAD)?.len();
        let clear_pmt = self.encrypted.clear_pmt()?;
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
            let id = (u16::from(packet[1] & 31) << 8) | u16::from(packet[2]);
            if packet[3] & 0x10 == 0 {
                continue;
            }
            let at = payload_at(&packet)?;
            if Some(id) == self.encrypted.pmt_pid() {
                let mut payload = vec![0];
                payload.extend_from_slice(&clear_pmt);
                write_slot(&mut output, slot, &replace_payload(&packet, &payload)?)?;
            } else if let Some(track) = self.encrypted.tracks.iter().find(|t| t.pid == id) {
                let kind = track.kind;
                if packet[1] & 0x40 != 0
                    && let Some(pes) = pending.remove(&id)
                {
                    self.flush(
                        (id, kind),
                        &pes,
                        &mut original,
                        &mut output,
                        stop,
                        &mut decrypt,
                    )?;
                }
                if packet[1] & 0x40 == 0 && !pending.contains_key(&id) {
                    return Err(ConversionError("ts-pes-incomplete-at-segment-boundary"));
                }
                let pes = pending.entry(id).or_insert_with(|| Pending {
                    first: slot,
                    slots: Vec::new(),
                    bytes: Zeroizing::new(Vec::new()),
                });
                if pes.slots.len() >= 65_536 || pes.bytes.len() + 188 - at > 8 * 1024 * 1024 {
                    return Err(LIMIT);
                }
                pes.slots.push(slot);
                pes.bytes.extend_from_slice(&packet[at..]);
                if pending.values().map(|p| p.bytes.len()).sum::<usize>() > 16 * 1024 * 1024 {
                    return Err(LIMIT);
                }
            }
        }
        for (id, pes) in pending {
            let kind = self
                .encrypted
                .tracks
                .iter()
                .find(|t| t.pid == id)
                .ok_or(BAD)?
                .kind;
            self.flush(
                (id, kind),
                &pes,
                &mut original,
                &mut output,
                stop,
                &mut decrypt,
            )?;
        }
        output.flush().map_err(|_| BAD)?;
        drop(output);
        // These boundaries belong to the original encrypted resources, before
        // the same-slot clear projection is admitted independently below.
        for native in self.native.values_mut() {
            native.end_segment()?;
        }
        self.clear.segment(destination, stop)
    }
    fn flush<F>(
        &mut self,
        track: (u16, TrackKind),
        pes: &Pending,
        original: &mut File,
        output: &mut File,
        stop: &Stop,
        decrypt: &mut F,
    ) -> Result<(), ConversionError>
    where
        F: FnMut(TrackKind, &[u8]) -> Result<Zeroizing<Vec<u8>>, ConversionError>,
    {
        let (id, kind) = track;
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        let (at, pts, dts) = admission::pes_content(&pes.bytes, kind)?;
        let plaintext = decrypt(kind, &pes.bytes[at..])?;
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        if plaintext.is_empty() || plaintext.len() > pes.bytes.len() - at {
            return Err(BAD);
        }
        if !self.native.contains_key(&id) {
            let config = match kind {
                TrackKind::Video => canonical::Config::video_access_unit(&plaintext)?,
                TrackKind::Audio => {
                    canonical::Config::audio(&self.encrypted.audio_specific_config.ok_or(BAD)?)?
                }
                _ => return Err(BAD),
            };
            self.native.insert(id, canonical::Native::new(config));
        }
        let anchor = self.native.get_mut(&id).ok_or(BAD)?;
        match kind {
            TrackKind::Video => anchor.record(&plaintext, pts, dts)?,
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
                while position < plaintext.len() {
                    let head = plaintext.get(position..position + 7).ok_or(BAD)?;
                    let length = (usize::from(head[3] & 3) << 11)
                        | (usize::from(head[4]) << 3)
                        | usize::from(head[5] >> 5);
                    if length < 8 {
                        return Err(BAD);
                    }
                    let frame = plaintext.get(position..position + length).ok_or(BAD)?;
                    anchor.record(frame, frame_clock.0, frame_clock.1)?;
                    frame_clock = audio.frame(frame_clock.0, frame_clock.1)?;
                    position += length;
                }
            }
            _ => return Err(BAD),
        }
        let mut clear = Zeroizing::new(Vec::with_capacity(at + plaintext.len()));
        clear.extend_from_slice(&pes.bytes[..at]);
        clear.extend_from_slice(&plaintext);
        if clear[4..6] != [0, 0] {
            let length = u16::try_from(clear.len() - 6).map_err(|_| BAD)?;
            clear[4..6].copy_from_slice(&length.to_be_bytes());
        }
        if clear.len() < pes.slots.len() {
            return Err(BAD);
        }
        let mut position = 0;
        for (index, slot) in pes.slots.iter().enumerate() {
            if stop.is_set() {
                return Err(ConversionError("cancelled"));
            }
            let mut packet = [0; 188];
            original
                .seek(SeekFrom::Start(slot * 188))
                .map_err(|_| BAD)?;
            original.read_exact(&mut packet).map_err(|_| BAD)?;
            let capacity = 188 - payload_at(&packet)?;
            let remaining_slots = pes.slots.len() - index - 1;
            let n = capacity.min(clear.len() - position - remaining_slots);
            write_slot(
                output,
                *slot,
                &replace_payload(&packet, &clear[position..position + n])?,
            )?;
            position += n;
        }
        if position != clear.len() {
            return Err(BAD);
        }
        Ok(())
    }
    pub fn finish(&self) -> Result<(), ConversionError> {
        self.encrypted.finish()?;
        self.clear.finish()
    }
}

fn payload_at(packet: &[u8; 188]) -> Result<usize, ConversionError> {
    let at = if packet[3] & 0x20 != 0 {
        5 + usize::from(packet[4])
    } else {
        4
    };
    if at >= 188 {
        return Err(BAD);
    }
    Ok(at)
}
fn write_slot(output: &mut File, slot: u64, packet: &[u8; 188]) -> Result<(), ConversionError> {
    output
        .seek(SeekFrom::Start(slot.checked_mul(188).ok_or(BAD)?))
        .map_err(|_| BAD)?;
    output.write_all(packet).map_err(|_| BAD)
}

fn replace_payload(packet: &[u8; 188], payload: &[u8]) -> Result<[u8; 188], ConversionError> {
    let mode = (packet[3] >> 4) & 3;
    if packet[0] != 0x47 || mode & 1 == 0 {
        return Err(BAD);
    }
    let old_start = if mode & 2 != 0 {
        5 + usize::from(packet[4])
    } else {
        4
    };
    if old_start >= 188 || payload.is_empty() || payload.len() > 188 - old_start {
        return Err(BAD);
    }
    let start = 188 - payload.len();
    let mut result = [0xff; 188];
    result[..4].copy_from_slice(&packet[..4]);
    if start > 4 {
        result[3] |= 0x20;
        result[4] = u8::try_from(start - 5).map_err(|_| BAD)?;
        if mode & 2 != 0 && old_start > 5 {
            result[5..old_start].copy_from_slice(&packet[5..old_start]);
        } else if start > 5 {
            result[5] = 0;
        }
    }
    result[start..].copy_from_slice(payload);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(mut bytes: Vec<u8>) -> Vec<u8> {
        let length = bytes.len() + 1;
        bytes[1] = (bytes[1] & 0xf0) | u8::try_from(length >> 8).unwrap();
        bytes[2] = u8::try_from(length & 255).unwrap();
        bytes.extend(admission::crc(&bytes).to_be_bytes());
        bytes
    }
    fn media(pts: u64, config: bool) -> Vec<u8> {
        let mut bytes = vec![
            0,
            0,
            1,
            0xe0,
            0,
            0,
            0x80,
            0x80,
            5,
            0x21 | u8::try_from((pts >> 29) & 0x0e).unwrap(),
            u8::try_from((pts >> 22) & 0xff).unwrap(),
            u8::try_from((pts >> 14) & 0xfe).unwrap() | 1,
            u8::try_from((pts >> 7) & 0xff).unwrap(),
            u8::try_from((pts << 1) & 0xfe).unwrap() | 1,
        ];
        if config {
            bytes.extend([0, 0, 1, 0x67, 0x80, 0, 0, 1, 0x68, 0x80]);
        }
        bytes.extend([0, 0, 1, 0x65, 0x80, 0x55, 0x55]);
        bytes
    }
    fn ts_packet(id: u16, start: bool, counter: u8, payload: &[u8], pcr: bool) -> [u8; 188] {
        let mut packet = [0xff; 188];
        packet[..6].copy_from_slice(&[
            0x47,
            u8::try_from(id >> 8).unwrap() | if start { 0x40 } else { 0 },
            u8::try_from(id & 255).unwrap(),
            0x30 | counter,
            u8::try_from(183 - payload.len()).unwrap(),
            if pcr { 0x10 } else { 0 },
        ]);
        if pcr {
            packet[6..12].copy_from_slice(&[0, 0, 0, 0, 0x7e, 0]);
        }
        packet[188 - payload.len()..].copy_from_slice(payload);
        packet
    }
    fn projection_packets() -> [[u8; 188]; 6] {
        let pat = section(vec![0, 0xb0, 0, 0, 1, 0xc1, 0, 0, 0, 1, 0xf0, 0]);
        let pmt = section(vec![
            2, 0xb0, 0, 0, 1, 0xc1, 0, 0, 0xe1, 0, 0xf0, 0, 0xdb, 0xe1, 0, 0xf0, 6, 0x0f, 4, b'z',
            b'a', b'v', b'c',
        ]);
        let mut association = vec![0];
        association.extend(pat);
        let mut program = vec![0];
        program.extend(pmt);
        let mut null = [0xff; 188];
        null[..4].copy_from_slice(&[0x47, 0x1f, 0xff, 0x10]);
        [
            ts_packet(0, true, 0, &association, false),
            ts_packet(4096, true, 0, &program, false),
            ts_packet(256, true, 0, &media(90000, true), true),
            null,
            ts_packet(256, true, 1, &media(93750, false), false),
            null,
        ]
    }
    #[test]
    fn projection_keeps_media_slots_raw_clocks_and_interleaved_packets() {
        let packets = projection_packets();
        let root = crate::native::private_tempdir().unwrap();
        let media = root.path().join("media");
        crate::native::create_media_dir_all(&media).unwrap();
        let source = media.join("source.ts");
        let destination = media.join("clear.ts");
        std::fs::write(&source, packets.concat()).unwrap();
        let mut projection = Projector::new();
        projection
            .segment(&source, &destination, &Stop::new(), |kind, bytes| {
                assert_eq!(kind, TrackKind::Video);
                Ok(Zeroizing::new(bytes.to_vec()))
            })
            .unwrap();
        projection.finish().unwrap();
        assert_eq!(projection.native[&256].count, 2);
        let output = std::fs::read(destination).unwrap();
        assert_eq!(output.len(), packets.len() * 188);
        for slot in [0, 2, 3, 4, 5] {
            assert_eq!(&output[slot * 188..(slot + 1) * 188], &packets[slot]);
        }
        assert_ne!(&output[188..376], &packets[1]);
        assert_eq!(std::fs::read(source).unwrap(), packets.concat());
    }
    #[test]
    fn pending_span_counts_interleaved_null_packets_not_only_elementary_bytes() {
        let packets = projection_packets();
        let root = crate::native::private_tempdir().unwrap();
        let media = root.path().join("media");
        crate::native::create_media_dir_all(&media).unwrap();
        let source = media.join("source.ts");
        let destination = media.join("clear.ts");
        let mut writer = std::io::BufWriter::new(File::create(&source).unwrap());
        for packet in &packets[..3] {
            writer.write_all(packet).unwrap();
        }
        for _ in 0..131_072 {
            writer.write_all(&packets[3]).unwrap();
        }
        writer.write_all(&packets[4]).unwrap();
        writer.flush().unwrap();
        drop(writer);
        let result = Projector::new().segment(&source, &destination, &Stop::new(), |_, bytes| {
            Ok(Zeroizing::new(bytes.to_vec()))
        });
        assert_eq!(result.unwrap_err(), LIMIT);
    }

    fn packet(adaptation: Option<u8>) -> [u8; 188] {
        let mut bytes = [0x55; 188];
        bytes[..4].copy_from_slice(&[
            0x47,
            0x41,
            0x00,
            if adaptation.is_some() { 0x37 } else { 0x17 },
        ]);
        if let Some(length) = adaptation {
            bytes[4] = length;
            if length != 0 {
                bytes[5] = 0x50;
                bytes[6..12].copy_from_slice(&[0, 0, 0, 1, 0x7e, 0]);
            }
        }
        bytes
    }

    #[test]
    fn reduced_payload_preserves_every_header_pcr_and_continuity_bit() {
        for adaptation in [None, Some(0), Some(7)] {
            let input = packet(adaptation);
            let at = adaptation.map_or(4, |n| 5 + usize::from(n));
            for length in [1, 16, 188 - at - 1, 188 - at] {
                let payload = vec![0x66; length];
                let result = replace_payload(&input, &payload).unwrap();
                assert_eq!(&result[..3], &input[..3]);
                assert_eq!(result[3] & 0xcf, input[3] & 0xcf);
                let start = if result[3] & 0x20 != 0 {
                    5 + usize::from(result[4])
                } else {
                    4
                };
                assert_eq!(&result[start..], payload);
                if adaptation == Some(7) {
                    assert_eq!(&result[5..12], &input[5..12]);
                    assert!(result[12..start].iter().all(|&b| b == 0xff));
                } else if start > 5 {
                    assert_eq!(result[5], 0);
                    assert!(result[6..start].iter().all(|&b| b == 0xff));
                }
            }
            assert_eq!(replace_payload(&input, &[]).unwrap_err(), BAD);
            assert_eq!(
                replace_payload(&input, &vec![0; 189 - at]).unwrap_err(),
                BAD
            );
        }
    }
}
