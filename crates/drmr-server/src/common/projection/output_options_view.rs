//! Saved path maps are validated privately and represented by closed public labels.
use super::{OutputFormat, Result, Value, bad, json, typed};
use drmr_core::conversion::{
    output_options::OutputOptions,
    plan::{
        SavedPlan,
        bundle::{BundleOutputIntent, OutputProcessing, OutputSettings},
    },
    processing::ProcessingSelection,
};

fn view(options: &OutputOptions, format: OutputFormat) -> Value {
    json!({"layout":if options.layout.is_some() {"source"} else if format == OutputFormat::Mp4 {"not-applicable"} else {"previous-saved"},
        "dash_addressing":options.dash_addressing})
}

fn filename_schema(options: &OutputOptions, current: bool) -> Result<()> {
    if let Some(layout) = &options.layout {
        let supported = if current {
            layout.has_source_names()
        } else {
            layout.has_legacy_names()
        };
        if !supported {
            return Err(bad());
        }
    }
    Ok(())
}

pub(super) fn single(
    value: &Value,
    format: OutputFormat,
    processing: &Value,
    current: bool,
) -> Result<Value> {
    let options: OutputOptions = typed(value)?;
    let processing: ProcessingSelection = typed(processing)?;
    filename_schema(&options, current)?;
    options
        .validate(format, processing.resolved)
        .map_err(|_| bad())?;
    Ok(view(&options, format))
}

pub(super) fn bundle(
    value: &Value,
    outputs: &[BundleOutputIntent],
    processing: &Value,
    current: bool,
) -> Result<Value> {
    let settings: Vec<OutputSettings> = typed(value)?;
    let processing: Vec<OutputProcessing> = typed(processing)?;
    if settings.len() != outputs.len() || processing.len() != outputs.len() {
        return Err(bad());
    }
    settings
        .iter()
        .zip(outputs)
        .zip(processing)
        .map(|((setting, output), choice)| {
            filename_schema(&setting.options, current)?;
            if setting.id != output.id
                || choice.id != output.id
                || output
                    .dash_addressing
                    .is_some_and(|mode| Some(mode) != setting.options.dash_addressing)
            {
                return Err(bad());
            }
            setting
                .options
                .validate(output.format, choice.selection.resolved)
                .map_err(|_| bad())?;
            Ok(json!({"id":setting.id,"options":view(&setting.options, output.format)}))
        })
        .collect::<Result<Vec<_>>>()
        .map(Value::Array)
}

pub(in crate::common) fn saved(plan: &SavedPlan) -> Option<Value> {
    match plan {
        SavedPlan::Single(plan) => plan.output_options.as_ref().map(|options| view(options, plan.intent.format)),
        SavedPlan::Bundle(plan) => plan.output_options.as_ref().map(|settings| Value::Array(settings.iter().zip(&plan.outputs)
            .map(|(setting, output)| json!({"id":setting.id,"options":view(&setting.options, output.format)})).collect())),
    }
}
