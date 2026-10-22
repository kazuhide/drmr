//! Activity collection, timestamps, and duration formatting.
use drmr_core::events::{Collector, Record, Sink, format_utc, now_iso, round3};
use serde_json::json;

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

#[test]
fn activity_collection_retains_independent_record_snapshots() {
    let collector = Collector::default();
    let mut first = Record::new();
    first.insert("stage".into(), json!("tool-start"));
    collector.emit(&first);
    first.insert("stage".into(), json!("changed"));
    collector.emit(&first);
    let records = collector.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["stage"], "tool-start");
    assert_eq!(records[1]["stage"], "changed");
}
