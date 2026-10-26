use super::{check as record, not_checked, numeric::Duration};
use crate::{DiagnosticSink, NotCheckedReason, Span};

pub(super) type PeriodDuration = Result<Duration, NotCheckedReason>;

#[derive(Clone, Copy)]
pub(super) struct Run {
    pub start: Option<u128>,
    pub length: u64,
    pub span: Span,
}

pub(super) fn check(
    run: Run,
    duration: PeriodDuration,
    timescale: Option<u64>,
    offset: Option<u64>,
    sink: &mut DiagnosticSink<'_>,
) {
    let result = duration.and_then(|duration| {
        calculate(
            run.start.ok_or(NotCheckedReason::InvalidPrerequisite)?,
            run.length,
            duration,
            timescale.ok_or(NotCheckedReason::InvalidPrerequisite)?,
            offset.ok_or(NotCheckedReason::InvalidPrerequisite)?,
        )
    });
    match result {
        Ok(_) => record(sink, "DASH-TIMELINE-ARITHMETIC", true, run.span),
        Err(reason) => not_checked(sink, "DASH-TIMELINE-ARITHMETIC", reason, run.span),
    }
}

/// Return the positive segment count and final sample-timeline end without
/// expanding the run. A rational Period end can be crossed by its final segment.
pub(super) fn calculate(
    start: u128,
    length: u64,
    duration: Duration,
    timescale: u64,
    offset: u64,
) -> Result<(u128, u128), NotCheckedReason> {
    if length == 0 || timescale == 0 || duration.denominator == 0 {
        return Err(NotCheckedReason::InvalidPrerequisite);
    }
    let divisor = gcd(duration.numerator, duration.denominator);
    let numerator = duration.numerator / divisor;
    let denominator = duration.denominator / divisor;
    let divisor = gcd(u128::from(timescale), denominator);
    let scale = u128::from(timescale) / divisor;
    let denominator = denominator / divisor;
    let overflow = NotCheckedReason::ResourceLimit;
    let endpoint = numerator
        .checked_mul(scale)
        .and_then(|value| {
            u128::from(offset)
                .checked_mul(denominator)
                .and_then(|offset| value.checked_add(offset))
        })
        .ok_or(overflow)?;
    let start_scaled = start.checked_mul(denominator).ok_or(overflow)?;
    if endpoint <= start_scaled {
        return Err(NotCheckedReason::InvalidPrerequisite);
    }
    let distance = endpoint - start_scaled;
    let divisor = u128::from(length)
        .checked_mul(denominator)
        .ok_or(overflow)?;
    let count = (distance / divisor)
        .checked_add(u128::from(distance % divisor != 0))
        .ok_or(overflow)?;
    let end = count
        .checked_mul(u128::from(length))
        .and_then(|length| start.checked_add(length))
        .ok_or(overflow)?;
    Ok((count, end))
}

pub(super) fn gcd(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_terminal_counts_use_duration_scale_offset_and_implicit_cursor() {
        let eight = Duration {
            numerator: 8,
            denominator: 1,
        };
        let fraction = Duration {
            numerator: 85,
            denominator: 10,
        };
        assert_eq!(calculate(0, 2, eight, 1, 0), Ok((4, 8)));
        assert_eq!(calculate(0, 2, fraction, 1, 0), Ok((5, 10)));
        assert_eq!(calculate(4, 2000, eight, 1000, 4), Ok((4, 8004)));
        assert_eq!(calculate(4, 2000, fraction, 1000, 4), Ok((5, 10004)));
        assert_eq!(calculate(4004, 2000, fraction, 1000, 4), Ok((3, 10004)));
        assert_eq!(calculate(8004, 2000, fraction, 1000, 4), Ok((1, 10004)));
        assert_eq!(
            calculate(8004, 2000, eight, 1000, 4),
            Err(NotCheckedReason::InvalidPrerequisite)
        );
        assert_eq!(
            calculate(8005, 2000, eight, 1000, 4),
            Err(NotCheckedReason::InvalidPrerequisite)
        );
    }

    #[test]
    fn rational_cancellation_avoids_false_overflow_and_true_overflow_is_explicit() {
        let numerator = u128::MAX - 5;
        let expected = numerator / 10 * 4;
        assert_eq!(
            calculate(
                0,
                1,
                Duration {
                    numerator,
                    denominator: 10
                },
                4,
                0
            ),
            Ok((expected, expected))
        );
        assert_eq!(
            calculate(
                0,
                1,
                Duration {
                    numerator: u128::MAX,
                    denominator: 1
                },
                2,
                0
            ),
            Err(NotCheckedReason::ResourceLimit)
        );
        assert_eq!(
            calculate(
                u128::MAX - 1,
                2,
                Duration {
                    numerator: u128::MAX,
                    denominator: 1
                },
                1,
                0
            ),
            Err(NotCheckedReason::ResourceLimit)
        );
    }

    #[test]
    fn bounded_exact_counts_match_independently_enumerated_small_runs() {
        for numerator in 0..31 {
            for denominator in [1, 10] {
                for timescale in 1..4 {
                    for offset in 0..4 {
                        for start in 0..12 {
                            for length in 1..6 {
                                let duration = Duration {
                                    numerator,
                                    denominator,
                                };
                                let endpoint = numerator * u128::from(timescale)
                                    + u128::from(offset) * denominator;
                                let mut end = start;
                                let mut count = 0;
                                while end * denominator < endpoint {
                                    end += u128::from(length);
                                    count += 1;
                                }
                                let expected = if count == 0 {
                                    Err(NotCheckedReason::InvalidPrerequisite)
                                } else {
                                    Ok((count, end))
                                };
                                assert_eq!(
                                    calculate(start, length, duration, timescale, offset),
                                    expected
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
