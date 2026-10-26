//! Original video resource clocks constrain a private resegmentation request.
use crate::{
    concurrency::Stop,
    conversion::{ConversionError, ts::NativeEvidence},
};

const INVALID: ConversionError = ConversionError("hls-video-cadence-not-supported");

pub(in crate::conversion) fn milliseconds(
    ticks: u64,
    scale: u64,
) -> Result<Option<u32>, ConversionError> {
    if scale == 0 || ticks == 0 {
        return Err(INVALID);
    }
    // Rounding upward can place a request just after an existing access point.
    let value = u128::from(ticks) * 1000 / u128::from(scale);
    Ok((value > 0).then(|| u32::try_from(value).unwrap_or(u32::MAX)))
}

pub(in crate::conversion) fn transport(
    native: &NativeEvidence,
    stop: &Stop,
) -> Result<Option<u32>, ConversionError> {
    let mut maximum = None;
    for timing in native.segment_timings()? {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        let ticks = timing
            .end_pts
            .checked_sub(timing.first_pts)
            .and_then(|value| u64::try_from(value).ok())
            .ok_or(INVALID)?;
        if let Some(value) = milliseconds(ticks, 90_000)? {
            maximum = Some(maximum.map_or(value, |prior: u32| prior.max(value)));
        }
    }
    Ok(maximum)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_video_clocks_are_floored_without_rounding_past_an_access_point() {
        for (ticks, scale, expected) in [
            (180_000, 90_000, Some(2000)),
            (180_181, 90_000, Some(2002)),
            (540_000, 90_000, Some(6000)),
            (1, 90_000, None),
        ] {
            assert_eq!(milliseconds(ticks, scale).unwrap(), expected);
        }
        assert!(milliseconds(1, 0).is_err());
        assert!(milliseconds(0, 90_000).is_err());
    }
}
