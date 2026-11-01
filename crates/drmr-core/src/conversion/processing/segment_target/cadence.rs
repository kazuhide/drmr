//! Original video resource clocks constrain a private resegmentation request.
use crate::{
    concurrency::Stop,
    conversion::{ConversionError, ts::NativeEvidence},
};

const INVALID: ConversionError = ConversionError("hls-video-cadence-not-supported");

/// Boundary fragments can include leading or trailing presentation offsets.
/// Keep only their values and the maximum interior cadence, in constant space.
#[derive(Default)]
pub(in crate::conversion) struct FragmentCadence {
    count: u8,
    first: Option<u32>,
    last: Option<u32>,
    interior: Option<u32>,
}

impl FragmentCadence {
    pub fn push(&mut self, value: Option<u32>) {
        if self.count == 0 {
            self.first = value;
        } else if self.count >= 2
            && let Some(previous) = self.last
        {
            self.interior = Some(self.interior.map_or(previous, |old| old.max(previous)));
        }
        self.last = value;
        self.count = self.count.saturating_add(1).min(3);
    }

    pub fn request_ms(&self) -> Option<u32> {
        if self.count >= 3 {
            self.interior
        } else {
            // A short asset has no established interior grid. The smaller
            // observed interval avoids asking just after its access point.
            self.first.into_iter().chain(self.last).min()
        }
    }
}

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

    #[test]
    fn boundary_fragments_do_not_replace_regular_or_nonuniform_interior_cadence() {
        for (values, expected) in [
            (vec![1916, 2000, 2000, 2000, 2083], 2000),
            (vec![7000, 2000, 4000, 3000, 9000], 4000),
            (vec![1916, 2083], 1916),
            (vec![2083, 1916], 1916),
            (vec![2083], 2083),
        ] {
            let mut cadence = FragmentCadence::default();
            for value in values {
                cadence.push(Some(value));
            }
            assert_eq!(cadence.request_ms(), Some(expected));
        }
        assert_eq!(FragmentCadence::default().request_ms(), None);
        let mut cadence = FragmentCadence::default();
        for value in [Some(5000), None, Some(6000)] {
            cadence.push(value);
        }
        assert_eq!(cadence.request_ms(), None);
    }
}
