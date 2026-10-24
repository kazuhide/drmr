//! One coherent AAC clock phase must explain every sample, including rounded clocks.
use super::super::{ConversionError, native_media::SampleInfo};
const BAD: ConversionError = ConversionError("native-aac-clock-mismatch");

pub(super) struct AudioClock {
    step: i128,
    rate: i128,
    first: Option<(i64, i64)>,
    low: i128,
    high: i128,
}
impl AudioClock {
    pub fn new(samples: u16, rate: u32, scale: u32) -> Result<Self, ConversionError> {
        if samples == 0 || rate == 0 || scale == 0 {
            return Err(BAD);
        }
        let step = i128::from(samples) * i128::from(scale);
        if step < i128::from(rate) {
            return Err(BAD);
        }
        Ok(Self {
            step,
            rate: i128::from(rate),
            first: None,
            low: 0,
            high: i128::from(rate),
        })
    }
    pub fn record(&mut self, info: &SampleInfo, last: bool) -> Result<(), ConversionError> {
        let first = *self.first.get_or_insert((info.dts, info.pts));
        let dts = i128::from(info.dts) - i128::from(first.0);
        let pts = i128::from(info.pts) - i128::from(first.1);
        if dts != pts {
            return Err(BAD);
        }
        let exact = i128::from(info.ordinal).checked_mul(self.step).ok_or(BAD)?;
        let lower = dts
            .checked_mul(self.rate)
            .and_then(|value| value.checked_sub(exact))
            .ok_or(BAD)?;
        self.low = self.low.max(lower);
        self.high = self.high.min(lower.checked_add(self.rate).ok_or(BAD)?);
        if self.low >= self.high {
            return Err(BAD);
        }
        let full_ceiling = (self.step + self.rate - 1) / self.rate;
        if last {
            // A progressive file may declare only the presented part of its
            // terminal AAC frame. Its complete encoded bytes are still read.
            if info.duration == 0 || i128::from(info.duration) > full_ceiling {
                return Err(BAD);
            }
        } else {
            let end = dts.checked_add(i128::from(info.duration)).ok_or(BAD)?;
            let next = exact.checked_add(self.step).ok_or(BAD)?;
            let lower = end
                .checked_mul(self.rate)
                .and_then(|value| value.checked_sub(next))
                .ok_or(BAD)?;
            self.low = self.low.max(lower);
            self.high = self.high.min(lower.checked_add(self.rate).ok_or(BAD)?);
            if self.low >= self.high {
                return Err(BAD);
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample(index: u64, dts: i64, duration: u32) -> SampleInfo {
        SampleInfo {
            ordinal: index,
            dts,
            pts: dts,
            duration,
            size: 1,
            range: 0..1,
            flags: 0,
            is_sync: true,
            roll_distance: None,
            preroll_distance: None,
            protection: super::super::super::native_media::SampleProtection::Clear,
            chunk_offset: 0,
            is_last_chunk: true,
        }
    }
    #[test]
    fn every_rounded_aac_clock_must_share_one_phase() {
        let mut clock = AudioClock::new(1024, 44_100, 90_000).unwrap();
        for index in 0..100_u64 {
            let start = (index * 1024 * 90_000 + 22_050) / 44_100;
            let end = ((index + 1) * 1024 * 90_000 + 22_050) / 44_100;
            clock
                .record(
                    &sample(
                        index,
                        i64::try_from(start).unwrap(),
                        u32::try_from(end - start).unwrap(),
                    ),
                    index == 99,
                )
                .unwrap();
        }
        let mut changed = AudioClock::new(1024, 44_100, 90_000).unwrap();
        assert!(changed.record(&sample(0, 0, 2089), false).is_ok());
        assert!(changed.record(&sample(1, 2089, 2090), false).is_ok());
        assert_eq!(
            changed.record(&sample(2, 4179, 2089), false).unwrap_err(),
            BAD
        );
    }
    #[test]
    fn terminal_selection_is_distinct_from_a_short_middle_frame() {
        let mut clock = AudioClock::new(1024, 48_000, 48_000).unwrap();
        clock.record(&sample(0, 0, 1024), false).unwrap();
        clock.record(&sample(1, 1024, 768), true).unwrap();
        let mut changed = AudioClock::new(1024, 48_000, 48_000).unwrap();
        assert_eq!(changed.record(&sample(0, 0, 768), false).unwrap_err(), BAD);
        let mut changed = AudioClock::new(1024, 48_000, 48_000).unwrap();
        assert_eq!(changed.record(&sample(0, 0, 1025), true).unwrap_err(), BAD);
    }
}
