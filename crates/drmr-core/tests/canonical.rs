//! The canonical writer against bytes Python's `json.dumps` produced (fixtures generated with
//! the Python interpreter; see `tests/fixtures/`).

use drmr_core::canonical::{Dumps, python_float_repr, write_string};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn canonical_bytes_match_python_json_dumps() {
    let value: Value =
        serde_json::from_slice(&std::fs::read(fixture("canonical.in.json")).unwrap()).unwrap();
    let expected =
        String::from_utf8(std::fs::read(fixture("canonical.expected.json")).unwrap()).unwrap();
    let actual = Dumps::CANONICAL.dumps(&value);
    assert_eq!(actual, expected);
    let digest = std::fs::read_to_string(fixture("canonical.sha256")).unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(actual.as_bytes())),
        digest.trim()
    );
}

#[test]
fn sorted_default_separators_reproduce_the_snapshot_file() {
    let bytes = std::fs::read(fixture("analysis.sample.json")).unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        Dumps::SORTED.dumps(&value),
        String::from_utf8(bytes).unwrap()
    );
}

#[test]
fn floats_print_as_python_repr() {
    for (value, expected) in [
        (6.0, "6.0"),
        (2f64.powi(-25), "2.9802322387695312e-08"),
        (8.0 + 2f64.powi(-16), "8.000015258789062"),
        (1.0 + 2f64.powi(-52), "1.0000000000000002"),
        (596.458_333_333_333_4, "596.4583333333334"),
        (0.0001, "0.0001"),
        (0.00001, "1e-05"),
        (1e16, "1e+16"),
        (1e15, "1000000000000000.0"),
        (9_999_999_999_999_998.0, "9999999999999998.0"),
        (2.5e-7, "2.5e-07"),
        (-0.0, "-0.0"),
        (0.0, "0.0"),
        (123_456_789.125, "123456789.125"),
        (5e-324, "5e-324"),
        (f64::MAX, "1.7976931348623157e+308"),
        (0.1, "0.1"),
        (100.0, "100.0"),
        (-3.5, "-3.5"),
        (1e22, "1e+22"),
        (123_456_789_012_345_680.0, "1.2345678901234568e+17"),
        (0.000_123, "0.000123"),
        (1.5e-5, "1.5e-05"),
        (7e-10, "7e-10"),
    ] {
        assert_eq!(python_float_repr(value), expected, "{value:?}");
    }
}

#[test]
fn strings_escape_like_ensure_ascii() {
    let mut out = String::new();
    write_string(
        &mut out,
        "quote \" backslash \\ slash / tab \t nl \n cr \r bell \u{7} del \u{7f} jp \u{65e5} emoji \u{1F600} ls \u{2028}",
    );
    assert_eq!(
        out,
        r#""quote \" backslash \\ slash / tab \t nl \n cr \r bell \u0007 del \u007f jp \u65e5 emoji \ud83d\ude00 ls \u2028""#
    );
}

#[test]
fn indent_two_and_default_separators_match_python() {
    let value: Value =
        serde_json::from_str(r#"{"b": [1, {}, []], "a": {"x": [1.0, "y"]}}"#).unwrap();
    assert_eq!(
        Dumps::INDENT2.dumps(&value),
        "{\n  \"b\": [\n    1,\n    {},\n    []\n  ],\n  \"a\": {\n    \"x\": [\n      1.0,\n      \"y\"\n    ]\n  }\n}"
    );
    let value: Value = serde_json::from_str(r#"{"b": [1, {}], "a": {}}"#).unwrap();
    assert_eq!(Dumps::SORTED.dumps(&value), r#"{"a": {}, "b": [1, {}]}"#);
    assert_eq!(Dumps::DEFAULT.dumps(&value), r#"{"b": [1, {}], "a": {}}"#);
    assert_eq!(Dumps::CANONICAL.dumps(&value), r#"{"a":{},"b":[1,{}]}"#);
}

#[test]
fn floats_parsed_from_json_print_the_digits_python_wrote() {
    // Fractional media durations plus values that need 17 significant digits:
    // the file holds Python's repr, one per line; parsing it as JSON and printing it must give
    // the same text (the digest of a snapshot depends on it).
    let text = std::fs::read_to_string(fixture("floats.python-repr.txt")).unwrap();
    let mut checked = 0;
    for line in text.lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        assert_eq!(Dumps::CANONICAL.dumps(&value), line);
        assert_eq!(python_float_repr(line.parse::<f64>().unwrap()), line);
        checked += 1;
    }
    assert!(checked >= 40, "{checked}");
}

#[test]
fn float_ties_round_half_to_even_like_cpython() {
    // Written by CPython 3.13 (`repr`): negative powers of two, n + 2**-k, neighbours of 1.0 and
    // random doubles — the cases where two shortest digit strings are equally close and Rust's
    // `{:e}` picks the upper one.
    let text = std::fs::read_to_string(fixture("floats.ties.python-repr.txt")).unwrap();
    let mut checked = 0;
    for line in text.lines() {
        let value: Value = serde_json::from_str(line).unwrap();
        assert_eq!(Dumps::CANONICAL.dumps(&value), line);
        assert_eq!(python_float_repr(line.parse::<f64>().unwrap()), line);
        checked += 1;
    }
    assert!(checked >= 500, "{checked}");
}

#[test]
fn backspace_and_formfeed_take_their_short_escapes() {
    // json.dumps("a\bb\fc\x1f") -> "a\\bb\\fc\\u001f": the two-character escapes for
    // U+0008 and U+000C, `\u00XX` for the other controls.
    assert_eq!(
        Dumps::DEFAULT.dumps(&Value::String("a\u{8}b\u{c}c\u{1f}".to_string())),
        r#""a\bb\fc\u001f""#
    );
}
