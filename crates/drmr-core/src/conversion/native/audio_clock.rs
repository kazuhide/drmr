//! One coherent AAC clock phase must explain every sample, including rounded clocks.
use super::super::{ConversionError, native_media::SampleInfo};
const BAD: ConversionError = ConversionError("native-aac-clock-mismatch");

pub(super) struct AudioClock {
    step: i128,
    rate: i128,
    first: Option<(i64, i64)>,
    low: i128,
    high: i128,
    transport: bool,
    observed: Option<(u64, i64)>,
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
            transport: samples == 1024 && rate == 44_100 && scale == 90_000,
            observed: None,
        })
    }
    pub fn record(&mut self, info: &SampleInfo, last: bool) -> Result<(), ConversionError> {
        let first = *self.first.get_or_insert((info.dts, info.pts));
        let dts = i128::from(info.dts) - i128::from(first.0);
        let pts = i128::from(info.pts) - i128::from(first.1);
        if dts != pts {
            return Err(BAD);
        }
        self.observe(info.ordinal, i128::from(info.dts))?;
        let full_ceiling = (self.step + self.rate - 1) / self.rate;
        if last {
            // A progressive file may declare only the presented part of its
            // terminal AAC frame. Its complete encoded bytes are still read.
            if info.duration == 0 || i128::from(info.duration) > full_ceiling {
                return Err(BAD);
            }
        } else {
            self.observe(
                info.ordinal.checked_add(1).ok_or(BAD)?,
                i128::from(info.dts)
                    .checked_add(i128::from(info.duration))
                    .ok_or(BAD)?,
            )?;
        }
        Ok(())
    }
    fn observe(&mut self, ordinal: u64, ticks: i128) -> Result<(), ConversionError> {
        if self.transport {
            let ticks = i64::try_from(ticks).map_err(|_| BAD)?;
            if let Some((previous, old)) = self.observed {
                if previous == ordinal {
                    return if old == ticks { Ok(()) } else { Err(BAD) };
                }
                if previous.checked_add(1) != Some(ordinal) {
                    return Err(BAD);
                }
                // Inside a PES, require the exact stable interpolation used by
                // the original transport clock. A different timestamp must be
                // a possible PES start on the same track-wide quantizer phase.
                // No timestamp-equality tolerance is introduced here.
                if super::super::ts::next_audio_clock(old, 44_100).map_err(|_| BAD)? == ticks {
                    self.observed = Some((ordinal, ticks));
                    return Ok(());
                }
            }
            self.observed = Some((ordinal, ticks));
        }
        let relative = ticks
            .checked_sub(i128::from(self.first.ok_or(BAD)?.0))
            .ok_or(BAD)?;
        let exact = i128::from(ordinal).checked_mul(self.step).ok_or(BAD)?;
        let lower = relative
            .checked_mul(self.rate)
            .and_then(|value| value.checked_sub(exact))
            .ok_or(BAD)?;
        self.low = self.low.max(lower);
        self.high = self.high.min(lower.checked_add(self.rate).ok_or(BAD)?);
        if self.low >= self.high {
            return Err(BAD);
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
    fn original_transport_clocks() -> [i64; 88] {
        // Exact packet clocks from the two-second, 44.1 kHz BBB transport fixture.
        // Its PES starts are quantized independently of stable frame interpolation.
        [
            126_000, 128_090, 130_179, 132_269, 134_359, 136_449, 138_539, 140_628, 142_718,
            144_808, 146_898, 148_988, 151_077, 153_167, 155_257, 157_347, 159_436, 161_527,
            163_617, 165_707, 167_797, 169_886, 171_976, 174_066, 176_155, 178_245, 180_334,
            182_424, 184_514, 186_604, 188_694, 190_784, 192_874, 194_964, 197_054, 199_144,
            201_233, 203_323, 205_412, 207_502, 209_592, 211_681, 213_771, 215_861, 217_951,
            220_041, 222_130, 224_220, 226_310, 228_400, 230_490, 232_579, 234_670, 236_760,
            238_850, 240_940, 243_029, 245_119, 247_209, 249_298, 251_388, 253_477, 255_567,
            257_657, 259_747, 261_836, 263_927, 266_017, 268_107, 270_197, 272_286, 274_376,
            276_466, 278_555, 280_645, 282_734, 284_824, 286_914, 289_004, 291_094, 293_184,
            295_274, 297_364, 299_454, 301_544, 303_633, 305_723, 307_812,
        ]
    }
    fn record_transport_clocks(ticks: &[i64]) -> Result<(), ConversionError> {
        let mut clock = AudioClock::new(1024, 44_100, 90_000)?;
        for (index, &dts) in ticks.iter().enumerate() {
            let duration = ticks.get(index + 1).map_or(2089, |next| next - dts);
            clock.record(
                &sample(
                    u64::try_from(index).unwrap(),
                    dts,
                    u32::try_from(duration).unwrap(),
                ),
                index + 1 == ticks.len(),
            )?;
        }
        Ok(())
    }
    #[test]
    fn transport_pes_quantization_and_interpolation_preserve_every_original_clock() {
        let ticks = original_transport_clocks();
        assert_eq!(ticks[17] - ticks[16], 2091);
        record_transport_clocks(&ticks).unwrap();
    }
    #[test]
    fn transport_clock_family_rejects_changed_middle_clocks_and_accumulated_drift() {
        for ordinal in [1, 40, 44, 60, 85] {
            for delta in [-1, 1] {
                let mut ticks = original_transport_clocks();
                ticks[ordinal] += delta;
                assert_eq!(record_transport_clocks(&ticks).unwrap_err(), BAD);
            }
        }
        for delta in [-2090, 2090] {
            let mut ticks = original_transport_clocks();
            for dts in &mut ticks[44..] {
                *dts += delta;
            }
            assert_eq!(record_transport_clocks(&ticks).unwrap_err(), BAD);
        }
        let mut ticks = original_transport_clocks();
        for (ordinal, dts) in ticks.iter_mut().enumerate() {
            *dts += i64::try_from(ordinal).unwrap();
        }
        assert_eq!(record_transport_clocks(&ticks).unwrap_err(), BAD);
    }
    #[test]
    fn every_rounded_aac_clock_must_share_one_phase() {
        for scale in [45_000, 90_000] {
            let mut clock = AudioClock::new(1024, 44_100, scale).unwrap();
            for index in 0..100_u64 {
                let start = (index * 1024 * u64::from(scale) + 22_050) / 44_100;
                let end = ((index + 1) * 1024 * u64::from(scale) + 22_050) / 44_100;
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
        }
        // Other container clocks retain the single-quantizer admission model.
        let mut changed = AudioClock::new(1024, 44_100, 45_000).unwrap();
        assert!(changed.record(&sample(0, 0, 1044), false).is_ok());
        assert!(changed.record(&sample(1, 1044, 1045), false).is_ok());
        assert_eq!(
            changed.record(&sample(2, 2089, 1044), false).unwrap_err(),
            BAD
        );
    }
    #[test]
    fn terminal_selection_is_distinct_from_a_short_middle_frame() {
        for rate in [44_100, 48_000] {
            for presented in [1, 768, 1023] {
                let mut clock = AudioClock::new(1024, rate, rate).unwrap();
                clock.record(&sample(0, 0, 1024), false).unwrap();
                clock.record(&sample(1, 1024, presented), true).unwrap();
            }
        }
        let mut changed = AudioClock::new(1024, 48_000, 48_000).unwrap();
        assert_eq!(changed.record(&sample(0, 0, 768), false).unwrap_err(), BAD);
        let mut changed = AudioClock::new(1024, 48_000, 48_000).unwrap();
        assert_eq!(changed.record(&sample(0, 0, 1025), true).unwrap_err(), BAD);
    }
}
