//! Prove nominal MPD timing against every measured edge without changing media clocks.
use super::{
    ConversionError, Path,
    addressing::{self, Nominal, Representation, Segment},
    error, reference,
};
use crate::{concurrency::Stop, conversion::timing::FragmentClock, inspection::TrackKind};

pub(super) struct Output {
    pub timescale: u64,
    pub offset: u64,
    pub duration: u64,
    pub presentation_duration: u64,
    pub end_number: u64,
}
struct Measured {
    spans: Vec<(u64, u64)>,
    scale: u64,
    offset: u64,
    video: bool,
}

fn scale(value: u64, from: u64, to: u64) -> Result<u64, ConversionError> {
    u128::from(value)
        .checked_mul(u128::from(to))
        .filter(|n| from > 0 && n % u128::from(from) == 0)
        .and_then(|n| u64::try_from(n / u128::from(from)).ok())
        .ok_or(error("dash-nominal-timescale-not-representable"))
}
fn common_scale(left: u64, right: u64) -> Result<u64, ConversionError> {
    let (mut a, mut b) = (left, right);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    if a == 0 {
        return Err(error("dash-nominal-timescale-not-representable"));
    }
    left.checked_div(a)
        .and_then(|n| n.checked_mul(right))
        .filter(|n| u32::try_from(*n).is_ok())
        .ok_or(error("dash-nominal-timescale-not-representable"))
}

fn planned_scale(common: u64, planned_ms: Option<u32>) -> Result<u64, ConversionError> {
    let Some(millis) = planned_ms else {
        return Ok(common);
    };
    if millis == 0 {
        return Err(error("dash-nominal-duration-required"));
    }
    let (mut a, mut b) = (u64::from(millis), 1000);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    // Only the request's reduced denominator is needed. Whole seconds retain
    // the original clock; fractional intent gains exact integer units without
    // rounding any sample edge or changing encoded media timestamps.
    common_scale(common, 1000 / a)
}

fn measure(
    root: &Path,
    resolved: &Representation,
    stop: &Stop,
    planned_ms: Option<u32>,
) -> Result<Measured, ConversionError> {
    let initialization = reference(root, &resolved.base, &resolved.initialization.uri)?;
    let report = crate::inspection::inspect(&initialization)
        .map_err(|_| error("dash-invalid-staged-media"))?;
    let video = report.tracks.len() == 1 && report.tracks[0].kind == TrackKind::Video;
    let mut clock = FragmentClock::read(&initialization)?;
    let mut spans = Vec::with_capacity(resolved.segments.len());
    let mut actual_scale = None;
    for segment in &resolved.segments {
        super::checkpoint(Some(stop))?;
        let path = reference(root, &resolved.base, &segment.resource.uri)?;
        let (declared, presented) = clock.dash_clocks(&path)?;
        if u128::from(declared.0) * u128::from(resolved.timescale)
            != u128::from(segment.time) * u128::from(declared.2)
            || u128::from(declared.1) * u128::from(resolved.timescale)
                != u128::from(segment.duration) * u128::from(declared.2)
        {
            return Err(error("manifest-fragment-timeline-mismatch"));
        }
        if actual_scale.is_some_and(|old| old != presented.2) {
            return Err(error("dash-nominal-timescale-not-representable"));
        }
        actual_scale = Some(presented.2);
        spans.push((presented.0, presented.1));
    }
    let window = clock.presentation_window()?;
    if spans.first().is_none_or(|span| span.0 != window.0)
        || spans.last().is_none_or(|span| span.1 != window.1)
    {
        return Err(error("dash-nominal-timeline-not-representable"));
    }
    let actual_scale = actual_scale.ok_or(error("dash-empty-segment-timeline"))?;
    let common = planned_scale(common_scale(resolved.timescale, actual_scale)?, planned_ms)?;
    for span in &mut spans {
        span.0 = scale(span.0, actual_scale, common)?;
        span.1 = scale(span.1, actual_scale, common)?;
    }
    Ok(Measured {
        spans,
        scale: common,
        offset: scale(resolved.offset, resolved.timescale, common)?,
        video,
    })
}

mod phase;
fn observed_duration(measured: &Measured) -> Result<u64, ConversionError> {
    let last = measured.spans.len().saturating_sub(1);
    let interior = &measured.spans[usize::from(last > 1)..last];
    // Boundary fragments may be shorter. Prefer the interior cadence in its exact clock units.
    let spans = if interior.is_empty() {
        &measured.spans[..1]
    } else {
        interior
    };
    let mut durations = spans
        .iter()
        .map(|&(start, end)| {
            end.checked_sub(start)
                .filter(|duration| *duration > 0)
                .ok_or(error("dash-nominal-duration-not-representable"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let median = durations.len() / 2;
    Ok(*durations.select_nth_unstable(median).1)
}

fn select(
    resolved: &Representation,
    measured: &Measured,
    planned_ms: Option<u32>,
    end: u64,
    period: (u64, u64),
    stop: &Stop,
) -> Result<u64, ConversionError> {
    let facts = phase::Facts::new(&measured.spans, measured.offset, end)?;
    let (low, high) = phase::count_bounds(period, measured.scale, measured.spans.len())?;
    let last = resolved.segments.len().saturating_sub(1);
    let nonterminal = &resolved.segments[usize::from(last > 1)..last];
    if measured.video
        && let Some(first) = nonterminal.first()
        && nonterminal
            .iter()
            .all(|segment| segment.duration == first.duration)
    {
        let constant = scale(first.duration, resolved.timescale, measured.scale)?;
        if (low..=high).contains(&constant)
            && facts
                .find_zero(constant, constant, constant, stop)?
                .is_some()
        {
            return Ok(constant);
        }
    }
    let planned = match planned_ms {
        Some(0) => return Err(error("dash-nominal-duration-required")),
        Some(millis) => scale(u64::from(millis), 1000, measured.scale)?,
        None => match resolved.nominal {
            Some(nominal) => scale(nominal.duration, resolved.timescale, measured.scale)?,
            None => observed_duration(measured)?,
        },
    };
    facts
        .find_zero(planned, low, high, stop)?
        .ok_or(error("dash-nominal-duration-not-representable"))
}

pub(super) fn output(
    root: &Path,
    resolved: &Representation,
    period: (u64, u64),
    planned_ms: Option<u32>,
    stop: &Stop,
) -> Result<Output, ConversionError> {
    let measured = measure(root, resolved, stop, planned_ms)?;
    let end = measured
        .spans
        .last()
        .ok_or(error("dash-empty-segment-timeline"))?
        .1;
    let presentation_duration = end
        .checked_sub(measured.offset)
        .filter(|n| *n > 0)
        .ok_or(error("dash-nominal-duration-not-representable"))?;
    if u128::from(presentation_duration) * u128::from(period.1)
        > u128::from(period.0) * u128::from(measured.scale)
    {
        return Err(error("dash-representation-duration-outside-period"));
    }
    let duration = select(
        resolved,
        &measured,
        planned_ms,
        presentation_duration,
        period,
        stop,
    )?;
    let nominal = Nominal {
        duration,
        period: (presentation_duration, measured.scale),
        delta: 0,
        explicit_presentation: true,
    };
    for (ordinal, (source, span)) in resolved.segments.iter().zip(&measured.spans).enumerate() {
        super::checkpoint(Some(stop))?;
        let time = measured
            .offset
            .checked_add(
                (ordinal as u64)
                    .checked_mul(duration)
                    .ok_or(error("dash-template-number-overflow"))?,
            )
            .ok_or(error("dash-template-number-overflow"))?;
        let segment = Segment {
            resource: source.resource.clone(),
            time,
            duration,
            number: source.number,
        };
        addressing::check_nominal(
            &segment,
            nominal,
            measured.offset,
            measured.scale,
            (span.0, span.1, measured.scale),
            ordinal,
            measured.spans.len(),
        )?;
    }
    Ok(Output {
        timescale: measured.scale,
        offset: measured.offset,
        duration,
        presentation_duration,
        end_number: resolved
            .segments
            .last()
            .ok_or(error("dash-empty-segment-timeline"))?
            .number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fractional_millisecond_intent_and_all_media_edges_have_an_exact_common_clock() {
        let common = planned_scale(12288, Some(2083)).unwrap();
        assert_eq!(common, 1_536_000);
        assert_eq!(scale(2083, 1000, common).unwrap(), 3_199_488);
        assert_eq!(scale(23552, 12288, common).unwrap(), 2_944_000);
        assert_eq!(scale(24576, 12288, common).unwrap(), 3_072_000);
        assert_eq!(scale(25600, 12288, common).unwrap(), 3_200_000);
    }

    #[test]
    fn integer_intent_and_legacy_metadata_keep_their_original_clock() {
        for planned in [None, Some(2000), Some(6000), Some(1500)] {
            assert_eq!(planned_scale(12288, planned).unwrap(), 12288);
        }
        assert!(planned_scale(12288, Some(0)).is_err());
        assert!(planned_scale(u64::from(u32::MAX), Some(1)).is_err());
    }
}
