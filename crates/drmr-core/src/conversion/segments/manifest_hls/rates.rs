//! Exact decimal rates share one bounded RFC 8216 moving-window algorithm.
use crate::{concurrency::Stop, conversion::ConversionError};
use std::collections::VecDeque;

pub(in crate::conversion) const SCALE: u128 = 1_000_000_000_000_000_000;
const BAD: ConversionError = ConversionError("hls-output-metadata-invalid");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct Rates {
    pub peak: u64,
    pub average: u64,
}

fn check(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}

pub(in crate::conversion) fn duration(text: &str) -> Result<u128, ConversionError> {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if text.len() > 64
        || whole.is_empty()
        || fraction.len() > 18
        || !whole
            .bytes()
            .chain(fraction.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        return Err(BAD);
    }
    let whole = whole.parse::<u64>().map_err(|_| BAD)?;
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<u128>()
            .map_err(|_| BAD)?
            .checked_mul(
                10_u128
                    .checked_pow(u32::try_from(18 - fraction.len()).map_err(|_| BAD)?)
                    .ok_or(BAD)?,
            )
            .ok_or(BAD)?
    };
    u128::from(whole)
        .checked_mul(SCALE)
        .and_then(|n| n.checked_add(fraction))
        .filter(|n| *n > 0)
        .ok_or(BAD)
}

pub(in crate::conversion) fn rate(bits: u128, time: u128) -> Result<u64, ConversionError> {
    let value = bits.checked_mul(SCALE).ok_or(BAD)?.div_ceil(time);
    u64::try_from(value).map_err(|_| BAD)
}

/// Integer feasibility over a moving duration window avoids quadratic segment-pair scans.
fn above(
    prefix: &[(u128, u128)],
    low: u128,
    high: u128,
    candidate: u64,
    stop: &Stop,
) -> Result<bool, ConversionError> {
    let score = |&(time, bits): &(u128, u128)| -> Result<i128, ConversionError> {
        let first = i128::try_from(bits.checked_mul(SCALE).ok_or(BAD)?).map_err(|_| BAD)?;
        let second =
            i128::try_from(time.checked_mul(u128::from(candidate)).ok_or(BAD)?).map_err(|_| BAD)?;
        first.checked_sub(second).ok_or(BAD)
    };
    let mut queue = VecDeque::<(usize, i128)>::new();
    let mut next = 0;
    for end in 1..prefix.len() {
        check(stop)?;
        let Some(latest) = prefix[end].0.checked_sub(low) else {
            continue;
        };
        while next < end && prefix[next].0 <= latest {
            let value = score(&prefix[next])?;
            while queue.back().is_some_and(|(_, previous)| *previous >= value) {
                queue.pop_back();
            }
            queue.push_back((next, value));
            next += 1;
        }
        while queue
            .front()
            .is_some_and(|(index, _)| prefix[end].0 - prefix[*index].0 > high)
        {
            queue.pop_front();
        }
        if let Some((_, minimum)) = queue.front()
            && score(&prefix[end])? > *minimum
        {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(in crate::conversion) fn measure(
    prefix: &[(u128, u128)],
    maximum: u64,
    target: u64,
    stop: &Stop,
) -> Result<Rates, ConversionError> {
    check(stop)?;
    if prefix.len() < 2 || target == 0 {
        return Err(BAD);
    }
    let target_time = u128::from(target).checked_mul(SCALE).ok_or(BAD)?;
    let low = target_time / 2;
    let high = target_time.checked_mul(3).ok_or(BAD)? / 2;
    let &(time, bits) = prefix.last().ok_or(BAD)?;
    let average = rate(bits, time)?;
    let peak = if time < low {
        // No RFC peak window exists for a shorter-than-half-target presentation.
        maximum
    } else {
        let mut first = 0;
        let mut last = maximum;
        while first < last {
            let middle = first + (last - first) / 2;
            if above(prefix, low, high, middle, stop)? {
                first = middle + 1;
            } else {
                last = middle;
            }
        }
        first
    };
    if peak == 0 || average == 0 {
        return Err(BAD);
    }
    Ok(Rates { peak, average })
}
