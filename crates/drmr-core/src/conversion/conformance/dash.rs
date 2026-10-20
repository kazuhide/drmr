use super::ConversionError;
use serde::Deserialize;

pub(super) fn admit(bytes: &[u8]) -> Result<(), ConversionError> {
    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let doc = roxmltree::Document::parse(text).map_err(|_| invalid())?;
    let root = doc.root_element();
    if root.tag_name().name() != "MPD"
        || root.tag_name().namespace() != Some("urn:mpeg:dash:schema:mpd:2011")
    {
        return Err(invalid());
    }
    if doc.descendants().any(|node| {
        node.tag_name().namespace() == Some("http://www.w3.org/2001/XInclude")
            || node.pi().is_some_and(|pi| pi.target == "xml-stylesheet")
            || node
                .attributes()
                .any(|a| a.namespace() == Some("http://www.w3.org/1999/xlink"))
    }) {
        return Err(ConversionError("manifest-validation-feature-unsupported"));
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    kind: String,
    schema_version: u32,
    scope: String,
    engine: String,
    engine_revision: Option<String>,
    manifest_sha256: String,
    checks: Checks,
    passed: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Checks {
    xsd: Check,
    schematron: Check,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Check {
    completed: bool,
    passed: bool,
    issue_count: u64,
}

pub(super) fn validate(
    bytes: &[u8],
    expected_digest: &str,
) -> Result<Option<String>, ConversionError> {
    let report: Report = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if report.kind != "drmr-dash-manifest-validation"
        || report.schema_version != 1
        || report.scope != "manifest-only"
        || report.engine != "dash-if"
        || report.manifest_sha256 != expected_digest
        || !report.checks.xsd.completed
        || !report.checks.schematron.completed
    {
        return Err(invalid());
    }
    if !report.passed
        || !report.checks.xsd.passed
        || !report.checks.schematron.passed
        || report.checks.xsd.issue_count != 0
        || report.checks.schematron.issue_count != 0
    {
        return Err(ConversionError("manifest-conformance-failed"));
    }
    match report.engine_revision {
        Some(value) if value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()) => {
            Ok(Some(format!("git:{}", value.to_ascii_lowercase())))
        }
        None => Ok(None),
        _ => Err(invalid()),
    }
}

fn invalid() -> ConversionError {
    ConversionError("manifest-validator-report-invalid")
}
