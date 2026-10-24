//! SAMPLE-AES transport output that retains original segment membership and clocks.

use super::{admission, canonical, native::NativeScanner, sample_aes};
use crate::{
    concurrency::Stop,
    conversion::{Aes128Key, ConversionError, SegmentIv, files},
    inspection::TrackKind,
    native::FileIdentity,
};
use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read as _, Write as _},
    path::Path,
};
use zeroize::Zeroizing;

const BAD: ConversionError = ConversionError("ts-segment-encryption-invalid");
const LIMIT: ConversionError = ConversionError("ts-segment-encryption-limit");
const MAX_PES: usize = 8 * 1024 * 1024;
const MAX_SLOTS: usize = 65_536;
const MAX_WINDOW: usize = 131_072;

struct Pending {
    first: u64,
    slots: Vec<u64>,
    bytes: Zeroizing<Vec<u8>>,
}
impl Pending {
    fn append(&mut self, slot: u64, bytes: &[u8]) -> Result<(), ConversionError> {
        let needed = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|n| *n <= MAX_PES)
            .ok_or(LIMIT)?;
        if self.slots.len() == MAX_SLOTS {
            return Err(LIMIT);
        }
        if needed > self.bytes.capacity() {
            let capacity = needed
                .checked_next_power_of_two()
                .filter(|n| *n <= MAX_PES)
                .ok_or(LIMIT)?;
            let mut replacement = Zeroizing::new(Vec::new());
            replacement.try_reserve_exact(capacity).map_err(|_| LIMIT)?;
            replacement.extend_from_slice(&self.bytes);
            self.bytes = replacement;
        }
        self.slots.try_reserve(1).map_err(|_| LIMIT)?;
        self.slots.push(slot);
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

struct Continuation {
    packet: Box<Zeroizing<[u8; 188]>>,
}
struct Slot {
    packet: Zeroizing<[u8; 188]>,
    // SAMPLE-AES packets still contain clear regions. Keep their allocations
    // stable while the pointer vector grows, just as for the original slots.
    extra: Vec<Continuation>,
}
#[derive(Default)]
struct Window {
    first: u64,
    // Reallocating this queue only moves owning pointers. Plaintext-bearing
    // packet allocations remain in place and are wiped when each slot drops.
    slots: VecDeque<Box<Slot>>,
}
impl Window {
    fn push(&mut self, packet: Zeroizing<[u8; 188]>) -> Result<(), ConversionError> {
        if self.slots.len() == MAX_WINDOW {
            return Err(LIMIT);
        }
        self.slots.try_reserve(1).map_err(|_| LIMIT)?;
        self.slots.push_back(Box::new(Slot {
            packet,
            extra: Vec::new(),
        }));
        Ok(())
    }
    fn get(&mut self, slot: u64) -> Result<&mut Slot, ConversionError> {
        let index = usize::try_from(slot.checked_sub(self.first).ok_or(BAD)?).map_err(|_| LIMIT)?;
        self.slots.get_mut(index).map(Box::as_mut).ok_or(BAD)
    }
}

/// One ordered playlist cohort. The caller holds a native resource permit and
/// retains immutable original snapshots and a private output staging directory.
/// A failed call poisons the cohort; the caller owns cleanup of the staging tree.
pub(in crate::conversion) struct SegmentEncryptor {
    clear: admission::Admission,
    original: NativeScanner,
    encrypted: admission::Admission,
    counters: BTreeMap<u16, u8>,
    audio: Option<[u8; 2]>,
    failed: Option<ConversionError>,
}
impl SegmentEncryptor {
    pub fn new() -> Self {
        Self {
            clear: admission::Admission::default(),
            original: NativeScanner::new(),
            encrypted: admission::Admission::encrypted_samples(),
            counters: BTreeMap::new(),
            audio: None,
            failed: None,
        }
    }
    pub fn encrypt_segment(
        &mut self,
        source: &Path,
        destination: &Path,
        key: &Aes128Key,
        iv: &SegmentIv,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        let result = self.segment_checked(source, destination, key, iv, stop);
        if let Err(error) = result {
            self.failed = Some(error);
        }
        result
    }
    fn segment_checked(
        &mut self,
        source: &Path,
        destination: &Path,
        key: &Aes128Key,
        iv: &SegmentIv,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        checkpoint(stop)?;
        let held = files::regular_input(source)?;
        let identity = FileIdentity::read(&held).map_err(|_| BAD)?;
        let metadata = held.metadata().map_err(|_| BAD)?;
        if metadata.len() > super::super::MAX_MEDIA_BYTES {
            return Err(ConversionError("media-size-limit"));
        }
        let modified = metadata.modified().map_err(|_| BAD)?;
        // Admission and native codec/clock checks both precede output creation.
        let (pmt, pmt_pid) = self.admit(source, stop)?;
        checkpoint(stop)?;
        let mut input = std::io::BufReader::new(held);
        let mut output = std::io::BufWriter::new(files::create_new(destination)?);
        let mut pending = BTreeMap::<u16, Pending>::new();
        let mut window = Window::default();
        for slot in 0..metadata.len() / 188 {
            checkpoint(stop)?;
            if pending
                .values()
                .any(|p| slot.saturating_sub(p.first) >= MAX_WINDOW as u64)
            {
                return Err(LIMIT);
            }
            let mut packet = Zeroizing::new([0; 188]);
            input.read_exact(packet.as_mut()).map_err(|_| BAD)?;
            let id = packet_pid(&packet);
            let start = packet[1] & 0x40 != 0;
            let at = payload_at(&packet)?;
            if id == pmt_pid {
                if start {
                    let mut payload = Vec::with_capacity(pmt.len() + 1);
                    payload.push(0);
                    payload.extend_from_slice(&pmt);
                    packet = rewrite_payload(&packet, &payload)?;
                } else {
                    // The qualified PMT fits one packet. Its old continuation
                    // slots carry no media or PCR and become null packets.
                    packet = null_packet();
                }
            } else if at == 188 && packet[3] & 0x10 != 0 {
                // Preserve admitted empty stuffing as a standard adaptation-only
                // packet; encrypted admission deliberately forbids the legacy
                // empty mode-3 compatibility shape.
                packet[3] &= !0x10;
            }
            window.push(packet)?;
            if let Some(kind) = self
                .clear
                .tracks
                .iter()
                .find(|t| t.pid == id)
                .map(|t| t.kind)
                && at < 188
            {
                if start {
                    if let Some(previous) = pending.remove(&id) {
                        Self::protect((id, kind), &previous, &mut window, key, iv, stop)?;
                    }
                    pending.insert(
                        id,
                        Pending {
                            first: slot,
                            slots: Vec::new(),
                            bytes: Zeroizing::new(Vec::new()),
                        },
                    );
                }
                let current = pending
                    .get_mut(&id)
                    .ok_or(ConversionError("ts-pes-incomplete-at-segment-boundary"))?;
                current.append(slot, &window.get(slot)?.packet[at..])?;
                if pending.values().map(|p| p.bytes.len()).sum::<usize>() > 2 * MAX_PES {
                    return Err(LIMIT);
                }
            }
            let until = pending.values().map(|p| p.first).min().unwrap_or(slot + 1);
            self.emit(&mut window, until, &mut output, stop)?;
        }
        for (id, pes) in pending {
            let kind = self
                .clear
                .tracks
                .iter()
                .find(|t| t.pid == id)
                .ok_or(BAD)?
                .kind;
            Self::protect((id, kind), &pes, &mut window, key, iv, stop)?;
        }
        self.emit(&mut window, metadata.len() / 188, &mut output, stop)?;
        output.flush().map_err(|_| BAD)?;
        drop(output);
        let after = input.get_ref().metadata().map_err(|_| BAD)?;
        let reopened = files::regular_input(source)?;
        if after.len() != metadata.len()
            || after.modified().map_err(|_| BAD)? != modified
            || FileIdentity::read(&reopened).map_err(|_| BAD)? != identity
        {
            return Err(ConversionError("source-changed"));
        }
        checkpoint(stop)?;
        self.encrypted.segment(destination, stop)
    }
    fn admit(&mut self, source: &Path, stop: &Stop) -> Result<(Vec<u8>, u16), ConversionError> {
        self.clear.segment(source, stop)?;
        let audio = &mut self.audio;
        self.original
            .segment_with(source, stop, &mut |_, kind, bytes, _, _| {
                if kind == TrackKind::Audio {
                    canonical::Config::audio_adts(bytes)?;
                    let current = [
                        (((bytes[2] >> 6) + 1) << 3) | ((bytes[2] >> 2 & 15) >> 1),
                        ((bytes[2] >> 2 & 1) << 7)
                            | ((((bytes[2] & 1) << 2) | (bytes[3] >> 6)) << 3),
                    ];
                    if audio.is_some_and(|old| old != current) {
                        return Err(ConversionError("ts-program-or-configuration-changed"));
                    }
                    *audio = Some(current);
                }
                Ok(())
            })?;
        Ok((
            self.clear.sample_pmt(self.audio)?,
            self.clear.pmt_pid().ok_or(BAD)?,
        ))
    }
    fn protect(
        track: (u16, TrackKind),
        pes: &Pending,
        window: &mut Window,
        key: &Aes128Key,
        iv: &SegmentIv,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        checkpoint(stop)?;
        let (at, _, _) = admission::pes_content(&pes.bytes, track.1)?;
        let protected = match track.1 {
            TrackKind::Video => sample_aes::encrypt_h264_annex_b(&pes.bytes[at..], key, iv)?,
            TrackKind::Audio => sample_aes::encrypt_aac_adts(&pes.bytes[at..], key, iv)?,
            _ => return Err(BAD),
        };
        let length = at
            .checked_add(protected.len())
            .filter(|n| *n <= MAX_PES)
            .ok_or(LIMIT)?;
        let mut rewritten = Zeroizing::new(Vec::new());
        rewritten.try_reserve_exact(length).map_err(|_| LIMIT)?;
        rewritten.extend_from_slice(&pes.bytes[..at]);
        rewritten.extend_from_slice(&protected);
        if rewritten[4..6] != [0, 0] {
            let length = match u16::try_from(length - 6) {
                Ok(length) => length,
                Err(_) if track.1 == TrackKind::Video => 0,
                Err(_) => return Err(LIMIT),
            };
            rewritten[4..6].copy_from_slice(&length.to_be_bytes());
        }
        let mut position = 0;
        for (index, &slot) in pes.slots.iter().enumerate() {
            checkpoint(stop)?;
            let queued = window.get(slot)?;
            let capacity = 188 - minimum_payload_at(&queued.packet)?;
            let remaining_slots = pes.slots.len() - index - 1;
            let remaining = rewritten
                .len()
                .checked_sub(position + remaining_slots)
                .ok_or(BAD)?;
            let n = capacity.min(remaining);
            queued.packet = rewrite_payload(&queued.packet, &rewritten[position..position + n])?;
            position += n;
        }
        let last = *pes.slots.last().ok_or(BAD)?;
        let queued = window.get(last)?;
        while position < rewritten.len() {
            checkpoint(stop)?;
            if queued.extra.len() == MAX_SLOTS {
                return Err(LIMIT);
            }
            let end = (position + 184).min(rewritten.len());
            queued.extra.try_reserve(1).map_err(|_| LIMIT)?;
            queued.extra.push(Continuation {
                packet: Box::new(continuation_packet(track.0, &rewritten[position..end])?),
            });
            position = end;
        }
        Ok(())
    }
    fn emit(
        &mut self,
        window: &mut Window,
        until: u64,
        output: &mut impl std::io::Write,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        while window.first < until {
            checkpoint(stop)?;
            let mut slot = window.slots.pop_front().ok_or(BAD)?;
            self.write_packet(&mut slot.packet, output)?;
            for packet in &mut slot.extra {
                self.write_packet(&mut packet.packet, output)?;
            }
            window.first = window.first.checked_add(1).ok_or(LIMIT)?;
        }
        Ok(())
    }
    fn write_packet(
        &mut self,
        packet: &mut [u8; 188],
        output: &mut impl std::io::Write,
    ) -> Result<(), ConversionError> {
        let id = packet_pid(packet);
        if id != 0x1fff {
            let counter = match self.counters.get(&id) {
                Some(previous) if packet[3] & 0x10 != 0 => (previous + 1) & 15,
                Some(previous) => *previous,
                None => packet[3] & 15,
            };
            packet[3] = (packet[3] & 0xf0) | counter;
            self.counters.insert(id, counter);
        }
        output.write_all(packet).map_err(|_| BAD)
    }
    pub fn finish(self) -> Result<(), ConversionError> {
        if let Some(error) = self.failed {
            return Err(error);
        }
        self.clear.finish()?;
        self.original.finish()?;
        self.encrypted.finish()
    }
}

fn checkpoint(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}
fn packet_pid(packet: &[u8; 188]) -> u16 {
    (u16::from(packet[1] & 31) << 8) | u16::from(packet[2])
}
fn payload_at(packet: &[u8; 188]) -> Result<usize, ConversionError> {
    if packet[0] != 0x47 || packet[3] & 0x30 == 0 {
        return Err(BAD);
    }
    let at = if packet[3] & 0x20 != 0 {
        5 + usize::from(packet[4])
    } else {
        4
    };
    if at > 188 {
        return Err(BAD);
    }
    Ok(if packet[3] & 0x10 == 0 { 188 } else { at })
}
fn minimum_payload_at(packet: &[u8; 188]) -> Result<usize, ConversionError> {
    payload_at(packet)?;
    Ok(if packet[3] & 0x20 != 0 && packet[4] != 0 {
        if packet[5] & 0x10 != 0 { 12 } else { 6 }
    } else {
        4
    })
}
fn rewrite_payload(
    packet: &[u8; 188],
    bytes: &[u8],
) -> Result<Zeroizing<[u8; 188]>, ConversionError> {
    if bytes.is_empty() || bytes.len() > 188 - minimum_payload_at(packet)? {
        return Err(BAD);
    }
    let at = 188 - bytes.len();
    let mut out = Zeroizing::new([0xff; 188]);
    out[..4].copy_from_slice(&packet[..4]);
    out[3] = (out[3] & !0x30) | if at == 4 { 0x10 } else { 0x30 };
    if at > 4 {
        out[4] = u8::try_from(at - 5).map_err(|_| BAD)?;
        if at > 5 {
            out[5] = 0;
            let metadata = minimum_payload_at(packet)?;
            if metadata > 4 {
                out[5..metadata].copy_from_slice(&packet[5..metadata]);
            }
        }
    }
    out[at..].copy_from_slice(bytes);
    Ok(out)
}
fn continuation_packet(id: u16, bytes: &[u8]) -> Result<Zeroizing<[u8; 188]>, ConversionError> {
    let mut template = [0xff; 188];
    template[..4].copy_from_slice(&[
        0x47,
        u8::try_from(id >> 8).map_err(|_| BAD)?,
        u8::try_from(id & 255).map_err(|_| BAD)?,
        0x10,
    ]);
    rewrite_payload(&template, bytes)
}
fn null_packet() -> Zeroizing<[u8; 188]> {
    let mut packet = Zeroizing::new([0xff; 188]);
    packet[..4].copy_from_slice(&[0x47, 0x1f, 0xff, 0x10]);
    packet
}

#[cfg(test)]
mod tests;
