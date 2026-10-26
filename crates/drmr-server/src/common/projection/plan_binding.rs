//! Bind public reports to immutable saved intent and the requested evidence floor.
use super::{Result, Value, bad, typed};
use crate::{common::plans::Plan, common_protocol::OperationName};
use drmr_core::{
    conversion::{
        ProcessingMode, ValidationDepth,
        plan::bundle::OutputProcessing,
        processing::{BoundaryHandling, ProcessingSelection},
    },
    inspection::{ActualProcessing, OutputFormat, ProcessingRouteId},
};

pub(in crate::common) fn bind_plan(
    plan: &Plan,
    operation: OperationName,
    requested: ValidationDepth,
    report: &Value,
) -> Result<()> {
    if operation == OperationName::Plan {
        if plan.required_depth < requested {
            return Err(bad());
        }
        let saved: Value = serde_json::from_slice(&plan.read()?).map_err(|_| bad())?;
        if saved.get("output_options") != report.get("output_options") {
            return Err(bad());
        }
        return (super::plan_view::project(&saved)?.0 == super::plan_view::project(report)?.0)
            .then_some(())
            .ok_or_else(bad);
    }
    let outputs = plan.outputs.as_array().ok_or_else(bad)?;
    let bundle = matches!(plan.schema, 2 | 4 | 6 | 8);
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
        bind_depth(&media["verification"], requested.max(plan.required_depth))?;
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

pub(in crate::common) fn bind_analysis_depth(
    report: &Value,
    requested: ValidationDepth,
) -> Result<()> {
    bind_depth(
        if report["schema_version"] == 1 {
            report
        } else {
            &report["verification"]
        },
        requested,
    )
}

fn bind_depth(proof: &Value, minimum: ValidationDepth) -> Result<()> {
    // The closed projection already checked the proof's schema and evidence scope.
    let reported = if proof["schema_version"] == 1 {
        ValidationDepth::Decoded
    } else {
        typed(&proof["required_depth"])?
    };
    (reported >= minimum).then_some(()).ok_or_else(bad)
}

fn saved_selections(plan: &Plan, count: usize) -> Result<Vec<Option<ProcessingSelection>>> {
    match plan.schema {
        5 | 7 => Ok(vec![Some(typed(
            plan.processing.as_ref().ok_or_else(bad)?,
        )?)]),
        6 | 8 => Ok(
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
        // Segment-only routes did not exist before explicit processing guarantees.
        return (!matches!(
            actual.route,
            ProcessingRouteId::Fmp4Segments | ProcessingRouteId::MpegTsSegments
        ))
        .then_some(())
        .ok_or_else(bad);
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
