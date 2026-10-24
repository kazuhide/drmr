//! Bind public processing evidence to the immutable, validated saved selection.
use super::{Result, Value, bad, typed};
use crate::{common::plans::Plan, common_protocol::OperationName};
use drmr_core::{
    conversion::{
        ProcessingMode,
        plan::bundle::OutputProcessing,
        processing::{BoundaryHandling, ProcessingSelection},
    },
    inspection::{ActualProcessing, OutputFormat},
};

pub(in crate::common) fn bind_plan(
    plan: &Plan,
    operation: OperationName,
    report: &Value,
) -> Result<()> {
    if operation == OperationName::Plan {
        if report["schema_version"] != plan.schema {
            return Err(bad());
        }
        let same = match plan.schema {
            5 => {
                typed::<ProcessingSelection>(plan.processing.as_ref().ok_or_else(bad)?)?
                    == typed::<ProcessingSelection>(&report["processing"])?
            }
            6 => {
                typed::<Vec<OutputProcessing>>(plan.processing.as_ref().ok_or_else(bad)?)?
                    == typed::<Vec<OutputProcessing>>(&report["processing"])?
            }
            _ => report.get("processing").is_none(),
        };
        return same.then_some(()).ok_or_else(bad);
    }
    let outputs = plan.outputs.as_array().ok_or_else(bad)?;
    let bundle = matches!(plan.schema, 2 | 4 | 6);
    let rows = if bundle {
        let rows = report["outputs"].as_array().ok_or_else(bad)?;
        if rows.len() != outputs.len()
            || rows
                .iter()
                .zip(outputs)
                .any(|(row, output)| row["id"] != output["id"])
        {
            return Err(bad());
        }
        rows.iter().collect::<Vec<_>>()
    } else {
        vec![report]
    };
    let selections = saved_selections(plan, outputs.len())?;
    if selections.len() != rows.len() {
        return Err(bad());
    }
    for ((row, output), selection) in rows.iter().zip(outputs).zip(selections) {
        let media = if bundle && operation == OperationName::Convert {
            &row["conversion"]
        } else {
            row
        };
        if operation == OperationName::Convert || media.get("processing").is_some() {
            bind_actual(
                media.get("processing"),
                selection,
                typed(&output["format"])?,
            )?;
        }
    }
    Ok(())
}

fn saved_selections(plan: &Plan, count: usize) -> Result<Vec<Option<ProcessingSelection>>> {
    match plan.schema {
        5 => Ok(vec![Some(typed(
            plan.processing.as_ref().ok_or_else(bad)?,
        )?)]),
        6 => Ok(
            typed::<Vec<OutputProcessing>>(plan.processing.as_ref().ok_or_else(bad)?)?
                .into_iter()
                .map(|choice| Some(choice.selection))
                .collect(),
        ),
        1..=4 => Ok(vec![None; count]),
        _ => Err(bad()),
    }
}

fn bind_actual(
    value: Option<&Value>,
    selection: Option<ProcessingSelection>,
    format: OutputFormat,
) -> Result<()> {
    let Some(value) = value else {
        return selection.is_none().then_some(()).ok_or_else(bad);
    };
    let actual: ActualProcessing = typed(value)?;
    // Legacy plans execute Stitch, but an old report's absent guarantees stay absent.
    if selection.is_none() && actual.mode.is_none() && actual.boundaries.is_none() {
        return Ok(());
    }
    let mode = selection.map_or(ProcessingMode::Stitch, |choice| choice.resolved);
    let boundaries = selection.map_or(
        if format == OutputFormat::Mp4 {
            BoundaryHandling::NotApplicable
        } else {
            BoundaryHandling::Resegment
        },
        |choice| choice.boundaries,
    );
    if actual.mode != Some(mode) || actual.boundaries != Some(boundaries) {
        return Err(bad());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use drmr_core::inspection::{InputPreparation, ProcessingRouteId};

    #[test]
    fn legacy_present_boundaries_follow_the_saved_output_container() {
        let mp4 = serde_json::to_value(ActualProcessing::new(
            ProcessingRouteId::ProgressiveMp4,
            InputPreparation::CompleteFile,
        ))
        .unwrap();
        assert!(bind_actual(Some(&mp4), None, OutputFormat::Mp4).is_ok());
        for format in [OutputFormat::HlsTs, OutputFormat::Hls, OutputFormat::Dash] {
            assert!(bind_actual(Some(&mp4), None, format).is_err());
        }
    }
}
