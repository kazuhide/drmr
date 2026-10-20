use super::{ConversionError, hls::Manifest, version_token};
use serde_json::Value;

pub(super) fn validate(
    bytes: &[u8],
    expected: &Manifest,
    expected_url: &str,
) -> Result<Option<String>, ConversionError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    if value.get("dataVersion").and_then(Value::as_f64) != Some(1.2)
        || value.get("validatorName").and_then(Value::as_str) != Some("mediastreamvalidator")
        || value.get("url").and_then(Value::as_str) != Some(expected_url)
    {
        return Err(invalid());
    }
    check_messages(&value)?;
    validate_playlist(&value, Some(expected))?;
    Ok(value
        .get("validatorVersion")
        .and_then(Value::as_str)
        .and_then(version_token))
}

fn validate_playlist(value: &Value, expected: Option<&Manifest>) -> Result<(), ConversionError> {
    if value.get("parseFailed").is_some_and(|v| v != false) {
        return Err(ConversionError("manifest-conformance-failed"));
    }
    match value.get("playlistKind").and_then(Value::as_str) {
        Some("media") => {
            let count = value
                .get("parsedSegmentsCount")
                .and_then(Value::as_u64)
                .ok_or_else(invalid)?;
            if count == 0
                || value.get("processedSegmentsCount").and_then(Value::as_u64) != Some(0)
                || value.get("fetchCount").and_then(Value::as_u64) != Some(1)
                || value.get("hasEndTag").and_then(Value::as_bool) != Some(true)
                || expected.is_some_and(|m| !m.media || m.segments as u64 != count)
            {
                return Err(invalid());
            }
            let discontinuities = value
                .get("discontinuities")
                .and_then(Value::as_array)
                .ok_or_else(invalid)?;
            let mut segments = 0_usize;
            for domain in discontinuities {
                let array = domain
                    .get("segments")
                    .and_then(Value::as_array)
                    .ok_or_else(invalid)?;
                for segment in array {
                    if segment.get("processed").and_then(Value::as_bool) != Some(false) {
                        return Err(invalid());
                    }
                }
                segments = segments.checked_add(array.len()).ok_or_else(invalid)?;
            }
            if segments as u64 != count {
                return Err(invalid());
            }
        }
        Some("multivariant") => {
            if expected.is_some_and(|m| m.media) {
                return Err(invalid());
            }
            let variants = value
                .get("variants")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
                .ok_or_else(invalid)?;
            for variant in variants {
                validate_playlist(variant, None)?;
            }
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

fn check_messages(value: &Value) -> Result<(), ConversionError> {
    match value {
        Value::Object(object) => {
            if let Some(messages) = object.get("messages") {
                let messages = messages.as_array().ok_or_else(invalid)?;
                // Parser messages can contain raw signaling and source lines. Do not
                // copy them to reports. The initial strict profile accepts no issues.
                if !messages.is_empty() {
                    return Err(ConversionError("manifest-conformance-failed"));
                }
            }
            for child in object.values() {
                check_messages(child)?;
            }
        }
        Value::Array(array) => {
            for child in array {
                check_messages(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn invalid() -> ConversionError {
    ConversionError("manifest-validator-report-invalid")
}
