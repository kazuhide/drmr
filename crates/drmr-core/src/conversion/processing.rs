//! Processing intent is separate from container selection and encryption protection.

use super::ConversionError;
use crate::inspection::OutputFormat;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Choose the processing unit independently of the requested output container.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessingMode {
    /// Select a qualified route for the actual input and output.
    #[default]
    Auto,
    /// Process original segments and retain their sample boundaries and timing.
    Segments,
    /// Join source segments when needed, then package the requested output.
    /// HLS and DASH output is segmented again.
    Stitch,
}

impl ProcessingMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Segments => "segments",
            Self::Stitch => "stitch",
        }
    }
}

/// The boundary guarantee is recorded separately from the requested processing name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BoundaryHandling {
    Preserve,
    Resegment,
    NotApplicable,
}

/// A saved, resolved choice is executed without selecting an automatic route again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessingSelection {
    pub requested: ProcessingMode,
    pub resolved: ProcessingMode,
    pub boundaries: BoundaryHandling,
    pub segment_duration_ms: Option<u32>,
}

impl ProcessingSelection {
    pub(in crate::conversion) fn resolve(
        requested: ProcessingMode,
        source: OutputFormat,
        output: OutputFormat,
    ) -> Result<Self, ConversionError> {
        let resolved = match requested {
            ProcessingMode::Auto | ProcessingMode::Stitch => ProcessingMode::Stitch,
            ProcessingMode::Segments => {
                segment_container(source, output)?;
                ProcessingMode::Segments
            }
        };
        let (boundaries, segment_duration_ms) = match resolved {
            ProcessingMode::Segments => (BoundaryHandling::Preserve, None),
            ProcessingMode::Stitch if output == OutputFormat::Mp4 => {
                (BoundaryHandling::NotApplicable, None)
            }
            ProcessingMode::Stitch => (BoundaryHandling::Resegment, Some(2000)),
            ProcessingMode::Auto => unreachable!("automatic intent is resolved above"),
        };
        Ok(Self {
            requested,
            resolved,
            boundaries,
            segment_duration_ms,
        })
    }

    pub(in crate::conversion) fn validate(
        &self,
        source: OutputFormat,
        output: OutputFormat,
    ) -> Result<(), ConversionError> {
        const INVALID: ConversionError = ConversionError("processing-selection-invalid");
        if self.resolved == ProcessingMode::Auto
            || (self.requested != ProcessingMode::Auto && self.requested != self.resolved)
        {
            return Err(INVALID);
        }
        let expected = Self::resolve(self.resolved, source, output).map_err(|_| INVALID)?;
        if self.boundaries != expected.boundaries
            || self.segment_duration_ms != expected.segment_duration_ms
        {
            return Err(INVALID);
        }
        Ok(())
    }
}

fn segment_container(source: OutputFormat, output: OutputFormat) -> Result<(), ConversionError> {
    if source == OutputFormat::Mp4 {
        return Err(ConversionError("processing-segments-source-required"));
    }
    if matches!(
        (source, output),
        (OutputFormat::HlsTs, OutputFormat::HlsTs)
            | (
                OutputFormat::Hls | OutputFormat::Dash,
                OutputFormat::Hls | OutputFormat::Dash
            )
    ) {
        Ok(())
    } else {
        Err(ConversionError("processing-container-change-not-supported"))
    }
}

impl super::Converter {
    pub(in crate::conversion) fn with_processing_clone(&self, mode: ProcessingMode) -> Self {
        Self {
            backend: self.backend.clone(),
            processing_mode: mode,
            maximum_workers: self.maximum_workers,
            downloaded: self.downloaded.clone(),
            frozen_source: self.frozen_source.clone(),
            planned_destination: self.planned_destination.clone(),
            output_signaling: self.output_signaling.clone(),
        }
    }
    pub(in crate::conversion) fn check_saved_processing(
        &self,
        resolved: ProcessingMode,
    ) -> Result<(), ConversionError> {
        if self.processing_mode != ProcessingMode::Auto && self.processing_mode != resolved {
            return Err(ConversionError("processing-plan-override-not-supported"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{BoundaryHandling, ProcessingMode, ProcessingSelection};
    use crate::inspection::OutputFormat;

    #[test]
    fn public_modes_round_trip_without_accepting_unapproved_aliases() {
        for (name, mode) in [
            ("auto", ProcessingMode::Auto),
            ("segments", ProcessingMode::Segments),
            ("stitch", ProcessingMode::Stitch),
        ] {
            assert_eq!(
                serde_json::from_str::<ProcessingMode>(&format!("\"{name}\"")).unwrap(),
                mode
            );
            assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{name}\""));
            assert_eq!(mode.as_str(), name);
        }
        for invalid in ["\"tracks\"", "\"segment\"", "\"STITCH\"", "null", "{}"] {
            assert!(serde_json::from_str::<ProcessingMode>(invalid).is_err());
        }
        assert_eq!(ProcessingMode::default(), ProcessingMode::Auto);
    }

    #[test]
    fn a_forced_segment_route_requires_original_matching_container_boundaries() {
        for source in [OutputFormat::Mp4, OutputFormat::HlsTs] {
            let error =
                ProcessingSelection::resolve(ProcessingMode::Segments, source, OutputFormat::Hls)
                    .unwrap_err();
            assert_eq!(
                error.0,
                if source == OutputFormat::Mp4 {
                    "processing-segments-source-required"
                } else {
                    "processing-container-change-not-supported"
                }
            );
        }
        let selection = ProcessingSelection::resolve(
            ProcessingMode::Segments,
            OutputFormat::Hls,
            OutputFormat::Dash,
        )
        .unwrap();
        assert_eq!(selection.resolved, ProcessingMode::Segments);
        assert_eq!(selection.boundaries, BoundaryHandling::Preserve);
        assert_eq!(selection.segment_duration_ms, None);
    }

    #[test]
    fn automatic_requests_keep_the_existing_resegmentation_default() {
        let selection = ProcessingSelection::resolve(
            ProcessingMode::Auto,
            OutputFormat::Hls,
            OutputFormat::Dash,
        )
        .unwrap();
        assert_eq!(selection.resolved, ProcessingMode::Stitch);
        assert_eq!(selection.boundaries, BoundaryHandling::Resegment);
        assert_eq!(selection.segment_duration_ms, Some(2000));
        let complete = ProcessingSelection::resolve(
            ProcessingMode::Stitch,
            OutputFormat::Hls,
            OutputFormat::Mp4,
        )
        .unwrap();
        assert_eq!(complete.boundaries, BoundaryHandling::NotApplicable);
        assert_eq!(complete.segment_duration_ms, None);
    }

    #[test]
    fn a_saved_automatic_choice_is_validated_without_selecting_the_route_again() {
        let saved = ProcessingSelection {
            requested: ProcessingMode::Auto,
            resolved: ProcessingMode::Segments,
            boundaries: BoundaryHandling::Preserve,
            segment_duration_ms: None,
        };
        saved
            .validate(OutputFormat::Hls, OutputFormat::Dash)
            .unwrap();
        let invalid = ProcessingSelection {
            requested: ProcessingMode::Stitch,
            ..saved
        };
        assert_eq!(
            invalid
                .validate(OutputFormat::Hls, OutputFormat::Dash)
                .unwrap_err()
                .0,
            "processing-selection-invalid"
        );
    }
}
