//! Output placement and manifest addressing retained by a saved plan.
use super::{
    ConversionError, Converter, ProcessingMode, SourceQualification,
    dash_addressing::DashAddressing,
    layout::{LayoutMode, SourceLayout},
};
use crate::inspection::{InputFormat, OutputFormat};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

/// Resource placement and DASH addressing are part of immutable output intent.
/// Missing layout retains the previous generated placement used by older Rust callers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputOptions {
    #[serde(
        default,
        deserialize_with = "present_layout",
        skip_serializing_if = "Option::is_none"
    )]
    pub layout: Option<SourceLayout>,
    #[serde(
        default,
        deserialize_with = "present_addressing",
        skip_serializing_if = "Option::is_none"
    )]
    pub dash_addressing: Option<DashAddressing>,
}

fn present_layout<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<SourceLayout>, D::Error> {
    SourceLayout::deserialize(deserializer).map(Some)
}
fn present_addressing<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<DashAddressing>, D::Error> {
    DashAddressing::deserialize(deserializer).map(Some)
}

impl OutputOptions {
    /// Validate placement and addressing without opening files or granting path authority.
    pub fn validate(
        &self,
        format: OutputFormat,
        processing: ProcessingMode,
    ) -> Result<(), ConversionError> {
        let invalid = ConversionError("output-options-invalid");
        if (format == OutputFormat::Dash) != self.dash_addressing.is_some() {
            return Err(invalid);
        }
        if let Some(layout) = &self.layout {
            if layout.mode != LayoutMode::Source || format == OutputFormat::Mp4 {
                return Err(invalid);
            }
            layout
                .validate_for_processing(format, layout.tracks.len(), processing)
                .map_err(|_| invalid)?;
        }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub(super) struct OutputState {
    layout: LayoutMode,
    addressing: DashAddressing,
    selected: Option<Arc<SourceLayout>>,
}

impl Converter {
    /// Select directory placement for new operations. Existing saved plans keep their own choice.
    #[must_use]
    pub fn with_layout(mut self, layout: LayoutMode) -> Self {
        self.output_options.layout = layout;
        self.output_options.selected = None;
        self
    }

    /// Select the addressing form for a newly generated DASH manifest.
    #[must_use]
    pub fn with_dash_addressing(mut self, addressing: DashAddressing) -> Self {
        self.output_options.addressing = addressing;
        self
    }

    /// Apply current user-facing defaults only when creating new intent.
    #[must_use]
    pub fn with_new_output_defaults(self) -> Self {
        self.with_layout(LayoutMode::Source)
            .with_dash_addressing(DashAddressing::TemplateDuration)
    }

    pub(super) fn layout_mode(&self) -> LayoutMode {
        self.output_options.layout
    }

    pub(super) fn using_admitted_output_options(
        &self,
        input: &Path,
        source_format: InputFormat,
        facts: &[super::layout::QualifiedTrack],
        format: OutputFormat,
        processing: ProcessingMode,
    ) -> Result<Self, ConversionError> {
        let mut command = self.with_processing_clone(self.processing_mode);
        if self.layout_mode() == LayoutMode::Source && format != OutputFormat::Mp4 {
            let layout = SourceLayout::resolve_from_admitted_tracks_with_names(
                input,
                source_format,
                facts,
                (
                    LayoutMode::Source,
                    self.output_options
                        .selected
                        .as_deref()
                        .is_none_or(SourceLayout::has_source_names),
                ),
                format,
                processing,
                &self.backend.stop,
            )?;
            if self
                .output_options
                .selected
                .as_deref()
                .is_some_and(|saved| saved != &layout)
            {
                return Err(ConversionError("execution-plan-output-options-changed"));
            }
            command.output_options.selected = Some(Arc::new(layout));
        }
        Ok(command)
    }

    pub(super) fn check_admitted_output_options(
        &self,
        input: &Path,
        source_format: InputFormat,
        facts: &[super::layout::QualifiedTrack],
        format: OutputFormat,
        processing: ProcessingMode,
    ) -> Result<(), ConversionError> {
        if self.output_options.selected.is_some() {
            self.using_admitted_output_options(input, source_format, facts, format, processing)?;
        }
        Ok(())
    }

    pub(super) fn finish_output_options(
        &self,
        stage: &super::files::Stage,
        format: OutputFormat,
    ) -> Result<(), ConversionError> {
        if let Some(layout) = &self.output_options.selected {
            layout.apply_to_stage(
                stage,
                format,
                self.output_options.addressing,
                &self.backend.stop,
            )?;
        }
        if format == OutputFormat::Dash {
            super::dash::rewrite_output(
                stage.path(),
                self.output_options.addressing,
                self.segment_duration_ms,
                &self.backend.stop,
            )?;
        }
        if let Some(layout) = &self.output_options.selected {
            layout.verify_actual_output(
                stage.path(),
                format,
                self.output_options.addressing,
                &self.backend.stop,
            )?;
        }
        Ok(())
    }

    pub(super) fn using_output_options(&self, options: Option<&OutputOptions>) -> Self {
        let mut command = self.with_processing_clone(self.processing_mode);
        command.output_options = OutputState::default();
        if let Some(options) = options {
            command.output_options.addressing = options.dash_addressing.unwrap_or_default();
            if let Some(layout) = &options.layout {
                command.output_options.layout = LayoutMode::Source;
                command.output_options.selected = Some(Arc::new(layout.clone()));
            }
        }
        command
    }

    pub(super) fn resolved_output_options(
        &self,
        input: &Path,
        qualified: &SourceQualification,
        format: OutputFormat,
        processing: ProcessingMode,
    ) -> Result<OutputOptions, ConversionError> {
        let layout = if self.layout_mode() == LayoutMode::Source && format != OutputFormat::Mp4 {
            Some(SourceLayout::resolve_from_qualified_source_with_names(
                input,
                qualified,
                LayoutMode::Source,
                format,
                processing,
                &self.backend.stop,
                self.output_options
                    .selected
                    .as_deref()
                    .is_none_or(SourceLayout::has_source_names),
            )?)
        } else {
            None
        };
        let options = OutputOptions {
            layout,
            dash_addressing: (format == OutputFormat::Dash)
                .then_some(self.output_options.addressing),
        };
        options.validate(format, processing)?;
        if self.output_options.selected.as_deref() != options.layout.as_ref()
            && self.output_options.selected.is_some()
        {
            return Err(ConversionError("execution-plan-output-options-changed"));
        }
        Ok(options)
    }

    pub(super) fn resolved_output_options_for_saved(
        &self,
        input: &Path,
        qualified: &SourceQualification,
        format: OutputFormat,
        processing: ProcessingMode,
        saved: &OutputOptions,
    ) -> Result<OutputOptions, ConversionError> {
        self.using_output_options(Some(saved))
            .resolved_output_options(input, qualified, format, processing)
    }

    pub(super) fn verify_selected_output_options(
        &self,
        entrypoint: &Path,
        format: OutputFormat,
        processing: ProcessingMode,
    ) -> Result<(), ConversionError> {
        let options = OutputOptions {
            layout: self.output_options.selected.as_deref().cloned(),
            dash_addressing: (format == OutputFormat::Dash)
                .then_some(self.output_options.addressing),
        };
        options.validate(format, processing)?;
        if let Some(layout) = &options.layout {
            layout.verify_actual_output(
                entrypoint
                    .parent()
                    .ok_or(ConversionError("output-options-invalid"))?,
                format,
                self.output_options.addressing,
                &self.backend.stop,
            )?;
        }
        if let Some(addressing) = options.dash_addressing {
            super::dash::verify_output_addressing(
                entrypoint
                    .parent()
                    .ok_or(ConversionError("output-options-invalid"))?,
                addressing,
                &self.backend.stop,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{conversion::dash_addressing::DashAddressing, inspection::OutputFormat};

    #[test]
    fn unused_fields_are_omitted_and_explicit_null_is_not_an_option() {
        assert_eq!(
            serde_json::to_value(OutputOptions::default()).unwrap(),
            serde_json::json!({})
        );
        for value in [
            serde_json::json!({"layout": null}),
            serde_json::json!({"dash_addressing": null}),
            serde_json::json!({"unknown": "private"}),
        ] {
            assert!(serde_json::from_value::<OutputOptions>(value).is_err());
        }
    }

    #[test]
    fn addressing_requires_dash_and_dash_requires_addressing() {
        let options = OutputOptions {
            layout: None,
            dash_addressing: Some(DashAddressing::TemplateDuration),
        };
        assert!(
            options
                .validate(OutputFormat::Dash, ProcessingMode::Stitch)
                .is_ok()
        );
        for format in [OutputFormat::Hls, OutputFormat::HlsTs, OutputFormat::Mp4] {
            assert!(options.validate(format, ProcessingMode::Stitch).is_err());
        }
        assert!(
            OutputOptions::default()
                .validate(OutputFormat::Dash, ProcessingMode::Stitch)
                .is_err()
        );
        assert!(
            OutputOptions::default()
                .validate(OutputFormat::Mp4, ProcessingMode::Stitch)
                .is_ok()
        );
    }
}
