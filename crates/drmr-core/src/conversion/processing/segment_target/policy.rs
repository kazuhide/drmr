//! Source bounds and exact packaging requests have distinct replay policies.
use super::{ConversionError, from_seconds};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::conversion) enum HlsTargetPolicy {
    /// Select the exact request once from the admitted source clocks.
    #[default]
    Fresh,
    /// Schemas 7/8 replay the exact request and inherit the source HLS bound.
    SavedCurrent,
    /// Schemas 1–6 retain their exact request and generated HLS headers.
    Legacy,
}

/// Nonserialized facts obtained during complete source admission.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct HlsTiming {
    pub bound_ms: u32,
    pub request_ms: u32,
}

impl HlsTiming {
    pub fn from_admitted(
        seconds: impl IntoIterator<Item = u64>,
        video_segments: impl IntoIterator<Item = Option<u32>>,
    ) -> Result<Self, ConversionError> {
        let bound_ms = from_seconds(seconds)?;
        let mut request_ms = bound_ms;
        for value in video_segments.into_iter().flatten() {
            if value == 0 {
                return Err(ConversionError("hls-video-cadence-not-supported"));
            }
            request_ms = request_ms.min(value);
        }
        Ok(Self {
            bound_ms,
            request_ms,
        })
    }
}

impl crate::conversion::Converter {
    pub(in crate::conversion) fn inherits_hls_target(&self) -> bool {
        self.hls_target_policy != HlsTargetPolicy::Legacy
    }

    pub(in crate::conversion) fn using_hls_timing(&self, timing: Option<HlsTiming>) -> Self {
        let mut command = self.with_processing_clone(self.processing_mode);
        command.source_hls_target_ms = None;
        if command.inherits_hls_target()
            && let Some(timing) = timing
        {
            command.source_hls_target_ms = Some(timing.bound_ms);
            if command.hls_target_policy == HlsTargetPolicy::Fresh
                && command.segment_duration_ms.is_none()
            {
                command.segment_duration_ms = Some(timing.request_ms);
            }
        }
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        conversion::{Converter, ProcessingMode, processing::ProcessingSelection},
        events::NoSink,
        inspection::OutputFormat,
        toolrun::{Secrets, ToolRunner},
    };
    use std::sync::Arc;

    fn command() -> Converter {
        Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
            "must-not-run",
            "must-not-run",
        )
    }

    #[test]
    fn fresh_requests_use_admitted_clocks_and_saved_requests_are_never_lowered() {
        for (bound, video, expected) in [(3, 2000, 2000), (2, 1000, 1000), (6, 6000, 6000)] {
            let timing = HlsTiming::from_admitted([bound], [Some(video)]).unwrap();
            let fresh = command().using_hls_timing(Some(timing));
            assert_eq!(fresh.segment_duration_ms, Some(expected));
            assert_eq!(
                fresh.source_hls_target_ms,
                Some(u32::try_from(bound).unwrap() * 1000)
            );
            let selection = ProcessingSelection::resolve(
                ProcessingMode::Stitch,
                OutputFormat::Hls,
                OutputFormat::Hls,
            )
            .unwrap();
            for current in [false, true] {
                let saved = command()
                    .with_plan_processing(Some(selection), current)
                    .using_hls_timing(Some(timing));
                assert_eq!(saved.segment_duration_ms, Some(2000));
                assert_eq!(saved.packaging_segment_duration_ms(), 2000);
                assert_eq!(
                    saved.source_hls_target_ms,
                    current.then_some(u32::try_from(bound).unwrap() * 1000)
                );
            }
        }
    }

    #[test]
    fn admitted_facts_keep_audio_fallback_and_strict_common_bounds() {
        assert_eq!(
            HlsTiming::from_admitted([6, 6], [None]).unwrap().request_ms,
            6000
        );
        assert_eq!(
            HlsTiming::from_admitted([3, 3], [Some(2000), Some(1000)])
                .unwrap()
                .request_ms,
            1000
        );
        assert!(HlsTiming::from_admitted([6, 4], [Some(2000)]).is_err());
        assert!(HlsTiming::from_admitted([6], [Some(0)]).is_err());
        assert!(HlsTiming::from_admitted([u64::MAX], [Some(2000)]).is_err());
    }
}
