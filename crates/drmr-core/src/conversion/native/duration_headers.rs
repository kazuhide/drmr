//! Check declared container durations against independently accumulated sample clocks.
use super::{
    super::{ConversionError, native_media::Edit},
    media::Media,
    samples::Ratio,
};
use std::cmp::Ordering;

const MISMATCH: ConversionError = ConversionError("native-media-duration-mismatch");
const UNSUPPORTED: ConversionError = ConversionError("native-presentation-window-not-supported");

fn declared(value: u64, scale: u32) -> Result<Option<Ratio>, ConversionError> {
    // The raw reader preserves field values. Unknown-marker conventions require
    // header-version evidence before they can be interpreted by this profile.
    if scale == 0 || value == u64::from(u32::MAX) || value == u64::MAX {
        return Err(UNSUPPORTED);
    }
    if value == 0 {
        return Ok(None);
    }
    Ok(Some(Ratio::new(i128::from(value), u64::from(scale))?))
}

fn matches(value: u64, scale: u32, actual: Ratio) -> Result<(), ConversionError> {
    let Some(expected) = declared(value, scale)? else {
        return Ok(());
    };
    let ticks = actual
        .numerator
        .checked_mul(i128::from(scale))
        .ok_or(UNSUPPORTED)?;
    if ticks % i128::from(actual.denominator) != 0 {
        return Err(UNSUPPORTED);
    }
    if expected.compare(actual)? != Ordering::Equal {
        return Err(MISMATCH);
    }
    Ok(())
}

fn maximum(current: &mut Option<Ratio>, value: Ratio) -> Result<(), ConversionError> {
    if match current {
        Some(old) => value.compare(*old)? == Ordering::Greater,
        None => true,
    } {
        *current = Some(value);
    }
    Ok(())
}

fn selection(
    edits: &[Edit],
    fragmented: bool,
    movie_scale: u32,
    first: Ratio,
    end: Ratio,
    required: bool,
) -> Result<Option<Ratio>, ConversionError> {
    let finite = match edits {
        [] => None,
        [edit] if edit.media_time >= 0 && edit.rate_integer == 1 && edit.rate_fraction == 0 => {
            if edit.duration == 0 && !fragmented {
                return Err(UNSUPPORTED);
            }
            declared(edit.duration, movie_scale)?
        }
        _ => return Err(UNSUPPORTED),
    };
    let zero = Ratio::new(0, 1)?;
    if end.compare(zero)? != Ordering::Greater {
        return Err(UNSUPPORTED);
    }
    if let Some(duration) = finite {
        if duration.compare(end)? == Ordering::Greater
            || duration.compare(first)? != Ordering::Greater
        {
            return Err(UNSUPPORTED);
        }
        return Ok(Some(duration));
    }
    if !required {
        return Ok(None);
    }
    // A positive first PTS leaves an initial movie-time gap. Its treatment by
    // known duration headers is not inferred by discarding that gap.
    if first.compare(zero)? == Ordering::Greater {
        return Err(UNSUPPORTED);
    }
    Ok(Some(end))
}

pub(super) fn validate(media: &Media) -> Result<(), ConversionError> {
    let movie = &media.movie;
    let known_movie = declared(movie.duration, movie.timescale)?.is_some();
    if let Some(duration) = movie.fragment_duration {
        declared(duration, movie.timescale)?;
    }
    let mut coded = None;
    let mut selected = None;
    for track in &media.tracks {
        let descriptor = &track.descriptor;
        let samples = &track.samples;
        let zero = Ratio::new(0, 1)?;
        if samples.coded_duration.compare(zero)? != Ordering::Greater {
            return Err(MISMATCH);
        }
        matches(
            descriptor.media_duration,
            descriptor.timescale,
            samples.coded_duration,
        )?;
        maximum(&mut coded, samples.coded_duration)?;
        let known_track = declared(descriptor.tkhd_duration, movie.timescale)?.is_some();
        if let Some(duration) = selection(
            &descriptor.edits,
            movie.fragmented,
            movie.timescale,
            samples.presented_first_pts,
            samples.presented_end_pts,
            known_movie || known_track,
        )? {
            matches(descriptor.tkhd_duration, movie.timescale, duration)?;
            maximum(&mut selected, duration)?;
        }
    }
    let coded = coded.ok_or(MISMATCH)?;
    if let Some(duration) = movie.fragment_duration {
        matches(duration, movie.timescale, coded)?;
    }
    if known_movie {
        matches(
            movie.duration,
            movie.timescale,
            selected.ok_or(UNSUPPORTED)?,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ratio(numerator: i128, denominator: u64) -> Ratio {
        Ratio::new(numerator, denominator).unwrap()
    }
    fn edit(duration: u64) -> Edit {
        Edit {
            duration,
            media_time: 1024,
            rate_integer: 1,
            rate_fraction: 0,
        }
    }

    #[test]
    fn unknown_zero_is_not_an_assertion_of_zero_length_and_ambiguous_markers_are_refused() {
        assert_eq!(declared(0, 48_000).unwrap(), None);
        matches(0, 48_000, ratio(10, 1)).unwrap();
        for value in [u64::from(u32::MAX), u64::MAX] {
            assert_eq!(declared(value, 48_000).unwrap_err(), UNSUPPORTED);
        }
        assert_eq!(declared(0, 0).unwrap_err(), UNSUPPORTED);
    }

    #[test]
    fn known_declarations_compare_exactly_and_one_tick_changes_are_not_tolerated() {
        matches(480_000, 48_000, ratio(10, 1)).unwrap();
        matches(481_024, 48_000, ratio(3758, 375)).unwrap();
        assert_eq!(
            matches(479_999, 48_000, ratio(10, 1)).unwrap_err(),
            MISMATCH
        );
        assert_eq!(
            matches(480_001, 48_000, ratio(10, 1)).unwrap_err(),
            MISMATCH
        );
        assert_eq!(
            matches(123_142, 12_288, ratio(3758, 375)).unwrap_err(),
            UNSUPPORTED
        );
    }

    #[test]
    fn audio_priming_coded_duration_stays_distinct_from_its_finite_presentation() {
        let selected = selection(
            &[edit(480_000)],
            false,
            48_000,
            ratio(-8, 375),
            ratio(10, 1),
            true,
        )
        .unwrap()
        .unwrap();
        assert_eq!(selected, ratio(10, 1));
        matches(480_000, 48_000, selected).unwrap();
        assert_eq!(matches(481_024, 48_000, selected).unwrap_err(), MISMATCH);
        matches(481_024, 48_000, ratio(3758, 375)).unwrap();
    }

    #[test]
    fn open_edits_keep_unknown_headers_unknown_and_do_not_remove_a_positive_gap() {
        assert_eq!(
            selection(&[edit(0)], true, 48_000, ratio(5, 1), ratio(15, 1), false).unwrap(),
            None
        );
        assert_eq!(
            selection(&[edit(0)], true, 48_000, ratio(5, 1), ratio(15, 1), true).unwrap_err(),
            UNSUPPORTED
        );
        assert_eq!(
            selection(&[], true, 48_000, ratio(5, 1), ratio(15, 1), true).unwrap_err(),
            UNSUPPORTED
        );
        assert_eq!(
            selection(&[edit(0)], true, 48_000, ratio(-8, 375), ratio(10, 1), true).unwrap(),
            Some(ratio(10, 1))
        );
        assert_eq!(
            selection(&[edit(0)], false, 48_000, ratio(0, 1), ratio(10, 1), false).unwrap_err(),
            UNSUPPORTED
        );
    }

    #[test]
    fn empty_or_unsupported_edit_selections_never_gain_a_duration_from_headers() {
        assert_eq!(
            selection(&[], true, 48_000, ratio(-1, 1), ratio(0, 1), false).unwrap_err(),
            UNSUPPORTED
        );
        assert_eq!(
            selection(
                &[edit(480_001)],
                false,
                48_000,
                ratio(0, 1),
                ratio(10, 1),
                true
            )
            .unwrap_err(),
            UNSUPPORTED
        );
        assert_eq!(
            selection(
                &[edit(480_000), edit(480_000)],
                true,
                48_000,
                ratio(0, 1),
                ratio(10, 1),
                true
            )
            .unwrap_err(),
            UNSUPPORTED
        );
        let mut invalid = edit(480_000);
        invalid.rate_fraction = 1;
        assert_eq!(
            selection(&[invalid], true, 48_000, ratio(0, 1), ratio(10, 1), true).unwrap_err(),
            UNSUPPORTED
        );
    }

    #[test]
    fn maxima_use_exact_rationals_for_tracks_with_different_time_scales() {
        let mut maximum_duration = None;
        maximum(&mut maximum_duration, ratio(480_000, 48_000)).unwrap();
        maximum(&mut maximum_duration, ratio(122_880, 12_288)).unwrap();
        maximum(&mut maximum_duration, ratio(481_024, 48_000)).unwrap();
        assert_eq!(maximum_duration, Some(ratio(3758, 375)));
    }
}
