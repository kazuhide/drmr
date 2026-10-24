//! Saved intent is displayed through a closed, path-free view of its own schema.
use super::{OutputFormat, Result, Value, bad, integer, object, processing_view, select, typed};

fn single(v: &Value, schema: u64, source: OutputFormat) -> Result<Value> {
    object(
        v,
        &[
            "kind",
            "schema_version",
            "plan_id",
            "source_format",
            "source",
            "intent",
            "measurements",
            "required_depth",
            "processing",
        ],
    )?;
    object(&v["intent"], &["format", "target", "output"])?;
    let intent: drmr_core::conversion::plan::OutputIntent = typed(&v["intent"])?;
    let mut out = select(v, &["kind", "schema_version", "plan_id", "source_format"]);
    out["intent"] = select(&v["intent"], &["format", "target"]);
    if schema == 5 {
        out["processing"] = processing_view::selection(&v["processing"], source, intent.format)?;
    } else if v.get("processing").is_some() {
        return Err(bad());
    }
    Ok(out)
}

fn bundle(v: &Value, schema: u64, source: OutputFormat) -> Result<Value> {
    object(
        v,
        &[
            "kind",
            "schema_version",
            "plan_id",
            "source_format",
            "source",
            "output_root",
            "outputs",
            "measurements",
            "required_depth",
            "processing",
        ],
    )?;
    let outputs: Vec<drmr_core::conversion::plan::bundle::BundleOutputIntent> =
        typed(&v["outputs"])?;
    for output in v["outputs"].as_array().ok_or_else(bad)? {
        object(
            output,
            &[
                "id",
                "format",
                "target",
                "drms",
                "conformance",
                "processing",
            ],
        )?;
        if schema != 6 && output.get("processing").is_some() {
            return Err(bad());
        }
    }
    let mut out = select(
        v,
        &[
            "kind",
            "schema_version",
            "plan_id",
            "source_format",
            "outputs",
        ],
    );
    if schema == 6 {
        out["processing"] = processing_view::bundle(&v["processing"], source, &outputs)?;
    } else if v.get("processing").is_some() {
        return Err(bad());
    }
    Ok(out)
}

pub(super) fn project(v: &Value) -> Result<(Value, &'static str, bool)> {
    if v["kind"] != "execution-plan" {
        return Err(bad());
    }
    let schema = integer(&v["schema_version"])?;
    let source = typed(&v["source_format"])?;
    let mut out = match schema {
        1 | 3 | 5 => single(v, schema, source)?,
        2 | 4 | 6 => bundle(v, schema, source)?,
        _ => return Err(bad()),
    };
    if schema >= 3 {
        let _: drmr_core::conversion::ValidationDepth = typed(&v["required_depth"])?;
        out["required_depth"] = v["required_depth"].clone();
    } else if v.get("required_depth").is_some() {
        return Err(bad());
    }
    Ok((out, "plan", true))
}
