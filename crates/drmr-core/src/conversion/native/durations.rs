//! Bind original transport spans to their original written HLS durations.
use super::super::{
    ConversionError,
    audio_presentation::{self, AudioEvidence, AudioPresentation},
    ts::{
        self, NativeEvidence,
        canonical::{Timing, TrackMetadata},
    },
};
use std::path::PathBuf;

const BAD: ConversionError = ConversionError("ts-segment-duration-mismatch");

pub(in crate::conversion) fn transport(
    native: &NativeEvidence,
    segments: &[(PathBuf, String)],
) -> Result<Option<AudioPresentation>, ConversionError> {
    let whole = native.timing()?;
    let spans = native.segment_timings()?;
    if segments.is_empty()
        || spans.len() != segments.len()
        || spans
            .iter()
            .try_fold(0_u64, |sum, span| sum.checked_add(span.count))
            != Some(whole.count)
    {
        return Err(BAD);
    }
    match native.metadata()? {
        TrackMetadata::Video(_) => {
            for (span, (_, declared)) in spans.iter().zip(segments) {
                // This binding is mandatory even when fixed_frame_rate_flag is
                // not asserted. The candidate endpoint comes from VUI clock syntax
                // plus every original DTS/PTS, rather than the final DTS delta.
                ts::check_duration(
                    declared,
                    span.end_pts.checked_sub(span.first_pts).ok_or(BAD)?,
                )?;
            }
            Ok(None)
        }
        TrackMetadata::Audio { sample_rate, .. } => audio(whole, &spans, segments, sample_rate),
    }
}
fn audio(
    whole: Timing,
    spans: &[Timing],
    segments: &[(PathBuf, String)],
    sample_rate: u32,
) -> Result<Option<AudioPresentation>, ConversionError> {
    for (index, span) in spans.iter().enumerate() {
        let declared = &segments[index].1;
        let ticks = span.end_pts.checked_sub(span.first_pts).ok_or(BAD)?;
        if let Some(next) = spans.get(index + 1) {
            // Preserve both existing source spellings: the next PES boundary
            // and the complete coded packet span. Neither is an output clock
            // tolerance; original sample clocks are compared independently.
            let boundary = next.first_pts.checked_sub(span.first_pts).ok_or(BAD)?;
            if ts::check_duration(declared, boundary).is_err()
                && ts::check_duration(declared, ticks).is_err()
            {
                audio_presentation::check_sample_duration(
                    declared,
                    span.count.checked_mul(1024).ok_or(BAD)?,
                    sample_rate,
                )?;
            }
            continue;
        }
        let full = if sample_rate == 44_100 {
            ts::check_final_audio_duration(declared, ticks, span.count)
        } else {
            ts::check_duration(declared, ticks).or_else(|_| {
                audio_presentation::check_sample_duration(
                    declared,
                    span.count.checked_mul(1024).ok_or(BAD)?,
                    sample_rate,
                )
            })
        };
        if full.is_err() {
            return AudioPresentation::from_final(
                AudioEvidence {
                    sample_rate,
                    coded_frames: whole.count,
                    raw_first_pts: whole.first_pts,
                    raw_coded_end: whole.end_pts,
                },
                span.count,
                declared,
            )
            .map(Some);
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversion::ts::CanonicalConfig;

    fn original(rate: u32) -> NativeEvidence {
        let mut native = NativeEvidence::new(CanonicalConfig::Audio {
            sample_rate: rate,
            channels: 2,
        });
        for (first, end) in [(0, 2), (2, 3)] {
            native.begin_segment().unwrap();
            for index in first..end {
                let pts = 126_000
                    + (i64::from(index) * 1024 * 90_000 + i64::from(rate) / 2) / i64::from(rate);
                native
                    .record(
                        &[
                            0xff,
                            0xf1,
                            if rate == 44_100 { 0x50 } else { 0x4c },
                            0x80,
                            1,
                            0x5f,
                            0xfc,
                            1,
                            0x34,
                            0x80,
                        ],
                        pts,
                        pts,
                    )
                    .unwrap();
            }
            native.end_segment().unwrap();
        }
        native
    }
    #[test]
    fn original_declarations_preserve_exact_audio_windows_at_both_rates() {
        for rate in [44_100, 48_000] {
            let native = original(rate);
            for final_samples in [1, 1023, 1024] {
                let segments = vec![
                    (
                        PathBuf::from("first.ts"),
                        audio_presentation::seconds(2048, u128::from(rate)).unwrap(),
                    ),
                    (
                        PathBuf::from("last.ts"),
                        audio_presentation::seconds(final_samples, u128::from(rate)).unwrap(),
                    ),
                ];
                let result = transport(&native, &segments).unwrap();
                if final_samples == 1024 {
                    assert!(result.is_none());
                } else {
                    assert_eq!(
                        u128::from(result.unwrap().presented_samples),
                        2048 + final_samples
                    );
                }
                let mut changed = segments.clone();
                changed[0].1 = audio_presentation::seconds(2049, u128::from(rate)).unwrap();
                assert_eq!(transport(&native, &changed).unwrap_err(), BAD);
                let mut missing = segments;
                missing.pop();
                assert_eq!(transport(&native, &missing).unwrap_err(), BAD);
            }
        }
    }
}
