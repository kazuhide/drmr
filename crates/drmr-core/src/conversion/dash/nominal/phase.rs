//! Prove a zero-phase nominal grid in one bounded pass over exact media edges.
use super::{ConversionError, Stop, error};

fn ceil(value: i128, divisor: i128) -> i128 {
    value.div_euclid(divisor) + i128::from(value.rem_euclid(divisor) != 0)
}

pub(super) fn count_bounds(
    period: (u64, u64),
    scale: u64,
    count: usize,
) -> Result<(u64, u64), ConversionError> {
    let bad = error("dash-nominal-duration-not-representable");
    if period.0 == 0 || period.1 == 0 || scale == 0 || count == 0 || count > 100_000 {
        return Err(bad);
    }
    let length = u128::from(period.0) * u128::from(scale);
    let unit = u128::from(period.1);
    let count = count as u128;
    let lower = length.div_ceil(unit * count).max(1);
    let upper = if count == 1 {
        u128::from(u32::MAX)
    } else {
        length.div_ceil(unit * (count - 1)).saturating_sub(1)
    }
    .min(u128::from(u32::MAX));
    if lower > upper {
        return Err(bad);
    }
    Ok((
        u64::try_from(lower).map_err(|_| bad)?,
        u64::try_from(upper).map_err(|_| bad)?,
    ))
}

pub(super) struct Facts<'a> {
    spans: &'a [(u64, u64)],
    offset: u64,
    end: u64,
}
impl<'a> Facts<'a> {
    pub(super) fn new(
        spans: &'a [(u64, u64)],
        offset: u64,
        end: u64,
    ) -> Result<Self, ConversionError> {
        if spans.is_empty()
            || spans.len() > 100_000
            || end == 0
            || spans.iter().any(|span| span.0 >= span.1)
        {
            return Err(error("dash-nominal-duration-not-representable"));
        }
        Ok(Self { spans, offset, end })
    }

    pub(super) fn find_zero(
        &self,
        preferred: u64,
        low: u64,
        high: u64,
        stop: &Stop,
    ) -> Result<Option<u64>, ConversionError> {
        let count = self.spans.len() as i128;
        let end = i128::from(self.end);
        // Both the Representation's finite range and the global Period must
        // enumerate exactly the files that exist, including in older players.
        let mut lower = i128::from(low.max(1)).max(ceil(end, count));
        let mut upper = i128::from(high.min(u64::from(u32::MAX)));
        if count > 1 {
            upper = upper.min((end - 1) / (count - 1));
        }
        for (ordinal, &(start, finish)) in self.spans.iter().enumerate() {
            super::super::checkpoint(Some(stop))?;
            let n = ordinal as i128;
            let start = i128::from(start) - i128::from(self.offset);
            let finish = i128::from(finish) - i128::from(self.offset);
            if ordinal == 0 {
                lower = lower.max(2 * start.abs());
            } else {
                lower = lower.max(ceil(2 * start, 2 * n + 1));
                upper = upper.min((2 * start).div_euclid(2 * n - 1));
            }
            if ordinal + 1 < self.spans.len() {
                lower = lower.max(ceil(2 * finish, 2 * n + 3));
                upper = upper.min((2 * finish).div_euclid(2 * n + 1));
            }
            if lower > upper {
                return Ok(None);
            }
        }
        if lower > upper {
            return Ok(None);
        }
        u64::try_from(i128::from(preferred).clamp(lower, upper))
            .map(Some)
            .map_err(|_| error("dash-nominal-duration-not-representable"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn valid(spans: &[(u64, u64)], offset: u64, end: u64, duration: u64) -> bool {
        let d = i128::from(duration);
        end.div_ceil(duration) == spans.len() as u64
            && spans.iter().enumerate().all(|(ordinal, &(start, finish))| {
                let nominal_start = ordinal as i128 * d;
                let nominal_end = nominal_start + d;
                nominal_start < i128::from(end)
                    && ((i128::from(start) - i128::from(offset)) - nominal_start).abs() * 2 <= d
                    && ((i128::from(finish) - i128::from(offset))
                        - nominal_end.min(i128::from(end)))
                    .abs()
                        * 2
                        <= d
            })
    }
    #[test]
    fn zero_phase_search_accounts_for_global_period_segment_count() {
        let spans = [(0, 6), (6, 12)];
        let facts = Facts::new(&spans, 0, 12).unwrap();
        // Duration six with a global Period just beyond twelve would cause
        // older players to request a nonexistent third file.
        let bounds = count_bounds((121, 10), 1, 2).unwrap();
        assert_eq!(bounds, (7, 12));
        let found = facts
            .find_zero(6, bounds.0, bounds.1, &Stop::new())
            .unwrap();
        assert_eq!(found, Some(7));
        assert!(valid(&spans, 0, 12, found.unwrap()));
    }
    #[test]
    fn a_leading_two_second_fragment_cannot_hide_a_six_second_phase_shift() {
        let mut spans = vec![(0, 2)];
        for i in 0..99 {
            spans.push((2 + i * 6, 8 + i * 6));
        }
        let end = spans.last().unwrap().1;
        let facts = Facts::new(&spans, 0, end).unwrap();
        assert_eq!(facts.find_zero(6, 6, 6, &Stop::new()).unwrap(), None);
    }
    #[test]
    fn zero_phase_domain_matches_an_independent_exhaustive_edge_oracle() {
        let stop = Stop::new();
        for offset in [0, 3] {
            for start in 0..4_u64 {
                for a in 1..7_u64 {
                    for b in 1..7_u64 {
                        for c in 1..7_u64 {
                            let spans = [
                                (offset + start, offset + start + a),
                                (offset + start + a, offset + start + a + b),
                                (offset + start + a + b, offset + start + a + b + c),
                            ];
                            let end = start + a + b + c;
                            let facts = Facts::new(&spans, offset, end).unwrap();
                            let expected = (1..=24)
                                .filter(|d| valid(&spans, offset, end, *d))
                                .min_by_key(|d| (d.abs_diff(6), *d));
                            let found = facts.find_zero(6, 1, 24, &stop).unwrap();
                            assert_eq!(found, expected, "{spans:?}/{offset}");
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn count_bounds_match_exact_rational_enumeration() {
        for numerator in 1..36_u64 {
            for denominator in 1..5_u64 {
                for scale in [1, 3, 7] {
                    for count in 1..12_usize {
                        let result = count_bounds((numerator, denominator), scale, count);
                        for duration in 1..=24_u64 {
                            let expected = (u128::from(numerator) * u128::from(scale))
                                .div_ceil(u128::from(denominator) * u128::from(duration))
                                == count as u128;
                            assert_eq!(
                                result
                                    .as_ref()
                                    .is_ok_and(|&(lo, hi)| (lo..=hi).contains(&duration)),
                                expected,
                            );
                        }
                    }
                }
            }
        }
    }
}
