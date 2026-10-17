//! Allowlisted structured diagnostics that exclude credentials.

use crate::hls::is_component;
use crate::pyutil;
use serde_json::{Map, Value};

/// The classification messages a clear-HLS record may carry (`_LOG_MESSAGES`).
const LOG_MESSAGES: [&str; 51] = [
    "ambiguous-playlist",
    "ambiguous-source-topology",
    "analysis-complete",
    "byte-budget-exceeded",
    "changing-codec-configuration",
    "changing-container-unsupported",
    "changing-initialization-unsupported",
    "discontinuity-unsupported",
    "disk-budget-exceeded",
    "duplicate-playlist",
    "empty-source-resource",
    "encrypted-source-unsupported",
    "invalid-asset-id",
    "invalid-attributes",
    "invalid-byte-range",
    "invalid-duration",
    "invalid-fetch-policy",
    "invalid-playlist",
    "invalid-source-redirect",
    "invalid-target-duration",
    "live-unsupported",
    "media-probe-failed",
    "missing-rendition-group",
    "non-independent-video",
    "package-config-invalid",
    "packaging-complete",
    "packaging-failed",
    "playlist-budget-exceeded",
    "playlist-cycle",
    "redirect-budget-exceeded",
    "segment-budget-exceeded",
    "snapshot-changed",
    "snapshot-invalid",
    "snapshot-write-failed",
    "source-config-invalid",
    "source-fetch-failed",
    "source-http-failed",
    "source-not-allowed",
    "source-read-failed",
    "source-resolution-failed",
    "source-snapshot-exists",
    "source-timeout",
    "track-failed",
    "track-packaged",
    "track-started",
    "unsafe-output-path",
    "unsupported-container",
    "unsupported-content-encoding",
    "unsupported-playlist-tag",
    "unsupported-source-track",
    "unsupported-subtitle-profile",
];

/// The analysis progress messages (`_ANALYSIS_PROGRESS`).
const ANALYSIS_PROGRESS: [&str; 7] = [
    "analysis-started",
    "playlist-started",
    "segment-downloaded",
    "probe-started",
    "segment-probed",
    "playlist-complete",
    "analysis-complete",
];

/// The tools whose name a tool record may carry (`_TOOLS`).
const TOOLS: [&str; 4] = ["packager", "ffmpeg", "ffprobe", "mp4dump"];

/// `_TOOL_TEXT_LIMIT`: 256 K characters plus room for the runner's own cut note.
pub const TOOL_TEXT_LIMIT: usize = 256 * 1024 + 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("unhashable value in a log record")]
pub struct Unhashable;

/// `value in {…}` for a set of strings: a list or a dict raises `TypeError` (unhashable).
fn member(value: Option<&Value>, set: &[&str]) -> Result<bool, Unhashable> {
    match value {
        Some(Value::Array(_) | Value::Object(_)) => Err(Unhashable),
        Some(Value::String(text)) => Ok(set.contains(&text.as_str())),
        _ => Ok(false),
    }
}

/// `type(value) in {int, float} and math.isfinite(value) and value >= 0`.
fn counter(value: Option<&Value>) -> Option<Value> {
    match value {
        Some(Value::Number(number))
            if number.as_f64().is_some_and(|n| n.is_finite() && n >= 0.0) =>
        {
            Some(Value::Number(number.clone()))
        }
        _ => None,
    }
}

/// `HlsService.safe_log(line)`: the line as the allowlist lets it through (a `json.dumps`
/// line).
pub fn safe_log(line: &str) -> Result<String, Unhashable> {
    let row = match pyutil::loads_nonfinite_as_null(line) {
        Some(Value::Object(row)) => row,
        _ => Map::new(),
    };
    if member(row.get("event"), &["command", "tool-output"])?
        && row.get("stage").and_then(Value::as_str) == Some("tool")
    {
        return safe_tool_record(&row);
    }
    let mut safe = Map::new();
    let stage = row
        .get("stage")
        .and_then(Value::as_str)
        .filter(|stage| ["analysis", "hls-source", "package-hls", "report"].contains(stage))
        .unwrap_or("raw");
    let level = row
        .get("level")
        .and_then(Value::as_str)
        .filter(|level| ["info", "warning", "error"].contains(level))
        .unwrap_or("info");
    let message = row
        .get("message")
        .and_then(Value::as_str)
        .filter(|message| LOG_MESSAGES.contains(message) || ANALYSIS_PROGRESS.contains(message))
        .unwrap_or("job-progress");
    safe.insert("stage".into(), Value::from(stage));
    safe.insert("level".into(), Value::from(level));
    safe.insert("message".into(), Value::from(message));
    for name in ["track_id", "rendition_id", "analysis_id"] {
        if let Some(Value::String(text)) = row.get(name)
            && is_component(text)
        {
            safe.insert(name.into(), Value::from(text.clone()));
        }
    }
    for name in ["index", "total", "track_count", "duration_s"] {
        if let Some(value) = counter(row.get(name)) {
            safe.insert(name.into(), value);
        }
    }
    Ok(pyutil::dumps(&Value::Object(safe)))
}

/// `_safe_tool_record(row)`: a `command` / `tool-output` record with bounded, typed fields.
fn safe_tool_record(row: &Map<String, Value>) -> Result<String, Unhashable> {
    let event = row.get("event").cloned().unwrap_or(Value::Null);
    let mut safe = Map::new();
    safe.insert("stage".into(), Value::from("tool"));
    safe.insert("event".into(), event.clone());
    safe.insert("message".into(), event.clone());
    let level = if member(row.get("level"), &["info", "warning", "error"])? {
        row["level"].clone()
    } else {
        Value::from("info")
    };
    safe.insert("level".into(), level);
    let tool = if member(row.get("tool"), &TOOLS)? {
        row["tool"].clone()
    } else {
        Value::from("tool")
    };
    safe.insert("tool".into(), tool);
    if let Some(Value::String(ts)) = row.get("ts")
        && ts.chars().count() <= 64
    {
        safe.insert("ts".into(), Value::from(ts.clone()));
    }
    if let Some(Value::String(rendition)) = row.get("rendition_id")
        && is_component(rendition)
    {
        safe.insert("rendition_id".into(), Value::from(rendition.clone()));
    }
    if let Some(Value::Number(call_id)) = row.get("call_id")
        && (call_id.is_u64() || call_id.as_i64().is_some_and(|id| id >= 0))
    {
        safe.insert("call_id".into(), Value::Number(call_id.clone()));
    }
    if event.as_str() == Some("command") {
        let argv = match row.get("argv") {
            Some(Value::Array(items)) if items.iter().all(Value::is_string) => items
                .iter()
                .take(512)
                .map(|token| {
                    let text: String = token
                        .as_str()
                        .unwrap_or_default()
                        .chars()
                        .take(4096)
                        .collect();
                    Value::from(replace_urls(&text))
                })
                .collect(),
            _ => Vec::new(),
        };
        safe.insert("argv".into(), Value::Array(argv));
    } else {
        let returncode = match row.get("returncode") {
            Some(Value::Number(code)) if code.is_i64() || code.is_u64() => {
                Value::Number(code.clone())
            }
            _ => Value::Null,
        };
        safe.insert("returncode".into(), returncode);
        safe.insert(
            "duration_s".into(),
            counter(row.get("duration_s")).unwrap_or(Value::Null),
        );
        let mut truncated = row.get("truncated") == Some(&Value::Bool(true));
        for name in ["stdout", "stderr"] {
            let text = match row.get(name) {
                Some(Value::String(text)) => {
                    let mut chars = text.chars();
                    let head: String = chars.by_ref().take(TOOL_TEXT_LIMIT).collect();
                    truncated |= chars.next().is_some();
                    replace_urls(&head)
                }
                _ => String::new(),
            };
            safe.insert(name.into(), Value::from(text));
        }
        safe.insert("truncated".into(), Value::Bool(truncated));
    }
    Ok(pyutil::dumps(&Value::Object(safe)))
}

/// `_URL.sub("<url>", text)` with `_URL = (?<![A-Za-z0-9+.\-])[A-Za-z][A-Za-z0-9+.\-]{0,31}://
/// [^\s"'<>)\]]+`: a scheme-shaped run (a letter first, at most 32 characters, not preceded by a
/// scheme character) followed by `://` and at least one character up to whitespace, a quote, `<`,
/// `>`, `)` or `]`. Linear: a run that does not match is skipped whole (no position inside it
/// can start a match, the lookbehind forbids it).
pub fn replace_urls(text: &str) -> String {
    let scheme_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-');
    let stop = |c: char| {
        drmr_core::hls_cli::py_isspace(c) || matches!(c, '"' | '\'' | '<' | '>' | ')' | ']')
    };
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if !scheme_char(c) {
            out.push(c);
            index += 1;
            continue;
        }
        // The start of a maximal run of scheme characters.
        let mut run_end = index;
        while run_end < chars.len() && scheme_char(chars[run_end]) {
            run_end += 1;
        }
        let candidate = c.is_ascii_alphabetic()
            && run_end - index <= 32
            && chars.get(run_end..run_end + 3) == Some(&[':', '/', '/'][..]);
        if candidate {
            let tail_start = run_end + 3;
            let mut tail_end = tail_start;
            while tail_end < chars.len() && !stop(chars[tail_end]) {
                tail_end += 1;
            }
            if tail_end > tail_start {
                out.push_str("<url>");
                index = tail_end;
                continue;
            }
        }
        out.extend(&chars[index..run_end]);
        index = run_end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_finite_numbers_drop_only_their_own_field() {
        let cases = [
            (
                r#"{"stage": "hls-source", "message": "analysis-complete", "duration_s": Infinity, "total": 3}"#,
                r#"{"stage": "hls-source", "level": "info", "message": "analysis-complete", "total": 3}"#,
            ),
            (
                r#"{"stage": "tool", "event": "tool-output", "tool": "ffprobe", "returncode": 0, "duration_s": NaN, "stdout": "x", "stderr": ""}"#,
                r#"{"stage": "tool", "event": "tool-output", "message": "tool-output", "level": "info", "tool": "ffprobe", "returncode": 0, "duration_s": null, "stdout": "x", "stderr": "", "truncated": false}"#,
            ),
            (
                "NaN",
                r#"{"stage": "raw", "level": "info", "message": "job-progress"}"#,
            ),
        ];
        for (line, expected) in cases {
            assert_eq!(safe_log(line).unwrap(), expected, "{line}");
        }
        assert_eq!(
            crate::jobs::ensure_jsonl(r#"{"stage": "report", "psnr": Infinity}"#),
            r#"{"stage": "report", "psnr": Infinity}"#,
            "a conversion line with a non-finite number is JSON for Python, kept verbatim"
        );
    }

    #[test]
    fn urls_are_replaced_and_their_closing_quote_kept() {
        assert_eq!(
            replace_urls("fetched https://origin.test/?token=PRIVATE ok"),
            "fetched <url> ok"
        );
        assert_eq!(
            replace_urls("key=\"https://x.test/k?t=1\" end"),
            "key=\"<url>\" end"
        );
        assert_eq!(replace_urls("skd://secret"), "<url>");
        assert_eq!(
            replace_urls("ahttps://x"),
            "<url>",
            "the run starts at its first letter"
        );
        assert_eq!(
            replace_urls("1https://x"),
            "1https://x",
            "a run starting with a digit is no URL"
        );
        assert_eq!(
            replace_urls("x://"),
            "x://",
            "at least one character after the scheme"
        );
        let long = format!("{}://x", "a".repeat(33));
        assert_eq!(
            replace_urls(&long),
            long,
            "a scheme is at most 32 characters"
        );
        assert_eq!(replace_urls("in=/output/a/x.mp4"), "in=/output/a/x.mp4");
    }

    #[test]
    fn tool_records_are_bounded() {
        let line = r#"{"stage": "tool", "event": "command", "tool": "ffprobe", "argv": ["ffprobe", "-i", "/output/a/seg.mp4", 5], "level": "info"}"#;
        assert_eq!(
            safe_log(line).unwrap(),
            r#"{"stage": "tool", "event": "command", "message": "command", "level": "info", "tool": "ffprobe", "argv": []}"#
        );
        assert!(
            safe_log(r#"{"event": [1]}"#).is_err(),
            "an unhashable event raises"
        );
        assert_eq!(
            safe_log("not json").unwrap(),
            r#"{"stage": "raw", "level": "info", "message": "job-progress"}"#
        );
    }
}
