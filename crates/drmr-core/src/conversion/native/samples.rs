//! Bounded sample digests keep packet boundaries and exact rational clocks.
use super::super::{ConversionError, native_media::SampleInfo, ts::CanonicalConfig};
use crate::sha256::Sha256;
use std::cmp::Ordering;
use zeroize::Zeroizing;

const BAD: ConversionError = ConversionError("native-sample-evidence-invalid");
const OVERFLOW: ConversionError = ConversionError("native-media-clock-overflow");
const MAX_CANONICAL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Ratio {
    pub numerator: i128,
    pub denominator: u64,
}
impl Ratio {
    pub fn new(numerator: i128, denominator: u64) -> Result<Self, ConversionError> {
        if denominator == 0 {
            return Err(BAD);
        }
        let mut a = numerator.unsigned_abs();
        let mut b = u128::from(denominator);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        let divisor = u64::try_from(a).map_err(|_| OVERFLOW)?;
        Ok(Self {
            numerator: numerator / i128::from(divisor),
            denominator: denominator / divisor,
        })
    }
    pub fn ticks(ticks: i64, scale: u32) -> Result<Self, ConversionError> {
        Self::new(i128::from(ticks), u64::from(scale))
    }
    pub fn compare(self, other: Self) -> Result<Ordering, ConversionError> {
        let left = self
            .numerator
            .checked_mul(i128::from(other.denominator))
            .ok_or(OVERFLOW)?;
        let right = other
            .numerator
            .checked_mul(i128::from(self.denominator))
            .ok_or(OVERFLOW)?;
        Ok(left.cmp(&right))
    }
    pub fn subtract(self, other: Self) -> Result<Self, ConversionError> {
        Self::new(
            self.numerator
                .checked_mul(i128::from(other.denominator))
                .and_then(|left| {
                    other
                        .numerator
                        .checked_mul(i128::from(self.denominator))
                        .and_then(|right| left.checked_sub(right))
                })
                .ok_or(OVERFLOW)?,
            self.denominator
                .checked_mul(other.denominator)
                .ok_or(OVERFLOW)?,
        )
    }
    fn hash(self, hash: &mut Sha256) {
        hash.update(self.numerator.to_be_bytes());
        hash.update(self.denominator.to_be_bytes());
    }
    pub(super) fn transport_ticks(self) -> Result<Option<i64>, ConversionError> {
        let scaled = self.numerator.checked_mul(90_000).ok_or(OVERFLOW)?;
        if scaled % i128::from(self.denominator) != 0 {
            return Ok(None);
        }
        Ok(Some(
            i64::try_from(scaled / i128::from(self.denominator)).map_err(|_| OVERFLOW)?,
        ))
    }
}

pub(in crate::conversion) struct SampleEvidence {
    pub count: u64,
    pub raw_payload: [u8; 32],
    pub canonical_payload: Option<[u8; 32]>,
    pub clocks: [u8; 32],
    pub presentation_clocks: [u8; 32],
    pub presentation_offset: Ratio,
    pub transport_clocks: Option<[u8; 32]>,
    pub durations: [u8; 32],
    pub coded_duration: Ratio,
    pub sync_samples: [u8; 32],
    pub recovery: [u8; 32],
    pub transport_recovery_compatible: bool,
    pub initial_composition_offset: Ratio,
    pub first_pts: Ratio,
    pub end_pts: Ratio,
    pub presented_first_pts: Ratio,
    pub presented_end_pts: Ratio,
    pub continuous_decode: bool,
    pub protected_samples: u64,
}
impl SampleEvidence {
    pub fn clocks_equal(&self, other: &Self) -> bool {
        if self.presentation_offset == other.presentation_offset {
            self.clocks == other.clocks
        } else {
            self.presentation_clocks == other.presentation_clocks
        }
    }
}

struct Pending {
    info: SampleInfo,
    received: u32,
    canonical_bytes: Zeroizing<Vec<u8>>,
}

pub(super) struct Samples {
    scale: u32,
    expected: u64,
    count: u64,
    raw: Sha256,
    canonical: Option<(CanonicalConfig, u8, Sha256)>,
    avc: Option<super::super::ts::avc::Configuration>,
    audio_clock: Option<super::audio_clock::AudioClock>,
    clocks: Sha256,
    presentation_clocks: Sha256,
    presentation_offset: Ratio,
    transport_clocks: Option<Sha256>,
    durations: Sha256,
    duration_ticks: u128,
    sync: Sha256,
    recovery: Sha256,
    transport_recovery_compatible: bool,
    initial_composition_offset: Option<Ratio>,
    first_pts: Option<Ratio>,
    end_pts: Option<Ratio>,
    next_dts: Option<i128>,
    continuous: bool,
    protected: u64,
    pending: Option<Pending>,
    failed: bool,
}
impl Samples {
    pub fn new(
        scale: u32,
        expected: u64,
        canonical: Option<(CanonicalConfig, u8)>,
    ) -> Result<Self, ConversionError> {
        if scale == 0 || expected == 0 {
            return Err(BAD);
        }
        let avc = match &canonical {
            Some((CanonicalConfig::Video { sps, pps }, _)) => {
                Some(super::super::ts::avc::Configuration::parse(sps, pps)?)
            }
            _ => None,
        };
        Ok(Self {
            scale,
            expected,
            count: 0,
            raw: Sha256::new(),
            canonical: canonical.map(|(config, width)| (config, width, Sha256::new())),
            avc,
            audio_clock: None,
            clocks: Sha256::new(),
            presentation_clocks: Sha256::new(),
            presentation_offset: Ratio::new(0, 1)?,
            transport_clocks: Some(Sha256::new()),
            durations: Sha256::new(),
            duration_ticks: 0,
            sync: Sha256::new(),
            recovery: Sha256::new(),
            transport_recovery_compatible: true,
            initial_composition_offset: None,
            first_pts: None,
            end_pts: None,
            next_dts: None,
            continuous: true,
            protected: 0,
            pending: None,
            failed: false,
        })
    }
    pub fn push(&mut self, info: &SampleInfo, bytes: &[u8]) -> Result<(), ConversionError> {
        if self.failed {
            return Err(BAD);
        }
        let result = self.push_checked(info, bytes);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    pub fn with_audio_clock(mut self, samples: u16, rate: u32) -> Result<Self, ConversionError> {
        self.audio_clock = Some(super::audio_clock::AudioClock::new(
            samples, rate, self.scale,
        )?);
        Ok(self)
    }
    pub(super) fn with_manifest_audio_clock(mut self) -> Result<Self, ConversionError> {
        self.audio_clock = Some(super::audio_clock::AudioClock::manifested()?);
        Ok(self)
    }
    pub fn with_presentation_offset(mut self, offset: Ratio) -> Self {
        self.presentation_offset = offset;
        self
    }
    fn push_checked(&mut self, info: &SampleInfo, bytes: &[u8]) -> Result<(), ConversionError> {
        if bytes.is_empty() || bytes.len() > super::super::native_media::CHUNK_BYTES {
            return Err(BAD);
        }
        if self.pending.is_none() {
            if info.ordinal != self.count
                || self.count >= self.expected
                || info.chunk_offset != 0
                || info.size == 0
                || info.duration == 0
                || info.range.end.checked_sub(info.range.start) != Some(u64::from(info.size))
            {
                return Err(BAD);
            }
            // Lengths make [ab,c] distinct from [a,bc] without retaining the asset.
            self.raw.update(b"raw-sample");
            self.raw.update(u64::from(info.size).to_be_bytes());
            if usize::try_from(info.size).map_err(|_| BAD)? > MAX_CANONICAL_BYTES {
                if self.avc.is_some() {
                    return Err(ConversionError("native-avc-access-unit-size-limit"));
                }
                self.canonical = None;
            }
            self.pending = Some(Pending {
                info: info.clone(),
                received: 0,
                canonical_bytes: Zeroizing::new(Vec::new()),
            });
        }
        let pending = self.pending.as_mut().ok_or(BAD)?;
        if !same_sample(&pending.info, info) || info.chunk_offset != pending.received {
            return Err(BAD);
        }
        pending.received = pending
            .received
            .checked_add(u32::try_from(bytes.len()).map_err(|_| BAD)?)
            .filter(|n| *n <= info.size)
            .ok_or(BAD)?;
        self.raw.update(bytes);
        if self.canonical.is_some() {
            if pending.canonical_bytes.capacity() == 0 {
                pending
                    .canonical_bytes
                    .try_reserve_exact(usize::try_from(info.size).map_err(|_| BAD)?)
                    .map_err(|_| BAD)?;
            }
            pending.canonical_bytes.extend_from_slice(bytes);
        }
        if info.is_last_chunk != (pending.received == info.size) {
            return Err(BAD);
        }
        if !info.is_last_chunk {
            return Ok(());
        }
        let pending = self.pending.take().ok_or(BAD)?;
        if let Some((config, width, hash)) = &mut self.canonical {
            if let Some(avc) = &self.avc {
                let unit = avc.length_prefixed_access_unit(&pending.canonical_bytes, *width)?;
                let independent = unit.is_idr
                    || unit.recovery_point.is_some_and(|recovery| {
                        recovery.frame_count == 0
                            && recovery.exact_match
                            && !recovery.broken_link
                            && recovery.changing_slice_group_idc == 0
                    });
                if info.is_sync != independent {
                    return Err(ConversionError("native-avc-sync-sample-mismatch"));
                }
            }
            hash.update(b"access-unit");
            config.units_with_width(&pending.canonical_bytes, false, *width, hash)?;
        }
        self.record_clock(info)
    }
    fn record_clock(&mut self, info: &SampleInfo) -> Result<(), ConversionError> {
        if let Some(clock) = &mut self.audio_clock {
            clock.record(info, info.ordinal.checked_add(1) == Some(self.expected))?;
        }
        let pts = Ratio::ticks(info.pts, self.scale)?;
        let dts = Ratio::ticks(info.dts, self.scale)?;
        if self.count == 0 {
            self.initial_composition_offset = Some(Ratio::new(
                i128::from(info.pts) - i128::from(info.dts),
                u64::from(self.scale),
            )?);
        }
        pts.hash(&mut self.clocks);
        dts.hash(&mut self.clocks);
        let presentation_time = pts.subtract(self.presentation_offset)?;
        let decode_time = dts.subtract(self.presentation_offset)?;
        presentation_time.hash(&mut self.presentation_clocks);
        decode_time.hash(&mut self.presentation_clocks);
        match (
            presentation_time.transport_ticks()?,
            decode_time.transport_ticks()?,
        ) {
            (Some(pts), Some(dts)) => {
                if let Some(hash) = &mut self.transport_clocks {
                    hash.update(pts.to_be_bytes());
                    hash.update(dts.to_be_bytes());
                }
            }
            _ => self.transport_clocks = None,
        }
        Ratio::new(i128::from(info.duration), u64::from(self.scale))?.hash(&mut self.durations);
        self.duration_ticks = self
            .duration_ticks
            .checked_add(u128::from(info.duration))
            .ok_or(OVERFLOW)?;
        self.sync.update([u8::from(info.is_sync)]);
        self.transport_recovery_compatible &= (self.avc.is_some() || info.is_sync)
            && info.roll_distance.is_none_or(|distance| distance == 0)
            && info.preroll_distance.is_none_or(|distance| distance == 0);
        for distance in [info.roll_distance, info.preroll_distance] {
            self.recovery.update([u8::from(distance.is_some())]);
            self.recovery
                .update(distance.unwrap_or_default().to_be_bytes());
        }
        let end = Ratio::new(
            i128::from(info.pts) + i128::from(info.duration),
            u64::from(self.scale),
        )?;
        if match self.first_pts {
            Some(old) => pts.compare(old)? == Ordering::Less,
            None => true,
        } {
            self.first_pts = Some(pts);
        }
        if match self.end_pts {
            Some(old) => end.compare(old)? == Ordering::Greater,
            None => true,
        } {
            self.end_pts = Some(end);
        }
        self.continuous &= self
            .next_dts
            .is_none_or(|next| next == i128::from(info.dts));
        self.next_dts = Some(i128::from(info.dts) + i128::from(info.duration));
        self.protected = self
            .protected
            .checked_add(u64::from(matches!(
                info.protection,
                super::super::native_media::SampleProtection::Encrypted { .. }
            )))
            .ok_or(BAD)?;
        self.count = self.count.checked_add(1).ok_or(BAD)?;
        Ok(())
    }
    pub fn finish(self) -> Result<SampleEvidence, ConversionError> {
        if self.failed || self.pending.is_some() || self.count != self.expected || self.count == 0 {
            return Err(BAD);
        }
        Ok(SampleEvidence {
            count: self.count,
            raw_payload: self.raw.finalize(),
            canonical_payload: self.canonical.map(|(_, _, hash)| hash.finalize()),
            clocks: self.clocks.finalize(),
            presentation_clocks: self.presentation_clocks.finalize(),
            presentation_offset: self.presentation_offset,
            transport_clocks: self.transport_clocks.map(Sha256::finalize),
            durations: self.durations.finalize(),
            coded_duration: Ratio::new(
                i128::try_from(self.duration_ticks).map_err(|_| OVERFLOW)?,
                u64::from(self.scale),
            )?,
            sync_samples: self.sync.finalize(),
            recovery: self.recovery.finalize(),
            transport_recovery_compatible: self.transport_recovery_compatible,
            initial_composition_offset: self.initial_composition_offset.ok_or(BAD)?,
            first_pts: self.first_pts.ok_or(BAD)?,
            end_pts: self.end_pts.ok_or(BAD)?,
            presented_first_pts: self
                .first_pts
                .ok_or(BAD)?
                .subtract(self.presentation_offset)?,
            presented_end_pts: self
                .end_pts
                .ok_or(BAD)?
                .subtract(self.presentation_offset)?,
            continuous_decode: self.continuous,
            protected_samples: self.protected,
        })
    }
}

fn same_sample(a: &SampleInfo, b: &SampleInfo) -> bool {
    a.ordinal == b.ordinal
        && a.dts == b.dts
        && a.pts == b.pts
        && a.duration == b.duration
        && a.size == b.size
        && a.range == b.range
        && a.flags == b.flags
        && a.is_sync == b.is_sync
        && a.roll_distance == b.roll_distance
        && a.preroll_distance == b.preroll_distance
        && a.protection == b.protection
}

#[cfg(test)]
mod tests {
    use super::*;
    fn info(ordinal: u64, size: u32, pts: i64) -> SampleInfo {
        SampleInfo {
            ordinal,
            dts: ordinal.cast_signed() * 100,
            pts,
            duration: 100,
            size,
            range: 0..u64::from(size),
            flags: 0,
            is_sync: ordinal == 0,
            roll_distance: None,
            preroll_distance: None,
            protection: super::super::super::native_media::SampleProtection::Clear,
            chunk_offset: 0,
            is_last_chunk: true,
        }
    }
    #[test]
    fn payload_boundaries_order_and_clocks_are_part_of_the_evidence() {
        let collect = |parts: &[&[u8]]| {
            let mut data = Samples::new(1_000, 2, None).unwrap();
            for (index, bytes) in parts.iter().enumerate() {
                data.push(
                    &info(
                        u64::try_from(index).unwrap(),
                        u32::try_from(bytes.len()).unwrap(),
                        i64::try_from(index).unwrap() * 100,
                    ),
                    bytes,
                )
                .unwrap();
            }
            data.finish().unwrap()
        };
        let first = collect(&[b"ab", b"c"]);
        let changed = collect(&[b"a", b"bc"]);
        assert_ne!(first.raw_payload, changed.raw_payload);
        assert_eq!(first.clocks, changed.clocks);
        let swapped = collect(&[b"c", b"ab"]);
        assert_ne!(first.raw_payload, swapped.raw_payload);
        let mut reordered = Samples::new(1_000, 2, None).unwrap();
        reordered.push(&info(0, 2, 100), b"ab").unwrap();
        reordered.push(&info(1, 1, 0), b"c").unwrap();
        let reordered = reordered.finish().unwrap();
        assert_ne!(first.clocks, reordered.clocks);
        assert_eq!(reordered.first_pts, Ratio::new(0, 1).unwrap());
        assert_eq!(
            reordered.initial_composition_offset,
            Ratio::new(1, 10).unwrap()
        );
        assert_eq!(reordered.end_pts, Ratio::new(1, 5).unwrap());
    }
    #[test]
    fn edits_map_all_clocks_without_discarding_raw_timing_or_b_frame_order() {
        let collect = |shift: i64, edit: i64| {
            let mut samples = Samples::new(1_000, 2, None)
                .unwrap()
                .with_presentation_offset(Ratio::ticks(edit, 1_000).unwrap());
            for (ordinal, pts) in [(0, 100), (1, 0)] {
                let mut packet = info(ordinal, 1, pts + shift);
                packet.dts += shift;
                samples.push(&packet, b"a").unwrap();
            }
            samples.finish().unwrap()
        };
        let source = collect(0, 0);
        let mapped = collect(75, 75);
        assert_ne!(source.clocks, mapped.clocks);
        assert!(source.clocks_equal(&mapped));
        assert_eq!(source.presented_first_pts, mapped.presented_first_pts);
        assert_eq!(source.presented_end_pts, mapped.presented_end_pts);
        assert_eq!(source.transport_clocks, mapped.transport_clocks);
        assert_eq!(
            source.initial_composition_offset,
            mapped.initial_composition_offset
        );
        assert!(!source.clocks_equal(&collect(75, 76)));
        assert!(!source.clocks_equal(&collect(75, 0)));
    }
    #[test]
    fn incomplete_changed_or_replayed_chunks_cannot_finish() {
        let mut data = Samples::new(1_000, 1, None).unwrap();
        let mut sample = info(0, 2, 0);
        sample.is_last_chunk = false;
        data.push(&sample, b"a").unwrap();
        assert!(data.finish().is_err());
        let mut data = Samples::new(1_000, 1, None).unwrap();
        data.push(&sample, b"a").unwrap();
        sample.chunk_offset = 1;
        sample.pts = 1;
        sample.is_last_chunk = true;
        assert!(data.push(&sample, b"b").is_err());
        assert!(data.finish().is_err());
        let mut data = Samples::new(1_000, 1, None).unwrap();
        data.push(&info(0, 1, 0), b"a").unwrap();
        assert!(data.push(&info(0, 1, 0), b"a").is_err());
        assert!(data.finish().is_err());
    }
    #[test]
    fn clock_units_are_exact_and_overflow_is_an_error() {
        assert_eq!(
            Ratio::ticks(1_920, 90_000).unwrap(),
            Ratio::ticks(1_024, 48_000).unwrap()
        );
        assert_eq!(
            Ratio::ticks(-1, 2).unwrap().transport_ticks().unwrap(),
            Some(-45_000)
        );
        assert_eq!(
            Ratio::ticks(1_024, 44_100)
                .unwrap()
                .transport_ticks()
                .unwrap(),
            None
        );
        assert!(Ratio::new(1, 0).is_err());
        assert!(
            Ratio::new(i128::MAX, 1)
                .unwrap()
                .compare(Ratio::new(1, 2).unwrap())
                .is_err()
        );
    }
}
