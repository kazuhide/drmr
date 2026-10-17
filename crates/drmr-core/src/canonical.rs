//! Deterministic JSON serialization compatible with `CPython` `json.dumps`.
//!
//! Snapshot digests use sorted keys and compact separators. Snapshot files, records,
//! and remodules use their respective spacing and indentation settings. Non-ASCII
//! characters are escaped, including surrogate pairs for code points above U+FFFF.
//! Floating-point values retain their shortest round-trip decimal representation.

use serde_json::{Number, Value};
use std::fmt::Write as _;

/// How `json.dumps` was called on the Python side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dumps {
    /// `sort_keys=True`.
    pub sort_keys: bool,
    /// `indent=N` (Python then puts each item on its own line).
    pub indent: Option<usize>,
    /// The first element of `separators`: between items.
    pub item_separator: &'static str,
    /// The second element of `separators`: between a key and its value.
    pub key_separator: &'static str,
}

impl Dumps {
    /// `json.dumps(value)`: insertion order, `", "` and `": "` (a log line).
    pub const DEFAULT: Dumps = Dumps {
        sort_keys: false,
        indent: None,
        item_separator: ", ",
        key_separator: ": ",
    };
    /// `json.dumps(value, sort_keys=True)`: the snapshot file as written.
    pub const SORTED: Dumps = Dumps {
        sort_keys: true,
        ..Dumps::DEFAULT
    };
    /// `json.dumps(value, sort_keys=True, separators=(",", ":"))`: the digest input.
    pub const CANONICAL: Dumps = Dumps {
        sort_keys: true,
        indent: None,
        item_separator: ",",
        key_separator: ":",
    };
    /// `json.dumps(value, indent=2)`: a report (Python switches the item separator to `","`
    /// when it indents).
    pub const INDENT2: Dumps = Dumps {
        sort_keys: false,
        indent: Some(2),
        item_separator: ",",
        key_separator: ": ",
    };

    /// The text `json.dumps` returns for `value` under these options.
    pub fn dumps(&self, value: &Value) -> String {
        let mut out = String::new();
        self.write(&mut out, value, 0);
        out
    }

    fn write(&self, out: &mut String, value: &Value, level: usize) {
        match value {
            Value::Null => out.push_str("null"),
            Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
            Value::Number(number) => write_number(out, number),
            Value::String(text) => write_string(out, text),
            Value::Array(items) => {
                if items.is_empty() {
                    out.push_str("[]");
                    return;
                }
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(self.item_separator);
                    }
                    self.newline(out, level + 1);
                    self.write(out, item, level + 1);
                }
                self.newline(out, level);
                out.push(']');
            }
            Value::Object(map) => {
                if map.is_empty() {
                    out.push_str("{}");
                    return;
                }
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                if self.sort_keys {
                    // Python's `sorted(dict.items)` compares the keys as `str`, i.e. by code
                    // point; `String`'s order is the same order over valid UTF-8.
                    entries.sort_by(|left, right| left.0.cmp(right.0));
                }
                out.push('{');
                for (index, (key, item)) in entries.iter().enumerate() {
                    if index > 0 {
                        out.push_str(self.item_separator);
                    }
                    self.newline(out, level + 1);
                    write_string(out, key);
                    out.push_str(self.key_separator);
                    self.write(out, item, level + 1);
                }
                self.newline(out, level);
                out.push('}');
            }
        }
    }

    fn newline(&self, out: &mut String, level: usize) {
        if let Some(indent) = self.indent {
            out.push('\n');
            out.extend(std::iter::repeat_n(' ', indent * level));
        }
    }
}

fn write_number(out: &mut String, number: &Number) {
    if let Some(integer) = number.as_i64() {
        write!(out, "{integer}").expect("String never fails to write");
    } else if let Some(integer) = number.as_u64() {
        write!(out, "{integer}").expect("String never fails to write");
    } else if let Some(float) = number.as_f64() {
        out.push_str(&python_float_repr(float));
    }
}

/// `str(x)` / `repr(x)` of a Python float: the shortest digits that round-trip, fixed notation
/// while the decimal exponent is in `-4..16`, otherwise `d.ddde±XX`; an integral value keeps a
/// trailing `.0`.
pub fn python_float_repr(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() {
            "-0.0"
        } else {
            "0.0"
        }
        .to_string();
    }
    debug_assert!(value.is_finite(), "JSON carries finite numbers only");
    // The shortest round-trip digits from `zmij`: on an exact tie between two shortest digit
    // strings it rounds half to even, as CPython's `repr` does; Rust's `{:e}` rounds such ties
    // up (2**-25: `…313` against Python's `…312`), which the tie fixture pins.
    let mut buffer = zmij::Buffer::new();
    let shortest = buffer.format_finite(value.abs());
    let (mantissa, exponent) = shortest.split_once('e').unwrap_or((shortest, "0"));
    let exponent: i32 = exponent.parse().expect("zmij writes a decimal exponent");
    let (integral, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits = format!("{integral}{fraction}");
    // Position of the decimal point relative to the digit string (`0.digits * 10^decpt`),
    // the quantity CPython's `format_float_short` decides the notation on.
    let mut decpt = i32::try_from(integral.len()).expect("short") + exponent;
    let leading = digits.len() - digits.trim_start_matches('0').len();
    digits.drain(..leading);
    decpt -= i32::try_from(leading).expect("short");
    let kept = digits.trim_end_matches('0').len();
    digits.truncate(kept);
    let count = i32::try_from(digits.len()).expect("fewer than 2^31 digits");
    let mut out = String::with_capacity(digits.len() + 8);
    if value.is_sign_negative() {
        out.push('-');
    }
    if decpt <= -4 || decpt > 16 {
        out.push_str(&digits[..1]);
        if count > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let exp = decpt - 1;
        out.push('e');
        out.push(if exp < 0 { '-' } else { '+' });
        write!(out, "{:02}", exp.abs()).expect("String never fails to write");
    } else if decpt <= 0 {
        out.push_str("0.");
        out.extend(std::iter::repeat_n(
            '0',
            usize::try_from(-decpt).expect("non-negative"),
        ));
        out.push_str(&digits);
    } else if decpt < count {
        let split = usize::try_from(decpt).expect("positive");
        out.push_str(&digits[..split]);
        out.push('.');
        out.push_str(&digits[split..]);
    } else {
        out.push_str(&digits);
        out.extend(std::iter::repeat_n(
            '0',
            usize::try_from(decpt - count).expect("non-negative"),
        ));
        out.push_str(".0");
    }
    out
}

/// A JSON string as `json.dumps` writes it with `ensure_ascii=True`.
pub fn write_string(out: &mut String, text: &str) {
    out.push('"');
    let mut units = [0u16; 2];
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(character),
            _ => {
                for unit in character.encode_utf16(&mut units) {
                    write!(out, "\\u{unit:04x}").expect("String never fails to write");
                }
            }
        }
    }
    out.push('"');
}

/// `json.dumps(value)` with these options, as a convenience.
pub fn dumps(value: &Value, options: Dumps) -> String {
    options.dumps(value)
}
