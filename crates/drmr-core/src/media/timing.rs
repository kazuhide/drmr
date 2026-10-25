//! Exact packet-timing comparisons and fragment-duration measurements.
//!
//! Rational arithmetic preserves timestamp precision. Integer division is rounded
//! correctly when converting final measurements to floating point.

use super::python::ratio;
use super::{MediaError, PacketInfo};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::Signed as _;
/// `same_timing`'s default `tolerance`: `Fraction(1, 45000)` (two 90 kHz transport ticks).
pub fn default_timing_tolerance() -> BigRational {
    BigRational::new(BigInt::from(1), BigInt::from(45_000))
}

impl PacketInfo {
    /// `PacketInfo.start`: `Fraction(min(pts), timescale)` — a `ValueError` without packets, a
    /// `ZeroDivisionError` for a zero timescale.
    pub fn start(&self) -> Result<BigRational, MediaError> {
        let first = self
            .pts
            .iter()
            .min()
            .ok_or_else(|| MediaError::Value("min() arg is an empty sequence".to_string()))?;
        ratio(*first, self.timescale)
    }

    /// `PacketInfo.end`: `max(Fraction(p + d, timescale) for p, d in zip(pts, durations))`.
    pub fn end(&self) -> Result<BigRational, MediaError> {
        let mut end: Option<BigRational> = None;
        for (pts, duration) in self.pts.iter().zip(&self.durations) {
            let candidate = ratio(i128::from(*pts) + i128::from(*duration), self.timescale)?;
            if end.as_ref().is_none_or(|current| candidate > *current) {
                end = Some(candidate);
            }
        }
        end.ok_or_else(|| MediaError::Value("max() arg is an empty sequence".to_string()))
    }
}

/// Every `(a, b)` pair within `tolerance` of `b / scale_b - a / scale_a - shift`, lazily as
/// Python's `all` over a `zip` (the shorter list bounds it; the first pair decides first).
fn pairs_within(
    source: &[i64],
    source_scale: i64,
    output: &[i64],
    output_scale: i64,
    shift: &BigRational,
    tolerance: &BigRational,
) -> Result<bool, MediaError> {
    for (a, b) in source.iter().zip(output) {
        let difference = ratio(*b, output_scale)? - ratio(*a, source_scale)? - shift;
        if difference.abs() > *tolerance {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `media::same_timing`: the same packet count, every pts and dts moved by exactly `shift`
/// and every duration unchanged, each within `tolerance` (default
/// [`default_timing_tolerance`]); compared exactly, in field insertion order (pts, then dts, then
/// durations, each stopping at the first pair outside). A zero timescale is Python's
/// `ZeroDivisionError`.
pub fn same_timing(
    source: &PacketInfo,
    output: &PacketInfo,
    shift: &BigRational,
    tolerance: Option<&BigRational>,
) -> Result<bool, MediaError> {
    if source.pts.len() != output.pts.len() {
        return Ok(false);
    }
    let default = default_timing_tolerance();
    let tolerance = tolerance.unwrap_or(&default);
    let zero = BigRational::from_integer(BigInt::from(0));
    Ok(pairs_within(
        &source.pts,
        source.timescale,
        &output.pts,
        output.timescale,
        shift,
        tolerance,
    )? && pairs_within(
        &source.dts,
        source.timescale,
        &output.dts,
        output.timescale,
        shift,
        tolerance,
    )? && pairs_within(
        &source.durations,
        source.timescale,
        &output.durations,
        output.timescale,
        &zero,
        tolerance,
    )?)
}
