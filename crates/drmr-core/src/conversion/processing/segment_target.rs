//! Segment targets are metadata intent, separate from exact sample clocks and boundaries.
use crate::conversion::ConversionError;

pub(in crate::conversion) mod cadence;
mod hls_output;
mod policy;
pub(in crate::conversion) use hls_output::finish as finish_hls;
pub(in crate::conversion) use policy::{HlsTargetPolicy, HlsTiming};

pub(in crate::conversion) const HISTORICAL_MS: u32 = 2000;
const INVALID: ConversionError = ConversionError("hls-invalid-target-duration");

/// Read the original bounded text before a localizer can rewrite its headers.
/// Missing declarations remain the caller's profile/completeness decision.
pub(in crate::conversion) fn declared_hls_seconds(
    text: &str,
) -> Result<Option<u64>, ConversionError> {
    let mut found = None;
    for line in text.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            let seconds = value
                .parse::<u64>()
                .ok()
                .filter(|value| *value > 0)
                .filter(|_| value.bytes().all(|byte| byte.is_ascii_digit()))
                .ok_or(INVALID)?;
            if found.replace(seconds).is_some() {
                return Err(ConversionError("hls-duplicate-header"));
            }
        }
    }
    Ok(found)
}

pub(in crate::conversion) fn from_seconds(
    values: impl IntoIterator<Item = u64>,
) -> Result<u32, ConversionError> {
    let mut target = None;
    for value in values {
        let milliseconds = value
            .checked_mul(1000)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or(INVALID)?;
        if target.is_some_and(|target| target != milliseconds) {
            return Err(ConversionError("hls-target-duration-mismatch"));
        }
        target = Some(milliseconds);
    }
    target.ok_or(INVALID)
}

pub(in crate::conversion) fn argument(milliseconds: u32) -> Result<String, ConversionError> {
    if milliseconds == 0 {
        return Err(ConversionError("processing-selection-invalid"));
    }
    let whole = milliseconds / 1000;
    let remainder = milliseconds % 1000;
    if remainder == 0 {
        Ok(whole.to_string())
    } else {
        Ok(format!("{whole}.{remainder:03}")
            .trim_end_matches('0')
            .into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        conversion::processing::{ProcessingMode, ProcessingSelection},
        inspection::OutputFormat,
    };

    #[test]
    fn target_comes_from_the_declared_bound_not_tail_or_frame_durations() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.013968253,\nfirst.ts\n#EXTINF:2.536417233,\nlast.ts\n#EXT-X-ENDLIST\n";
        assert_eq!(declared_hls_seconds(text).unwrap(), Some(6));
        assert_eq!(from_seconds([6, 6, 6]).unwrap(), 6000);
        assert_eq!(
            from_seconds([6, 4]).unwrap_err().0,
            "hls-target-duration-mismatch"
        );
    }

    #[test]
    fn invalid_and_overflowing_targets_are_not_truncated_or_replaced() {
        for declaration in [
            "",
            "0",
            "-1",
            "+6",
            "6.0",
            "18446744073709551616",
            "6\n#EXT-X-TARGETDURATION:6",
        ] {
            assert!(
                declared_hls_seconds(&format!("#EXTM3U\n#EXT-X-TARGETDURATION:{declaration}\n"))
                    .is_err()
            );
        }
        assert!(from_seconds([]).is_err());
        assert!(from_seconds([u64::MAX]).is_err());
        assert!(from_seconds([u64::from(u32::MAX) / 1000 + 1]).is_err());
        assert_eq!(
            from_seconds([u64::from(u32::MAX) / 1000]).unwrap(),
            4_294_967_000
        );
    }

    #[test]
    fn packager_arguments_use_exact_integer_milliseconds() {
        assert_eq!(argument(6000).unwrap(), "6");
        assert_eq!(argument(2000).unwrap(), "2");
        assert_eq!(argument(6001).unwrap(), "6.001");
        assert_eq!(argument(6010).unwrap(), "6.01");
        assert_eq!(argument(1).unwrap(), "0.001");
        assert_eq!(argument(u32::MAX).unwrap(), "4294967.295");
        assert!(argument(0).is_err());
    }
    #[test]
    fn segment_targets_are_positive_and_do_not_apply_to_preserved_or_complete_outputs() {
        let mut selection = ProcessingSelection::resolve(
            ProcessingMode::Stitch,
            OutputFormat::HlsTs,
            OutputFormat::Dash,
        )
        .unwrap();
        for duration in [1, 2000, 6000, u32::MAX] {
            selection.segment_duration_ms = Some(duration);
            selection
                .validate(OutputFormat::HlsTs, OutputFormat::Dash)
                .unwrap();
        }
        for duration in [None, Some(0)] {
            selection.segment_duration_ms = duration;
            assert!(
                selection
                    .validate(OutputFormat::HlsTs, OutputFormat::Dash)
                    .is_err()
            );
        }
        for (mode, output) in [
            (ProcessingMode::Segments, OutputFormat::HlsTs),
            (ProcessingMode::Stitch, OutputFormat::Mp4),
        ] {
            let mut selection =
                ProcessingSelection::resolve(mode, OutputFormat::HlsTs, output).unwrap();
            selection.segment_duration_ms = Some(6000);
            assert!(selection.validate(OutputFormat::HlsTs, output).is_err());
        }
    }

    #[test]
    fn fixed_targets_survive_measurement_source_and_destination_contexts() {
        use crate::{
            conversion::plan::destination::OutputGuard,
            events::NoSink,
            toolrun::{Secrets, ToolRunner},
        };
        use std::sync::Arc;
        let root = crate::native::private_tempdir().unwrap();
        let input = root.path().join("source.mp4");
        std::fs::write(&input, b"\0\0\0\x18ftypisom bounded source").unwrap();
        let source =
            crate::conversion::FrozenSource::capture(&input, &crate::concurrency::Stop::new())
                .unwrap();
        for duration in [2000, 6000] {
            let mut selection = ProcessingSelection::resolve(
                ProcessingMode::Stitch,
                OutputFormat::HlsTs,
                OutputFormat::Dash,
            )
            .unwrap();
            selection.segment_duration_ms = Some(duration);
            let mut command = crate::conversion::Converter::new(
                Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
                "must-not-run",
                "must-not-run",
            )
            .with_saved_processing(Some(selection));
            command.source_hls_target_ms = Some(duration);
            assert_eq!(
                command
                    .with_processing_clone(ProcessingMode::Auto)
                    .source_hls_target_ms,
                Some(duration)
            );
            assert_eq!(
                command.using_frozen_source(&source).source_hls_target_ms,
                Some(duration)
            );
            let (value, _) = command.measure(|command| Ok(command.source_hls_target_ms));
            assert_eq!(value.unwrap(), Some(duration));
            let expected = argument(duration).unwrap();
            assert_eq!(
                command
                    .with_processing_clone(ProcessingMode::Auto)
                    .segment_target_argument()
                    .unwrap(),
                expected
            );
            assert_eq!(
                command
                    .using_frozen_source(&source)
                    .segment_target_argument()
                    .unwrap(),
                expected
            );
            let destination = OutputGuard::new(
                &root
                    .path()
                    .canonicalize()
                    .unwrap()
                    .join(format!("output-{duration}")),
            )
            .unwrap();
            assert_eq!(
                command
                    .using_plan_destination(destination)
                    .segment_target_argument()
                    .unwrap(),
                expected
            );
            let (value, _) = command.measure(crate::conversion::Converter::segment_target_argument);
            assert_eq!(value.unwrap(), expected);
            assert_eq!(
                command
                    .with_saved_processing(None)
                    .segment_target_argument()
                    .unwrap(),
                "2"
            );
        }
    }
}
