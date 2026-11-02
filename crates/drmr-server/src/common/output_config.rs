//! The HTTP output vocabulary reuses the CLI's strict, path-free configuration.
use drmr_core::conversion::plan::bundle::{
    BundleOutputIntent, OutputConfig, parse_output_config_diagnostic,
};
use serde::{Deserialize, Serialize, de::Error as _, ser::Error as _};
use serde_json::{Value, json};

pub fn serialize<S>(outputs: &[BundleOutputIntent], serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    view(outputs)
        .map_err(|_| S::Error::custom("invalid-output-config"))?
        .serialize(serializer)
}

pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<BundleOutputIntent>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let outputs = Value::deserialize(deserializer)?;
    // A browser may select output conditions, but never a native destination path.
    let bytes = serde_json::to_vec(&json!({"schema_version":4,"outputs":outputs}))
        .map_err(|_| D::Error::custom("invalid-output-config"))?;
    parse_output_config_diagnostic(&bytes)
        .map(|configuration| configuration.outputs)
        .map_err(|_| D::Error::custom("invalid-output-config"))
}

pub fn view(outputs: &[BundleOutputIntent]) -> Result<Value, serde_json::Error> {
    let configuration = serde_json::to_value(OutputConfig {
        output_dir: None,
        outputs: outputs.to_vec(),
    })?;
    Ok(configuration["outputs"].clone())
}

pub fn format_name(format: drmr_core::inspection::OutputFormat) -> &'static str {
    match format {
        drmr_core::inspection::OutputFormat::Hls => "hls-fmp4",
        value => value.as_str(),
    }
}

pub fn plan_view(outputs: &Value, single: bool) -> super::wire::Result<Value> {
    let invalid = super::wire::unavailable;
    if !single {
        let outputs: Vec<BundleOutputIntent> =
            serde_json::from_value(outputs.clone()).map_err(|_| invalid())?;
        return view(&outputs).map_err(|_| invalid());
    }
    let rows = outputs.as_array().ok_or_else(invalid)?;
    if rows.len() != 1 || rows[0].get("id") != Some(&Value::Null) {
        return Err(invalid());
    }
    let row = &rows[0];
    let format = serde_json::from_value(row["format"].clone()).map_err(|_| invalid())?;
    let encryption: drmr_core::inspection::TargetProtection =
        serde_json::from_value(row["target"].clone()).map_err(|_| invalid())?;
    let external = row
        .get("external_manifest_check")
        .map_or(Ok(false), |value| value.as_bool().ok_or_else(invalid))?;
    let mut projected = json!({"id":null,"output_format":format_name(format),
        "encryption":encryption,"external_manifest_check":external});
    if let Some(value) = row.get("manifest_profile") {
        let profile: drmr_core::conversion::manifest_lint::ManifestProfile =
            serde_json::from_value(value.clone()).map_err(|_| invalid())?;
        projected["manifest_profile"] = serde_json::to_value(profile).map_err(|_| invalid())?;
    }
    Ok(json!([projected]))
}
