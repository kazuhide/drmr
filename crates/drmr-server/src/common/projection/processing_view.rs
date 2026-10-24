//! Fixed processing selections expose no source paths or execution secrets.
use super::{Result, Value, bad, typed};
use drmr_core::{
    conversion::{
        ProcessingMode,
        plan::bundle::{BundleOutputIntent, OutputProcessing},
        processing::{BoundaryHandling, ProcessingSelection},
    },
    inspection::OutputFormat,
};

pub(super) fn selection(
    value: &Value,
    source: OutputFormat,
    output: OutputFormat,
) -> Result<Value> {
    let choice: ProcessingSelection = typed(value)?;
    if choice.resolved == ProcessingMode::Auto
        || choice.requested != ProcessingMode::Auto && choice.requested != choice.resolved
    {
        return Err(bad());
    }
    let (boundaries, duration) = match choice.resolved {
        ProcessingMode::Segments => {
            let matching = matches!(
                (source, output),
                (OutputFormat::HlsTs, OutputFormat::HlsTs)
                    | (
                        OutputFormat::Hls | OutputFormat::Dash,
                        OutputFormat::Hls | OutputFormat::Dash
                    )
            );
            if !matching {
                return Err(bad());
            }
            (BoundaryHandling::Preserve, None)
        }
        ProcessingMode::Stitch if output == OutputFormat::Mp4 => {
            (BoundaryHandling::NotApplicable, None)
        }
        ProcessingMode::Stitch => (BoundaryHandling::Resegment, Some(2000)),
        ProcessingMode::Auto => return Err(bad()),
    };
    if choice.boundaries != boundaries || choice.segment_duration_ms != duration {
        return Err(bad());
    }
    serde_json::to_value(choice).map_err(|_| bad())
}

pub(super) fn bundle(
    value: &Value,
    source: OutputFormat,
    outputs: &[BundleOutputIntent],
) -> Result<Value> {
    let choices: Vec<OutputProcessing> = typed(value)?;
    if choices.len() != outputs.len() || outputs.is_empty() || outputs.len() > 8 {
        return Err(bad());
    }
    let mut ids = std::collections::BTreeSet::new();
    for choice in &choices {
        let output = outputs
            .iter()
            .find(|output| output.id == choice.id)
            .ok_or_else(bad)?;
        if !super::safe_id(&choice.id)
            || !ids.insert(&choice.id)
            || output
                .processing
                .is_some_and(|mode| mode != choice.selection.requested)
        {
            return Err(bad());
        }
        selection(
            &serde_json::to_value(choice.selection).map_err(|_| bad())?,
            source,
            output.format,
        )?;
    }
    serde_json::to_value(choices).map_err(|_| bad())
}
