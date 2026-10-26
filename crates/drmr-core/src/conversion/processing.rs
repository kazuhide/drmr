//! Processing intent is separate from container selection and encryption protection.
pub(in crate::conversion) mod segment_target;

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
    /// Exact Shaka Packager request in milliseconds, separate from an HLS duration bound.
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
        let valid_duration = if expected.segment_duration_ms.is_some() {
            self.segment_duration_ms.is_some_and(|value| value > 0)
        } else {
            self.segment_duration_ms.is_none()
        };
        if self.boundaries != expected.boundaries || !valid_duration {
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
    pub(in crate::conversion) fn resolve_processing(
        &self,
        timing: Option<segment_target::HlsTiming>,
        requested: ProcessingMode,
        source: OutputFormat,
        output: OutputFormat,
    ) -> Result<ProcessingSelection, ConversionError> {
        let mut selection = ProcessingSelection::resolve(requested, source, output)?;
        if selection.segment_duration_ms.is_some() {
            selection.segment_duration_ms = Some(self.segment_duration_ms.unwrap_or_else(|| {
                timing.map_or(segment_target::HISTORICAL_MS, |timing| timing.request_ms)
            }));
        }
        Ok(selection)
    }

    pub(in crate::conversion) fn with_saved_processing(
        mut self,
        selection: Option<ProcessingSelection>,
    ) -> Self {
        self.hls_target_policy = segment_target::HlsTargetPolicy::Legacy;
        self.source_hls_target_ms = None;
        self.processing_mode = selection.map_or(ProcessingMode::Stitch, |choice| choice.resolved);
        // Schemas without an explicit selection keep historical packaging rather
        // than silently adopting the current source-inheritance default.
        self.segment_duration_ms = Some(
            selection
                .and_then(|choice| choice.segment_duration_ms)
                .unwrap_or(segment_target::HISTORICAL_MS),
        );
        self
    }

    pub(in crate::conversion) fn with_plan_processing(
        self,
        selection: Option<ProcessingSelection>,
        current: bool,
    ) -> Self {
        let mut command = self.with_saved_processing(selection);
        if current {
            command.hls_target_policy = segment_target::HlsTargetPolicy::SavedCurrent;
        }
        command
    }

    pub(in crate::conversion) fn segment_target_argument(&self) -> Result<String, ConversionError> {
        segment_target::argument(self.packaging_segment_duration_ms())
    }

    pub(in crate::conversion) fn packaging_segment_duration_ms(&self) -> u32 {
        self.segment_duration_ms
            .unwrap_or(segment_target::HISTORICAL_MS)
    }
    /// Infer an omitted output format from bounded local metadata.
    /// Forced segment processing retains the declared HLS container type. This
    /// choice does not replace native admission of the actual media resources.
    pub fn default_output_format(
        &self,
        input: &std::path::Path,
    ) -> Result<OutputFormat, ConversionError> {
        self.backend.check_cancelled()?;
        let report = crate::inspection::inspect(input)
            .map_err(|_| ConversionError("invalid-input-media"))?;
        if self.processing_mode != ProcessingMode::Segments
            || report.format != crate::inspection::InputFormat::Hls
        {
            return Ok(report.format.into());
        }
        let mut mapped = None;
        for media in super::presentation::hls_media_paths(input)? {
            self.backend.check_cancelled()?;
            let bytes = super::files::read_manifest(&media.path)?;
            let text =
                std::str::from_utf8(&bytes).map_err(|_| ConversionError("hls-invalid-utf8"))?;
            let has_map = text
                .lines()
                .any(|line| line.trim().starts_with("#EXT-X-MAP:"));
            if mapped.is_some_and(|previous| previous != has_map) {
                return Err(ConversionError("ts-presentation-layout-not-supported"));
            }
            mapped = Some(has_map);
        }
        match mapped {
            Some(true) => Ok(OutputFormat::Hls),
            Some(false) => Ok(OutputFormat::HlsTs),
            None => Err(ConversionError("processing-segments-source-required")),
        }
    }

    pub(in crate::conversion) fn with_processing_clone(&self, mode: ProcessingMode) -> Self {
        Self {
            backend: self.backend.clone(),
            processing_mode: mode,
            segment_duration_ms: self.segment_duration_ms,
            output_options: self.output_options.clone(),
            source_hls_target_ms: self.source_hls_target_ms,
            hls_target_policy: self.hls_target_policy,
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

    pub(in crate::conversion) fn check_complete_file_processing(
        &self,
    ) -> Result<(), ConversionError> {
        if self.processing_mode == ProcessingMode::Segments {
            Err(ConversionError("processing-segments-source-required"))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BoundaryHandling, ProcessingMode, ProcessingSelection};
    use crate::inspection::OutputFormat;

    #[test]
    fn forced_segments_defaults_to_the_declared_hls_media_container() {
        use crate::{
            conversion::Converter,
            events::NoSink,
            toolrun::{Secrets, ToolRunner},
        };
        use std::{fs, sync::Arc};
        let directory = tempfile::tempdir().unwrap();
        let master = directory.path().join("master.m3u8");
        let media = directory.path().join("media.m3u8");
        fs::write(
            &master,
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nmedia.m3u8\n",
        )
        .unwrap();
        let converter = Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
            "must-not-run-packager",
            "must-not-run-ffmpeg",
        );
        for map in ["", "#EXT-X-MAP:URI=\"init.mp4\"\n"] {
            fs::write(
                &media,
                format!(
                    "#EXTM3U\n#EXT-X-TARGETDURATION:2\n{map}#EXTINF:2,\nsegment\n#EXT-X-ENDLIST\n"
                ),
            )
            .unwrap();
            for path in [&master, &media] {
                assert_eq!(
                    converter.default_output_format(path).unwrap(),
                    OutputFormat::Hls
                );
                assert_eq!(
                    converter
                        .with_processing_clone(ProcessingMode::Stitch)
                        .default_output_format(path)
                        .unwrap(),
                    OutputFormat::Hls
                );
                assert_eq!(
                    converter
                        .with_processing_clone(ProcessingMode::Segments)
                        .default_output_format(path)
                        .unwrap(),
                    if map.is_empty() {
                        OutputFormat::HlsTs
                    } else {
                        OutputFormat::Hls
                    }
                );
            }
        }
        fs::write(
            directory.path().join("other.m3u8"),
            "#EXTM3U\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
        fs::write(&master, "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100\nmedia.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=200\nother.m3u8\n").unwrap();
        assert_eq!(
            converter
                .with_processing_clone(ProcessingMode::Segments)
                .default_output_format(&master)
                .unwrap_err()
                .0,
            "ts-presentation-layout-not-supported"
        );
    }

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
    fn format_resolution_keeps_the_historical_fallback_until_the_source_is_bound() {
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
