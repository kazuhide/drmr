//! Bounded fragment timing checks against manifest declarations, without reading media payloads.
use super::ConversionError;
use crate::mp4::{self, Mp4Box};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};
const BAD: ConversionError = ConversionError("fragment-timing-not-supported");
fn integer(data: &[u8], offset: usize) -> Result<u32, ConversionError> {
    Ok(u32::from_be_bytes(
        data.get(offset..offset + 4)
            .ok_or(BAD)?
            .try_into()
            .map_err(|_| BAD)?,
    ))
}
fn metadata(path: &Path) -> Result<Vec<u8>, ConversionError> {
    let mut file = File::open(path).map_err(|_| BAD)?;
    let length = file.metadata().map_err(|_| BAD)?.len();
    let mut at = 0u64;
    let mut out = Vec::new();
    for _ in 0..100_000 {
        if at == length {
            return Ok(out);
        }
        let mut head = [0u8; 8];
        file.read_exact(&mut head).map_err(|_| BAD)?;
        let small = integer(&head, 0)?;
        let mut bytes = head.to_vec();
        let size = match small {
            0 => length - at,
            1 => {
                let mut large = [0; 8];
                file.read_exact(&mut large).map_err(|_| BAD)?;
                bytes.extend(large);
                u64::from_be_bytes(large)
            }
            n => u64::from(n),
        };
        if size < bytes.len() as u64 || size > length - at {
            return Err(BAD);
        }
        if &head[4..] == b"emsg" {
            return Err(ConversionError("inband-events-not-supported"));
        }
        if matches!(&head[4..], b"moov" | b"moof") {
            let size = usize::try_from(size).map_err(|_| BAD)?;
            if size > 16 * 1024 * 1024 || out.len() + size > 16 * 1024 * 1024 {
                return Err(ConversionError("fragment-metadata-size-limit"));
            }
            let start = bytes.len();
            bytes.resize(size, 0);
            file.read_exact(&mut bytes[start..]).map_err(|_| BAD)?;
            if small == 0 {
                bytes[..4].copy_from_slice(&u32::try_from(size).map_err(|_| BAD)?.to_be_bytes());
            }
            out.extend(bytes);
        }
        at += size;
        file.seek(SeekFrom::Start(at)).map_err(|_| BAD)?;
    }
    Err(BAD)
}
fn unique(nodes: &[Mp4Box], kind: [u8; 4]) -> Result<&Mp4Box, ConversionError> {
    let mut values = nodes.iter().filter(|n| n.kind() == kind);
    let node = values.next().ok_or(BAD)?;
    if values.next().is_some() {
        return Err(BAD);
    }
    Ok(node)
}
/// One actual track's decode timeline, reused across its consecutive media segments.
pub(super) struct FragmentClock {
    track_id: u32,
    scale: u32,
    default_duration: u32,
    trim: u64,
    next: Option<u64>,
    finite: Option<super::finite::Window>,
    segment_first: Option<u64>,
    segment_end: u64,
    first_presented: Option<u64>,
    presented_end: u64,
}
impl FragmentClock {
    pub fn read(path: &Path) -> Result<Self, ConversionError> {
        let data = metadata(path)?;
        let nodes = mp4::parse_boxes(&data).map_err(|_| BAD)?;
        let moov = unique(&nodes, *b"moov")?;
        let trak = unique(&moov.children, *b"trak")?;
        let mdia = unique(&trak.children, *b"mdia")?;
        let mdhd = unique(&mdia.children, *b"mdhd")?.header.payload(&data);
        let scale = integer(
            mdhd,
            match mdhd.first() {
                Some(0) => 12,
                Some(1) => 20,
                _ => return Err(BAD),
            },
        )?;
        if scale == 0 {
            return Err(BAD);
        }
        let tkhd = unique(&trak.children, *b"tkhd")?.header.payload(&data);
        let track_id = integer(
            tkhd,
            match tkhd.first() {
                Some(0) => 12,
                Some(1) => 20,
                _ => return Err(BAD),
            },
        )?;
        let mut default_duration = 0;
        if let Some(mvex) = moov.children.iter().find(|n| n.kind() == *b"mvex") {
            let trex = unique(&mvex.children, *b"trex")?.header.payload(&data);
            if integer(trex, 4)? != track_id {
                return Err(BAD);
            }
            default_duration = integer(trex, 12)?;
        }
        let mut trim = 0;
        if unique(&mdia.children, *b"hdlr")?
            .header
            .payload(&data)
            .get(8..12)
            == Some(b"soun")
            && let Some(edts) = trak.children.iter().find(|n| n.kind() == *b"edts")
        {
            let elst = unique(&edts.children, *b"elst")?.header.payload(&data);
            let width = match elst.first() {
                Some(0) => 4,
                Some(1) => 8,
                _ => return Err(ConversionError("fragment-presentation-edit-not-supported")),
            };
            if elst.get(1..4) != Some(&[0, 0, 0])
                || integer(elst, 4)? != 1
                || elst.get(8 + width * 2..) != Some(&[0, 1, 0, 0])
            {
                return Err(ConversionError("fragment-presentation-edit-not-supported"));
            }
            let value = if width == 4 {
                i64::from(i32::from_be_bytes(
                    elst.get(12..16).ok_or(BAD)?.try_into().map_err(|_| BAD)?,
                ))
            } else {
                i64::from_be_bytes(elst.get(16..24).ok_or(BAD)?.try_into().map_err(|_| BAD)?)
            };
            trim = u64::try_from(value).map_err(|_| BAD)?;
        }
        Ok(Self {
            track_id,
            scale,
            default_duration,
            trim,
            next: None,
            finite: super::finite::window(path, 0)?,
            segment_first: None,
            segment_end: 0,
            first_presented: None,
            presented_end: 0,
        })
    }
    fn measure(&mut self, path: &Path) -> Result<(u64, u64), ConversionError> {
        self.segment_first = None;
        self.segment_end = 0;
        let data = metadata(path)?;
        let nodes = mp4::parse_boxes(&data).map_err(|_| BAD)?;
        let mut total = 0u64;
        let mut first = None;
        for moof in nodes.iter().filter(|n| n.kind() == *b"moof") {
            let traf = unique(&moof.children, *b"traf")?;
            let tfhd = unique(&traf.children, *b"tfhd")?.header.payload(&data);
            if integer(tfhd, 4)? != self.track_id {
                return Err(BAD);
            }
            let flags = integer(tfhd, 0)? & 0x00ff_ffff;
            let mut at = 8usize;
            if flags & 1 != 0 {
                at += 8;
            }
            if flags & 2 != 0 {
                at += 4;
            }
            let default = if flags & 8 != 0 {
                integer(tfhd, at)?
            } else {
                self.default_duration
            };
            let tfdt = unique(&traf.children, *b"tfdt")?.header.payload(&data);
            let base = match tfdt.first() {
                Some(0) => u64::from(integer(tfdt, 4)?),
                Some(1) => {
                    u64::from_be_bytes(tfdt.get(4..12).ok_or(BAD)?.try_into().map_err(|_| BAD)?)
                }
                _ => return Err(BAD),
            };
            first.get_or_insert(base);
            if self.next.is_some_and(|next| next != base) {
                return Err(ConversionError("fragment-timeline-gap-or-overlap"));
            }
            let mut duration = 0u64;
            let mut count = 0usize;
            for trun in traf.children.iter().filter(|n| n.kind() == *b"trun") {
                count = count
                    .checked_add(self.measure_run(
                        trun.header.payload(&data),
                        default,
                        base,
                        &mut duration,
                    )?)
                    .filter(|n| *n <= 100_000)
                    .ok_or(BAD)?;
            }
            if count == 0 {
                return Err(BAD);
            }
            self.next = Some(base.checked_add(duration).ok_or(BAD)?);
            total = total.checked_add(duration).ok_or(BAD)?;
        }
        Ok((first.ok_or(BAD)?, total))
    }
    fn measure_run(
        &mut self,
        payload: &[u8],
        default: u32,
        base: u64,
        duration: &mut u64,
    ) -> Result<usize, ConversionError> {
        let version = *payload.first().ok_or(BAD)?;
        if version > 1 {
            return Err(BAD);
        }
        let flags = integer(payload, 0)? & 0x00ff_ffff;
        let samples = usize::try_from(integer(payload, 4)?).map_err(|_| BAD)?;
        if samples > 100_000 {
            return Err(BAD);
        }
        let mut at = 8;
        if flags & 1 != 0 {
            at += 4;
        }
        if flags & 4 != 0 {
            at += 4;
        }
        for _ in 0..samples {
            let d = if flags & 256 != 0 {
                let value = integer(payload, at)?;
                at += 4;
                value
            } else {
                default
            };
            if d == 0 {
                return Err(BAD);
            }
            for flag in [512, 1024] {
                if flags & flag != 0 {
                    integer(payload, at)?;
                    at += 4;
                }
            }
            let offset = if flags & 2048 != 0 {
                let value = integer(payload, at)?;
                at += 4;
                if version == 0 {
                    i64::from(value)
                } else {
                    i64::from(i32::from_be_bytes(value.to_be_bytes()))
                }
            } else {
                0
            };
            let dts = base.checked_add(*duration).ok_or(BAD)?;
            self.record_presentation(dts, offset, d)?;
            *duration = duration.checked_add(u64::from(d)).ok_or(BAD)?;
        }
        if at != payload.len() {
            return Err(BAD);
        }
        Ok(samples)
    }
    fn record_presentation(
        &mut self,
        dts: u64,
        offset: i64,
        duration: u32,
    ) -> Result<(), ConversionError> {
        let pts = i128::from(dts) + i128::from(offset);
        let trim = i128::from(self.finite.map_or(self.trim, |w| w.start));
        let first = (pts - trim).max(0);
        let end = pts + i128::from(duration) - trim;
        let end = self.finite.map_or(end, |w| end.min(i128::from(w.duration)));
        if end > first {
            let first = u64::try_from(first).map_err(|_| BAD)?;
            let end = u64::try_from(end).map_err(|_| BAD)?;
            self.first_presented = Some(self.first_presented.map_or(first, |old| old.min(first)));
            self.presented_end = self.presented_end.max(end);
            self.segment_first = Some(self.segment_first.map_or(first, |old| old.min(first)));
            self.segment_end = self.segment_end.max(end);
        }
        Ok(())
    }
    pub(super) fn presentation_window(&self) -> Result<(u64, u64, u64), ConversionError> {
        if self
            .finite
            .is_some_and(|w| self.first_presented != Some(0) || self.presented_end != w.duration)
        {
            return Err(ConversionError("fragment-presentation-window-incomplete"));
        }
        Ok((
            self.first_presented.ok_or(BAD)?,
            self.presented_end,
            u64::from(self.scale),
        ))
    }
    pub(super) fn segment_presentation(
        &mut self,
        path: &Path,
    ) -> Result<(u64, u64, u64), ConversionError> {
        self.measure(path)?;
        Ok((
            self.segment_first.ok_or(BAD)?,
            self.segment_end,
            u64::from(self.scale),
        ))
    }
    pub fn check_dash(
        &mut self,
        path: &Path,
        time: u64,
        duration: u64,
        scale: u64,
    ) -> Result<(), ConversionError> {
        let (base, total) = self.measure(path)?;
        let (base, total) = if self.finite.is_some() {
            let first = self.segment_first.ok_or(BAD)?;
            (first, self.segment_end.checked_sub(first).ok_or(BAD)?)
        } else {
            let end = base
                .checked_add(total)
                .ok_or(BAD)?
                .saturating_sub(self.trim);
            let base = base.saturating_sub(self.trim);
            (base, end.checked_sub(base).ok_or(BAD)?)
        };
        if u128::from(base) * u128::from(scale) != u128::from(time) * u128::from(self.scale)
            || u128::from(total) * u128::from(scale)
                != u128::from(duration) * u128::from(self.scale)
        {
            return Err(ConversionError("manifest-fragment-timeline-mismatch"));
        }
        Ok(())
    }
    pub fn check(&mut self, path: &Path, declared: f64) -> Result<(), ConversionError> {
        let (base, total) = self.measure(path)?;
        let total = if self.finite.is_some() {
            self.segment_end
                .checked_sub(self.segment_first.ok_or(BAD)?)
                .ok_or(BAD)?
        } else {
            base.checked_add(total)
                .ok_or(BAD)?
                .saturating_sub(self.trim)
                .checked_sub(base.saturating_sub(self.trim))
                .ok_or(BAD)?
        };
        if self.finite.is_some() {
            let canonical = super::finite::manifest::seconds(total, u64::from(self.scale))?
                .parse::<f64>()
                .map_err(|_| BAD)?;
            // Both values are parsed from decimal declarations; require the canonical value, not a tolerance.
            return if declared.to_bits() == canonical.to_bits() {
                Ok(())
            } else {
                Err(ConversionError("manifest-segment-duration-mismatch"))
            };
        }
        let actual =
            f64::from(u32::try_from(total).map_err(|_| ConversionError("segment-duration-limit"))?)
                / f64::from(self.scale);
        let tolerance = (1.0 / f64::from(self.scale)).max(0.001_001);
        if !declared.is_finite() || (declared - actual).abs() > tolerance {
            return Err(ConversionError("manifest-segment-duration-mismatch"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn clock(trim: u64) -> FragmentClock {
        FragmentClock {
            track_id: 1,
            scale: 90_000,
            default_duration: 0,
            trim,
            next: None,
            finite: None,
            segment_first: None,
            segment_end: 0,
            first_presented: None,
            presented_end: 0,
        }
    }
    fn run(version: u8, offsets: &[u32], duration: u32) -> Vec<u8> {
        let mut data = vec![version, 0, 9, 0];
        data.extend(u32::try_from(offsets.len()).unwrap().to_be_bytes());
        for offset in offsets {
            data.extend(duration.to_be_bytes());
            data.extend(offset.to_be_bytes());
        }
        data
    }
    #[test]
    fn presented_bounds_inspect_every_reordered_sample_and_signed_composition_offset() {
        let mut c = clock(0);
        let mut duration = 0;
        let negative = u32::from_be_bytes((-20_i32).to_be_bytes());
        assert_eq!(
            c.measure_run(&run(1, &[50, negative], 10), 0, 100, &mut duration)
                .unwrap(),
            2
        );
        assert_eq!(duration, 20);
        assert_eq!(c.presentation_window().unwrap(), (90, 160, 90_000));
        let mut unsigned = clock(0);
        unsigned
            .measure_run(&run(0, &[negative], 10), 0, 100, &mut 0)
            .unwrap();
        assert_eq!(
            unsigned.presentation_window().unwrap().0,
            100 + u64::from(negative)
        );
        assert!(
            clock(0)
                .measure_run(&run(2, &[0], 10), 0, 100, &mut 0)
                .is_err()
        );
    }
    #[test]
    fn priming_edit_is_applied_once_and_excluded_coded_intervals_do_not_set_origin() {
        let mut c = clock(1024);
        c.measure_run(&run(0, &[0, 0], 1024), 0, 0, &mut 0).unwrap();
        assert_eq!(c.presentation_window().unwrap(), (0, 1024, 90_000));
    }
}
