//! Record shapes, timestamps and the JSON-Lines writer.

use drmr_core::checks::CheckId;
use drmr_core::events::{
    Collector, JsonLinesWriter, Record, Sink, format_utc, now_iso, pipeline_record, round3,
};
use drmr_core::toolrun::Secrets;
use serde_json::{Value, json};
use std::sync::Arc;

#[test]
fn timestamps_format_like_python_isoformat() {
    for (seconds, micros, expected) in [
        (0, 0, "1970-01-01T00:00:00+00:00"),
        (1_792_195_200, 0, "2026-10-17T00:00:00+00:00"),
        (1_792_195_200, 500_000, "2026-10-17T00:00:00.500000+00:00"),
        (1_792_195_200, 7, "2026-10-17T00:00:00.000007+00:00"),
        (951_782_400, 123_456, "2000-02-29T00:00:00.123456+00:00"),
        (4_102_444_800, 0, "2100-01-01T00:00:00+00:00"),
        (-1, 999_999, "1969-12-31T23:59:59.999999+00:00"),
    ] {
        assert_eq!(format_utc(seconds, micros), expected);
    }
    let now = now_iso();
    let shape =
        regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{6})?\+00:00$").unwrap();
    assert!(shape.is_match(&now), "{now}");
}

#[test]
fn pipeline_records_keep_the_python_key_order() {
    let record = pipeline_record(
        "manifest",
        "info",
        "6-4.framemd5 PASS",
        Some("video_1"),
        Some(CheckId::Framemd5),
        Some(1.234_56),
    );
    let keys: Vec<&str> = record.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        [
            "ts",
            "stage",
            "level",
            "message",
            "rendition_id",
            "check_id",
            "duration_s"
        ]
    );
    assert_eq!(record["duration_s"], json!(1.235));
    assert_eq!(record["check_id"], json!("6-4.framemd5"));
    let minimal = pipeline_record("report", "info", "written", None, None, None);
    assert_eq!(
        minimal.keys().map(String::as_str).collect::<Vec<_>>(),
        ["ts", "stage", "level", "message"]
    );
}

#[test]
fn the_writer_emits_one_scrubbed_ascii_line_per_record() {
    let secrets = Arc::new(Secrets::with(["0f0e0d0c0b0a09080706050403020100"]));
    let writer = JsonLinesWriter::new(Vec::new(), secrets);
    writer.emit(&pipeline_record(
        "report",
        "info",
        "iv 0F0E0D0C0B0A09080706050403020100 日本語",
        None,
        None,
        None,
    ));
    writer.emit(&pipeline_record(
        "report", "info", "second", None, None, None,
    ));
    let text = String::from_utf8(writer.into_inner()).unwrap();
    let lines: Vec<&str> = text.split_terminator('\n').collect();
    assert_eq!(lines.len(), 2);
    assert!(
        lines[0].contains(r#""message": "iv ***REDACTED*** \u65e5\u672c\u8a9e""#),
        "{}",
        lines[0]
    );
    assert!(!text.to_lowercase().contains("0f0e0d0c"));
    assert!(text.is_ascii());
    let parsed: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(parsed["stage"], json!("report"));

    let collector = Collector::default();
    collector.emit(&pipeline_record("raw", "info", "a", None, None, None));
    collector.emit(&pipeline_record("raw", "info", "b", None, None, None));
    assert_eq!(collector.records().len(), 2);
}

#[test]
fn round3_matches_pythons_round() {
    // `round(x, 3)` on CPython 3.13 for each of these; the product-and-round form gave 1.235
    // for 1.2345 (the double is 1.23449999…) where Python gives 1.234.
    for (value, expected) in [
        (0.0005, 0.001),
        (1.0005, 1.0),
        (2.0005, 2.001),
        (0.00025, 0.0),
        (1.2345, 1.234),
        (2.675, 2.675),
        (0.1235, 0.123),
        (0.0015, 0.002),
        (123.4565, 123.457),
        (7.0, 7.0),
        (0.9995, 1.0),
        (-0.0005, -0.001),
        // exact binary ties (k/16): half to even, as Python
        (0.0625, 0.062),
        (0.1875, 0.188),
        (0.3125, 0.312),
        (0.4375, 0.438),
        (0.5625, 0.562),
        (2.0625, 2.062),
    ] {
        assert_eq!(
            round3(value).to_bits(),
            f64::to_bits(expected),
            "{value} -> {} (expected {expected})",
            round3(value)
        );
    }
}

/// A log stream whose reader went away.
struct ClosedPipe;

impl std::io::Write for ClosedPipe {
    fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
        Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_closed_log_stream_is_remembered_not_ignored() {
    let secrets = Arc::new(Secrets::with(["0f0e0d0c0b0a09080706050403020100"]));
    let writer = JsonLinesWriter::new(ClosedPipe, secrets);
    assert!(!writer.failed());
    writer.emit(&Record::new());
    assert!(
        writer.failed(),
        "the first failed write is kept for the exit code"
    );
    writer.emit(&Record::new());
    assert!(writer.failed());
}
