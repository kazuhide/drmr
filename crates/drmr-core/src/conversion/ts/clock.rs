//! AAC clocks stay integral in the 90 kHz transport timescale.
use super::super::ConversionError;

const BAD: ConversionError = ConversionError("ts-clock-wrap-or-discontinuity-not-supported");
const FRAME_TICKS_NUMERATOR: i128 = 1024 * 90_000;

fn nearest(numerator: i128, denominator: i128) -> Result<i128, ConversionError> {
    if numerator < 0 || denominator <= 0 {
        return Err(BAD);
    }
    numerator
        .checked_add(denominator / 2)
        .map(|n| n / denominator)
        .ok_or(BAD)
}

/// Advance one decoded AAC frame using the absolute rational clock phase.
/// This is the integer rounding rule used by `FFprobe`'s stable audio clock;
/// repeatedly adding the truncated packet duration would accumulate drift.
pub(in crate::conversion) fn next(ticks: i64, sample_rate: u32) -> Result<i64, ConversionError> {
    if ticks < 0 || !matches!(sample_rate, 44_100 | 48_000) {
        return Err(BAD);
    }
    let rate = i128::from(sample_rate);
    let ticks = i128::from(ticks);
    let old_frame = nearest(ticks.checked_mul(rate).ok_or(BAD)?, FRAME_TICKS_NUMERATOR)?;
    let old_ticks = nearest(
        old_frame.checked_mul(FRAME_TICKS_NUMERATOR).ok_or(BAD)?,
        rate,
    )?;
    let next_ticks = nearest(
        old_frame
            .checked_add(1)
            .and_then(|n| n.checked_mul(FRAME_TICKS_NUMERATOR))
            .ok_or(BAD)?,
        rate,
    )?;
    i64::try_from(
        next_ticks
            .checked_add(ticks.checked_sub(old_ticks).ok_or(BAD)?)
            .ok_or(BAD)?,
    )
    .map_err(|_| BAD)
}

#[derive(Clone, Copy)]
struct Phase {
    low: i128,
    high: i128,
}

/// Source admission only. Exact frame clocks are hashed separately and must
/// match conversion results; this interval is never an output-equality tolerance.
#[derive(Clone)]
pub(in crate::conversion) struct Audio {
    sample_rate: u32,
    frames: u64,
    next: Option<(i64, i64)>,
    pts_phase: Option<Phase>,
    dts_phase: Option<Phase>,
}
impl Audio {
    pub(in crate::conversion) fn new(sample_rate: u32) -> Result<Self, ConversionError> {
        if !matches!(sample_rate, 44_100 | 48_000) {
            return Err(BAD);
        }
        Ok(Self {
            sample_rate,
            frames: 0,
            next: None,
            pts_phase: None,
            dts_phase: None,
        })
    }

    pub(in crate::conversion) fn begin(
        &mut self,
        pts: i64,
        dts: i64,
    ) -> Result<(), ConversionError> {
        if dts < 0 || pts < dts {
            return Err(BAD);
        }
        if self.sample_rate == 48_000 {
            if self.next.is_some_and(|expected| expected != (pts, dts)) {
                return Err(BAD);
            }
            return Ok(());
        }
        // A raw PES timestamp is rounded to integer transport ticks. Frames
        // interpolated inside that PES undergo one further integer rounding.
        // Repackaging those exact frame clocks as one-frame PES retains both
        // quantizations. One shared phase interval bounds both steps for the
        // entire track, so permitted rounding cannot accumulate with duration.
        let ticks = i128::from(self.frames)
            .checked_mul(FRAME_TICKS_NUMERATOR)
            .ok_or(BAD)?;
        Self::intersect(&mut self.pts_phase, pts, ticks, self.sample_rate)?;
        Self::intersect(&mut self.dts_phase, dts, ticks, self.sample_rate)
    }

    fn intersect(
        phase: &mut Option<Phase>,
        ticks: i64,
        frame_ticks: i128,
        sample_rate: u32,
    ) -> Result<(), ConversionError> {
        let rate = i128::from(sample_rate);
        let offset = i128::from(ticks)
            .checked_mul(rate)
            .and_then(|n| n.checked_sub(frame_ticks))
            .ok_or(BAD)?;
        let observed = Phase {
            low: offset.checked_sub(rate).ok_or(BAD)?,
            high: offset.checked_add(rate).ok_or(BAD)?,
        };
        let intersection = phase.map_or(observed, |old| Phase {
            low: old.low.max(observed.low),
            high: old.high.min(observed.high),
        });
        if intersection.low >= intersection.high {
            return Err(BAD);
        }
        *phase = Some(intersection);
        Ok(())
    }

    pub(in crate::conversion) fn frame(
        &mut self,
        pts: i64,
        dts: i64,
    ) -> Result<(i64, i64), ConversionError> {
        self.frames = self.frames.checked_add(1).ok_or(BAD)?;
        let following = (next(pts, self.sample_rate)?, next(dts, self.sample_rate)?);
        self.next = Some(following);
        Ok(following)
    }
}

/// A single codec clock phase covers every decode and presentation timestamp.
/// Reordering retains at most a bounded decoder window; no full-track PTS list
/// is needed. Exact timestamp equality is still hashed independently.
#[derive(Clone)]
pub(super) struct Video {
    numerator: i128,
    denominator: i128,
    frames: u64,
    first_dts: Option<i64>,
    phase: Option<Phase>,
    pending: std::collections::BTreeMap<u64, i64>,
    next_presentation: Option<u64>,
    presented: u64,
}
impl Video {
    pub(super) fn new(clock: super::avc::VideoClock) -> Result<Self, ConversionError> {
        let numerator = i128::from(clock.num_units_in_tick)
            .checked_mul(180_000)
            .ok_or(BAD)?;
        let denominator = i128::from(clock.time_scale);
        if numerator == 0 || denominator == 0 || numerator / denominator < 2 {
            return Err(BAD);
        }
        Ok(Self {
            numerator,
            denominator,
            frames: 0,
            first_dts: None,
            phase: None,
            pending: std::collections::BTreeMap::new(),
            next_presentation: None,
            presented: 0,
        })
    }
    pub(super) fn frame(&mut self, pts: i64, dts: i64) -> Result<(), ConversionError> {
        if dts < 0 || pts < dts {
            return Err(BAD);
        }
        let first = *self.first_dts.get_or_insert(dts);
        let offset = i128::from(pts).checked_sub(i128::from(first)).ok_or(BAD)?;
        let slot = nearest(
            offset.checked_mul(self.denominator).ok_or(BAD)?,
            self.numerator,
        )?;
        let slot = u64::try_from(slot).map_err(|_| BAD)?;
        self.observe(dts, self.frames)?;
        self.observe(pts, slot)?;
        if self.next_presentation.is_some_and(|next| slot < next)
            || self.pending.insert(slot, pts).is_some()
        {
            return Err(BAD);
        }
        self.frames = self.frames.checked_add(1).ok_or(BAD)?;
        self.flush(Some(dts))?;
        if self.pending.len() > 1024 {
            return Err(ConversionError("ts-video-reorder-limit"));
        }
        Ok(())
    }
    fn observe(&mut self, ticks: i64, ordinal: u64) -> Result<(), ConversionError> {
        let offset = i128::from(ticks)
            .checked_mul(self.denominator)
            .and_then(|n| n.checked_sub(i128::from(ordinal).checked_mul(self.numerator)?))
            .and_then(|n| n.checked_mul(2))
            .ok_or(BAD)?;
        // Half-open nearest-integer intervals preserve tie direction without
        // admitting accumulated drift or using an output-equality tolerance.
        let observed = Phase {
            low: offset.checked_sub(self.denominator).ok_or(BAD)?,
            high: offset.checked_add(self.denominator).ok_or(BAD)?,
        };
        let intersection = self.phase.map_or(observed, |old| Phase {
            low: old.low.max(observed.low),
            high: old.high.min(observed.high),
        });
        if intersection.low >= intersection.high {
            return Err(BAD);
        }
        self.phase = Some(intersection);
        Ok(())
    }
    fn flush(&mut self, before: Option<i64>) -> Result<(), ConversionError> {
        while let Some((&slot, &pts)) = self.pending.first_key_value() {
            if before.is_some_and(|limit| pts >= limit) {
                break;
            }
            if self.next_presentation.is_some_and(|next| slot != next) {
                return Err(BAD);
            }
            self.next_presentation = Some(slot.checked_add(1).ok_or(BAD)?);
            self.presented = self.presented.checked_add(1).ok_or(BAD)?;
            self.pending.remove(&slot);
        }
        Ok(())
    }
    pub(super) fn finish(&self) -> Result<(), ConversionError> {
        if self.frames == 0 {
            return Err(BAD);
        }
        let mut completed = self.clone();
        completed.flush(None)?;
        if completed.presented != self.frames {
            return Err(BAD);
        }
        Ok(())
    }
    /// Nominal packet duration follows the explicit codec clock. A VUI that
    /// does not assert fixed rate also requires original manifest duration
    /// evidence; the caller must not treat this candidate as that evidence.
    pub(super) fn nominal_duration(&self) -> Result<i64, ConversionError> {
        i64::try_from(self.numerator / self.denominator).map_err(|_| BAD)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_interpolation_retains_the_absolute_clock_phase() {
        let mut pts = 126_000;
        for expected in [128_090, 130_179, 132_269, 134_359, 136_449, 138_539] {
            pts = next(pts, 44_100).unwrap();
            assert_eq!(pts, expected);
        }
        let mut pts = 140_629;
        for expected in [142_719, 144_809, 146_899, 148_989, 151_078, 153_168] {
            pts = next(pts, 44_100).unwrap();
            assert_eq!(pts, expected);
        }
        assert_eq!(next(90_000, 48_000).unwrap(), 91_920);
        assert!(next(-1, 44_100).is_err());
        assert!(next(i64::MAX, 44_100).is_err());
        assert!(next(90_000, 32_000).is_err());
    }

    #[test]
    fn source_rounding_is_track_wide_and_cannot_accumulate_drift() {
        let mut clock = Audio::new(44_100).unwrap();
        for index in 0_i64..500 {
            let pts = 900_000 + (index * 1024 * 90_000 + 22_050) / 44_100;
            clock.begin(pts, pts).unwrap();
            clock.frame(pts, pts).unwrap();
        }
        let pts = 900_000 + (500 * 1024 * 90_000 + 22_050) / 44_100 + 2;
        assert!(clock.begin(pts, pts).is_err());

        let mut clock = Audio::new(44_100).unwrap();
        for index in 0_i64..100 {
            let pts = 900_000 + index * 2089;
            if clock.begin(pts, pts).is_err() {
                assert!(index < 10);
                return;
            }
            clock.frame(pts, pts).unwrap();
        }
        panic!("truncated duration drift was admitted");
    }

    #[test]
    fn integral_audio_clocks_still_reject_a_single_tick_change() {
        let mut clock = Audio::new(48_000).unwrap();
        clock.begin(90_000, 90_000).unwrap();
        clock.frame(90_000, 90_000).unwrap();
        assert!(clock.begin(91_919, 91_920).is_err());
        assert!(clock.begin(91_920, 91_921).is_err());
        clock.begin(91_920, 91_920).unwrap();
    }

    fn video_clock(units: u32, scale: u32) -> super::super::avc::VideoClock {
        super::super::avc::VideoClock {
            num_units_in_tick: units,
            time_scale: scale,
            fixed_frame_rate: false,
            pic_struct_present: false,
        }
    }
    #[test]
    fn bframe_presentation_order_is_complete_even_when_the_first_pts_is_not_minimal() {
        let mut video = Video::new(video_clock(1, 48)).unwrap();
        for (index, pts) in [97_500, 90_000, 93_750, 101_250].into_iter().enumerate() {
            video
                .frame(pts, 82_500 + i64::try_from(index).unwrap() * 3750)
                .unwrap();
        }
        video.finish().unwrap();
        assert_eq!(video.nominal_duration().unwrap(), 3750);
        let mut video = Video::new(video_clock(1, 48)).unwrap();
        for (index, pts) in [90_000, 101_250, 93_750, 93_750].into_iter().enumerate() {
            if video
                .frame(pts, 82_500 + i64::try_from(index).unwrap() * 3750)
                .is_err()
            {
                return;
            }
        }
        assert!(video.finish().is_err());
    }
    #[test]
    fn video_uses_one_rational_phase_without_a_terminal_dts_guess() {
        let mut video = Video::new(video_clock(1001, 120_000)).unwrap();
        for index in 0..50_i64 {
            let ticks = 90_000 + (index * 3003 + 1) / 2;
            video.frame(ticks, ticks).unwrap();
        }
        video.finish().unwrap();
        assert_eq!(video.nominal_duration().unwrap(), 1501);
        let mut video = Video::new(video_clock(1, 48)).unwrap();
        video.frame(90_000, 90_000).unwrap();
        assert!(video.frame(93_751, 93_750).is_err());
        assert!(Video::new(video_clock(0, 48)).is_err());
        assert!(Video::new(video_clock(1, 0)).is_err());
    }
}
