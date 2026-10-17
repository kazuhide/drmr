//! `CPython`-compatible numeric, text, and path conversions for media metadata.
//!
//! JSON values from ffprobe and mp4dump use exact integer/rational conversion,
//! compatible truth and equality rules, and stable path and text formatting.
//! The reference version is `CPython` 3.11. Integer results are bounded to i128,
//! numeric text accepts ASCII digits only, and non-ASCII repr text remains literal.

use super::MediaError;
use crate::canonical::python_float_repr;
use num_bigint::{BigInt, BigUint, Sign};
use num_integer::Integer as _;
use num_rational::BigRational;
use num_traits::{FromPrimitive as _, One as _, ToPrimitive as _, Zero as _};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// `str.isspace`: Unicode whitespace plus U+001C–U+001F, which Python counts and Rust's
/// `char::is_whitespace` does not.
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// The whitespace `int`, `float` and `bytes.fromhex` skip: ASCII `Py_ISSPACE` (space, `\t`,
/// `\n`, `\v`, `\f`, `\r`) and, for `int` / `float`, non-ASCII Unicode whitespace — but not
/// U+001C–U+001F, which `str.isspace` counts (`CPython` keeps ASCII as is before stripping).
fn is_number_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
        || (!c.is_ascii() && c.is_whitespace())
}

/// `str.splitlines` (no `keepends`): `\n`, `\r`, `\r\n`, `\v`, `\f`, U+001C–U+001E, U+0085,
/// U+2028 and U+2029 end a line; a final line break adds no empty line.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let breaks = matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !breaks {
            continue;
        }
        lines.push(&text[start..index]);
        let mut end = index + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
            chars.next();
            end += 1;
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Python's truth value of a JSON value (`None`, `False`, `0`, `0.0`, `""`, `[]`, `{}` are false).
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|float| float != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// A JSON number or boolean as Python compares it: an integer (`bool` is an `int`) or a float.
#[derive(Clone, Copy)]
enum Numeric {
    Int(i128),
    Float(f64),
}

fn numeric(value: &Value) -> Option<Numeric> {
    match value {
        Value::Bool(flag) => Some(Numeric::Int(i128::from(*flag))),
        Value::Number(number) => Some(if let Some(integer) = number.as_i64() {
            Numeric::Int(i128::from(integer))
        } else if let Some(integer) = number.as_u64() {
            Numeric::Int(i128::from(integer))
        } else {
            Numeric::Float(number.as_f64().unwrap_or(f64::NAN))
        }),
        _ => None,
    }
}

/// Exact comparison of an integer and a float, as Python compares them (`2**53 + 1 != 2.0**53`).
fn int_eq_float(integer: i128, float: f64) -> bool {
    float.is_finite()
        && float.fract() == 0.0
        && BigInt::from_f64(float).is_some_and(|whole| whole == BigInt::from(integer))
}

fn numeric_eq(left: Numeric, right: Numeric) -> bool {
    match (left, right) {
        (Numeric::Int(a), Numeric::Int(b)) => a == b,
        #[allow(clippy::float_cmp)] // Python's `==` between floats is exact equality.
        (Numeric::Float(a), Numeric::Float(b)) => a == b,
        (Numeric::Int(a), Numeric::Float(b)) | (Numeric::Float(b), Numeric::Int(a)) => {
            int_eq_float(a, b)
        }
    }
}

/// `value == integer` in Python (`True == 1`, `0.0 == 0`, `"0" != 0`).
pub fn eq_int(value: &Value, integer: i64) -> bool {
    numeric(value).is_some_and(|number| numeric_eq(number, Numeric::Int(i128::from(integer))))
}

/// `left == right` in Python for JSON-derived values: numbers by value across `bool`, `int` and
/// `float`; dicts as unordered mappings; lists element by element.
pub fn py_eq(left: &Value, right: &Value) -> bool {
    if let (Some(a), Some(b)) = (numeric(left), numeric(right)) {
        return numeric_eq(a, b);
    }
    match (left, right) {
        (Value::Null, Value::Null) => true,
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| py_eq(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, x)| b.get(key).is_some_and(|y| py_eq(x, y)))
        }
        _ => false,
    }
}

/// `repr(text)` of a `str`: single quotes unless the text holds a `'` and no `"`; ASCII control
/// characters escaped. Non-ASCII characters are kept as written (Python escapes the
/// non-printable ones; the texts here are tool tokens).
pub fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ if c == quote => {
                out.push('\\');
                out.push(c);
            }
            _ if c.is_ascii_control() || ('\u{80}'..='\u{a0}').contains(&c) => {
                write!(out, "\\x{:02x}", u32::from(c)).expect("String never fails to write");
            }
            _ => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `str(value)` of a JSON value as an f-string interpolates it (`5`, `5.0`, `True`, `None`,
/// the text itself, `[1, 'a']`).
pub fn py_str(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        _ => py_repr(value),
    }
}

fn py_repr(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(flag) => if *flag { "True" } else { "False" }.to_string(),
        Value::Number(number) => match numeric(value) {
            Some(Numeric::Int(integer)) => integer.to_string(),
            _ => python_float_repr(number.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(text) => repr_str(text),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(py_repr).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(key, item)| format!("{}: {}", repr_str(key), py_repr(item)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

/// Python's type name of a JSON value, as error messages print it.
pub fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(number) => {
            if number.is_f64() {
                "float"
            } else {
                "int"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn type_error(message: String) -> MediaError {
    MediaError::uncaught("TypeError", message)
}

fn overflow(message: &str) -> MediaError {
    MediaError::uncaught("OverflowError", message.to_string())
}

/// Digits with single underscores between them (PEP 515), as `int` / `float` /
/// `Fraction` accept them; `None` when the grouping is invalid or a non-digit appears.
fn digits_without_underscores(text: &str) -> Option<String> {
    if text.is_empty() || text.starts_with('_') || text.ends_with('_') || text.contains("__") {
        return None;
    }
    let digits: String = text.chars().filter(|c| *c != '_').collect();
    digits
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then_some(digits)
}

/// `int(text)` for a `str` (base 10): surrounding whitespace, a sign, digits with underscores.
fn int_literal(text: &str) -> Option<BigInt> {
    let body = text.trim_matches(is_number_space);
    let (negative, digits) = match body.as_bytes().first() {
        Some(b'-') => (true, &body[1..]),
        Some(b'+') => (false, &body[1..]),
        _ => (false, body),
    };
    let digits = digits_without_underscores(digits)?;
    let magnitude: BigInt = digits.parse().ok()?;
    Some(if negative { -magnitude } else { magnitude })
}

fn int_from_float(float: f64) -> Result<BigInt, MediaError> {
    if float.is_nan() {
        return Err(MediaError::Value(
            "cannot convert float NaN to integer".to_string(),
        ));
    }
    if float.is_infinite() {
        return Err(overflow("cannot convert float infinity to integer"));
    }
    Ok(BigInt::from_f64(float.trunc()).expect("a finite float has an integer part"))
}

/// `int(value)` of a JSON value: a number truncated toward zero, a `bool` as 0 / 1, a `str`
/// parsed in base 10; `None`, lists and dicts are a `TypeError`. Results beyond `i128` (Python
/// keeps the big integer) are an `OverflowError` — no tool prints one.
pub fn int(value: &Value) -> Result<i128, MediaError> {
    let big = match value {
        Value::Bool(flag) => BigInt::from(u8::from(*flag)),
        Value::Number(number) => match numeric(value) {
            Some(Numeric::Int(integer)) => BigInt::from(integer),
            _ => int_from_float(number.as_f64().unwrap_or(f64::NAN))?,
        },
        Value::String(text) => int_literal(text).ok_or_else(|| {
            MediaError::Value(format!(
                "invalid literal for int() with base 10: {}",
                repr_str(text)
            ))
        })?,
        _ => {
            return Err(type_error(format!(
                "int() argument must be a string, a bytes-like object or a real number, not '{}'",
                type_name(value)
            )));
        }
    };
    big.to_i128()
        .ok_or_else(|| overflow("int too large for this port (beyond i128)"))
}

/// [`int`], then into `i64` (ffprobe's packet fields are `int64_t`).
pub fn int64(value: &Value) -> Result<i64, MediaError> {
    i64::try_from(int(value)?).map_err(|_| overflow("int too large for this port (beyond i64)"))
}

/// `float(text)` for a `str`: surrounding whitespace, `inf` / `infinity` / `nan` in any case
/// with a sign, decimal literals with underscores between digits.
fn float_literal(text: &str) -> Option<f64> {
    let body = text.trim_matches(is_number_space);
    let unsigned = body.trim_start_matches(['+', '-']);
    if body.len() - unsigned.len() > 1 {
        return None;
    }
    let lower = unsigned.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "infinity" | "nan") {
        return body.parse().ok().or_else(|| {
            let magnitude = if lower == "nan" {
                f64::NAN
            } else {
                f64::INFINITY
            };
            Some(if body.starts_with('-') {
                -magnitude
            } else {
                magnitude
            })
        });
    }
    // mantissa [e exponent]; each run of digits may carry single underscores between digits.
    let (mantissa, exponent) = match lower.split_once('e') {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (lower.as_str(), None),
    };
    let (whole, fraction) = match mantissa.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (mantissa, None),
    };
    let whole_digits = if whole.is_empty() {
        String::new()
    } else {
        digits_without_underscores(whole)?
    };
    let fraction_digits = match fraction {
        Some(part) if !part.is_empty() => digits_without_underscores(part)?,
        _ => String::new(),
    };
    if whole_digits.is_empty() && fraction_digits.is_empty() {
        return None;
    }
    let mut cleaned = String::with_capacity(body.len());
    if body.starts_with('-') {
        cleaned.push('-');
    }
    cleaned.push_str(&whole_digits);
    if fraction.is_some() {
        cleaned.push('.');
        cleaned.push_str(&fraction_digits);
    }
    if let Some(exponent) = exponent {
        let digits = exponent.trim_start_matches(['+', '-']);
        if exponent.len() - digits.len() > 1 {
            return None;
        }
        cleaned.push('e');
        if exponent.starts_with('-') {
            cleaned.push('-');
        }
        cleaned.push_str(&digits_without_underscores(digits)?);
    }
    cleaned.parse().ok()
}

/// `float(value)` of a JSON value (`int` → nearest float, `bool` → 0.0 / 1.0, `str` parsed);
/// `None`, lists and dicts are a `TypeError`.
pub fn float(value: &Value) -> Result<f64, MediaError> {
    match value {
        Value::Bool(flag) => Ok(f64::from(u8::from(*flag))),
        Value::Number(number) => Ok(number.as_f64().unwrap_or(f64::NAN)),
        Value::String(text) => float_literal(text).ok_or_else(|| {
            MediaError::Value(format!(
                "could not convert string to float: {}",
                repr_str(text)
            ))
        }),
        _ => Err(type_error(format!(
            "float() argument must be a string or a real number, not '{}'",
            type_name(value)
        ))),
    }
}

/// `CPython` 3.11's `_RATIONAL_FORMAT` (no whitespace around `/`; `\s` is `str.isspace`):
/// `[sign] num [/ denom | [. decimal] [e exp]]`, the numerator possibly empty before a decimal.
fn fraction_literal(text: &str) -> Option<Result<BigRational, MediaError>> {
    let body = text.trim_matches(is_space);
    let (negative, rest) = match body.as_bytes().first() {
        Some(b'-') => (true, &body[1..]),
        Some(b'+') => (false, &body[1..]),
        _ => (false, body),
    };
    // The lookahead `(?=\d|\.\d)`.
    let bytes = rest.as_bytes();
    let starts = bytes.first().is_some_and(u8::is_ascii_digit)
        || (bytes.first() == Some(&b'.') && bytes.get(1).is_some_and(u8::is_ascii_digit));
    if !starts {
        return None;
    }
    let num_end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '_'))
        .unwrap_or(rest.len());
    let (num_text, tail) = rest.split_at(num_end);
    let mut numerator = if num_text.is_empty() {
        BigInt::zero()
    } else {
        digits_without_underscores(num_text)?
            .parse::<BigInt>()
            .ok()?
    };
    let mut denominator = BigInt::one();
    if let Some(denom) = tail.strip_prefix('/') {
        denominator = digits_without_underscores(denom)?.parse().ok()?;
    } else {
        let lower = tail.to_ascii_lowercase();
        let (decimal, exponent) = match lower.split_once('e') {
            Some((decimal, exponent)) => (decimal, Some(exponent)),
            None => (lower.as_str(), None),
        };
        if let Some(digits) = decimal.strip_prefix('.') {
            if !digits.is_empty() {
                let digits = digits_without_underscores(digits)?;
                let scale = BigInt::from(10u8).pow(u32::try_from(digits.len()).ok()?);
                numerator = numerator * &scale + digits.parse::<BigInt>().ok()?;
                denominator *= scale;
            }
        } else if !decimal.is_empty() {
            return None;
        }
        if let Some(exponent) = exponent {
            let (exp_negative, digits) = match exponent.as_bytes().first() {
                Some(b'-') => (true, &exponent[1..]),
                Some(b'+') => (false, &exponent[1..]),
                _ => (false, exponent),
            };
            let power: u32 = digits_without_underscores(digits)?.parse().ok()?;
            let scale = BigInt::from(10u8).pow(power);
            if exp_negative {
                denominator *= scale;
            } else {
                numerator *= scale;
            }
        }
    }
    if negative {
        numerator = -numerator;
    }
    if denominator.is_zero() {
        return Some(Err(MediaError::uncaught(
            "ZeroDivisionError",
            format!("Fraction({numerator}, 0)"),
        )));
    }
    Some(Ok(BigRational::new(numerator, denominator)))
}

/// `Fraction(value)` (`CPython` 3.11) of a JSON value: an `int` or `bool` exactly, a `float`
/// exactly (its binary value), a `str` by `_RATIONAL_FORMAT`; `None`, lists and dicts are a
/// `TypeError`; a zero denominator is a `ZeroDivisionError`.
pub fn fraction(value: &Value) -> Result<BigRational, MediaError> {
    match value {
        Value::Bool(flag) => Ok(BigRational::from_integer(BigInt::from(u8::from(*flag)))),
        Value::Number(number) => match numeric(value) {
            Some(Numeric::Int(integer)) => Ok(BigRational::from_integer(BigInt::from(integer))),
            _ => BigRational::from_float(number.as_f64().unwrap_or(f64::NAN)).ok_or_else(|| {
                MediaError::Value("cannot convert NaN to integer ratio".to_string())
            }),
        },
        Value::String(text) => fraction_literal(text).unwrap_or_else(|| {
            Err(MediaError::Value(format!(
                "Invalid literal for Fraction: {}",
                repr_str(text)
            )))
        }),
        _ => Err(type_error(
            "argument should be a string or a Rational instance".to_string(),
        )),
    }
}

/// `Fraction(numerator, denominator)`: a `ZeroDivisionError` for a zero denominator.
pub fn ratio(
    numerator: impl Into<BigInt>,
    denominator: impl Into<BigInt>,
) -> Result<BigRational, MediaError> {
    let (numerator, denominator) = (numerator.into(), denominator.into());
    if denominator.is_zero() {
        return Err(MediaError::uncaught(
            "ZeroDivisionError",
            format!("Fraction({numerator}, 0)"),
        ));
    }
    Ok(BigRational::new(numerator, denominator))
}

/// A `u64` as the nearest `f64` (round half to even): exact below 2**53.
pub fn exact_f64(value: u64) -> f64 {
    let high = u32::try_from(value >> 32).expect("the high half fits");
    let low = u32::try_from(value & 0xffff_ffff).expect("the low half fits");
    // high * 2**32 is exact; the one addition rounds once, to nearest even.
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

/// `2**exponent` for a normal exponent (-1022..=1023).
fn pow2(exponent: i64) -> f64 {
    f64::from_bits(u64::try_from(exponent + 1023).expect("a normal exponent") << 52)
}

/// `ldexp(value, exponent)` where the exact result is representable (or underflows to it).
fn ldexp(mut value: f64, mut exponent: i64) -> f64 {
    while exponent > 1000 {
        value *= pow2(1000);
        exponent -= 1000;
    }
    while exponent < -1000 {
        value *= pow2(-1000);
        exponent += 1000;
    }
    value * pow2(exponent)
}

const DBL_MANT_DIG: i64 = 53;
const DBL_MAX_EXP: i64 = 1024;
const DBL_MIN_EXP: i64 = -1021;

/// `a / b` for Python integers: the correctly rounded quotient (`CPython`'s `long_true_divide`,
/// round half to even, subnormals included); a zero divisor is a `ZeroDivisionError`, a quotient
/// beyond the float range an `OverflowError`.
pub fn true_divide(a: &BigInt, b: &BigInt) -> Result<f64, MediaError> {
    if b.is_zero() {
        return Err(MediaError::uncaught(
            "ZeroDivisionError",
            "division by zero".to_string(),
        ));
    }
    let negate = (a.sign() == Sign::Minus) != (b.sign() == Sign::Minus);
    let signed = |value: f64| if negate { -value } else { value };
    let (a, b) = (a.magnitude(), b.magnitude());
    if a.is_zero() {
        return Ok(signed(0.0));
    }
    if a.bits() <= 53 && b.bits() <= 53 {
        let numerator = exact_f64(a.to_u64().expect("53 bits"));
        let denominator = exact_f64(b.to_u64().expect("53 bits"));
        return Ok(signed(numerator / denominator));
    }
    let bits = |value: &BigUint| i64::try_from(value.bits()).expect("bit counts fit i64");
    let diff = bits(a) - bits(b);
    if diff > DBL_MAX_EXP {
        return Err(overflow("integer division result too large for a float"));
    }
    if diff < DBL_MIN_EXP - DBL_MANT_DIG - 1 {
        return Ok(signed(0.0));
    }
    let shift = diff.max(DBL_MIN_EXP) - DBL_MANT_DIG - 2;
    let mut inexact = false;
    let shifted = if shift <= 0 {
        a << usize::try_from(-shift).expect("non-negative")
    } else {
        let places = usize::try_from(shift).expect("positive");
        let dropped = a - ((a >> places) << places);
        inexact = !dropped.is_zero();
        a >> places
    };
    let (quotient, remainder) = shifted.div_rem(b);
    inexact |= !remainder.is_zero();
    let quotient_bits = bits(&quotient);
    // The bits to round away: 2 or 3 (`CPython` asserts it).
    let extra_bits = quotient_bits.max(DBL_MIN_EXP - shift) - DBL_MANT_DIG;
    let mask = 1u64 << (extra_bits - 1);
    let low = (&quotient & BigUint::from(15u8))
        .to_u64()
        .expect("four bits")
        | u64::from(inexact);
    let cleared = &quotient - (&quotient & BigUint::from(2 * mask - 1));
    let rounded = if low & mask != 0 && low & (3 * mask - 1) != 0 {
        cleared + BigUint::from(2 * mask)
    } else {
        cleared
    };
    let value = exact_f64(rounded.to_u64().expect("at most 57 bits"));
    if shift + quotient_bits >= DBL_MAX_EXP
        && (shift + quotient_bits > DBL_MAX_EXP
            || value.to_bits() == ldexp(1.0, quotient_bits).to_bits())
    {
        return Err(overflow("integer division result too large for a float"));
    }
    Ok(signed(ldexp(value, shift)))
}

/// `float(fraction)`: `numerator / denominator` correctly rounded (`Fraction.__float__`).
pub fn fraction_to_f64(value: &BigRational) -> Result<f64, MediaError> {
    true_divide(value.numer(), value.denom())
}

/// `bytes.fromhex(text)` (`CPython` 3.11): pairs of hex digits, ASCII whitespace skipped between
/// pairs; the error names the position only, never the text (a key passes through here).
pub fn fromhex(text: &str) -> Result<zeroize::Zeroizing<Vec<u8>>, MediaError> {
    let invalid = |position: usize| {
        MediaError::Value(format!(
            "non-hexadecimal number found in fromhex() arg at position {position}"
        ))
    };
    let chars: Vec<char> = text.chars().collect();
    let mut out = zeroize::Zeroizing::new(Vec::with_capacity(chars.len() / 2));
    let mut index = 0;
    while index < chars.len() {
        if matches!(chars[index], ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r') {
            index += 1;
            continue;
        }
        let top = chars[index].to_digit(16).ok_or_else(|| invalid(index))?;
        let bottom = chars
            .get(index + 1)
            .and_then(|c| c.to_digit(16))
            .ok_or_else(|| invalid(index + 1))?;
        out.push(u8::try_from(top * 16 + bottom).expect("two hex digits"));
        index += 2;
    }
    Ok(out)
}

/// The text of `pathlib.PurePosixPath(path)`: repeated separators and `.` components dropped, a
/// trailing separator removed, two leading separators kept (POSIX), `.` for an empty path.
pub fn path_str(path: &Path) -> Result<String, MediaError> {
    let text = path.to_str().ok_or_else(|| {
        MediaError::Os(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not valid UTF-8 (the tool runner takes text arguments)",
        ))
    })?;
    let (root, parts) = path_parts(text);
    let joined = format!("{root}{}", parts.join("/"));
    Ok(if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    })
}

fn path_parts(text: &str) -> (&'static str, Vec<&str>) {
    let root = if text.starts_with("//") && !text.starts_with("///") {
        "//"
    } else if text.starts_with('/') {
        "/"
    } else {
        ""
    };
    let parts = text
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    (root, parts)
}

/// `pathlib.PurePath.name`: the last component, `""` for a root or `.`.
pub fn path_name(path: &Path) -> Result<String, MediaError> {
    let text = path_str(path)?;
    let (_, parts) = path_parts(&text);
    Ok(parts
        .last()
        .map_or_else(String::new, |part| (*part).to_string()))
}

/// `posixpath.split`.
fn split(path: &str) -> (String, String) {
    let index = path.rfind('/').map_or(0, |position| position + 1);
    let (head, tail) = path.split_at(index);
    let head = if !head.is_empty() && head.chars().any(|c| c != '/') {
        head.trim_end_matches('/')
    } else {
        head
    };
    (head.to_string(), tail.to_string())
}

/// `posixpath.join(base, name)`.
fn join(base: &str, name: &str) -> String {
    if name.starts_with('/') {
        name.to_string()
    } else if base.is_empty() || base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// `posixpath.normpath`.
fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let initial = if path.starts_with("//") && !path.starts_with("///") {
        2
    } else {
        usize::from(path.starts_with('/'))
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part != ".." || (initial == 0 && parts.is_empty()) || parts.last() == Some(&"..") {
            parts.push(part);
        } else if !parts.is_empty() {
            parts.pop();
        }
    }
    let joined = format!("{}{}", "/".repeat(initial), parts.join("/"));
    if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

/// `posixpath._joinrealpath` (`CPython` 3.11, non-strict): every existing symlink resolved, the
/// rest of the path taken as written; `false` when a symlink loop stopped the resolution.
fn join_real_path(
    mut path: String,
    rest: &str,
    seen: &mut HashMap<String, Option<String>>,
) -> (String, bool) {
    let mut rest = rest.to_string();
    if rest.starts_with('/') {
        rest.remove(0);
        path = "/".to_string();
    }
    while !rest.is_empty() {
        let (name, remainder) = match rest.split_once('/') {
            Some((name, remainder)) => (name.to_string(), remainder.to_string()),
            None => (std::mem::take(&mut rest), String::new()),
        };
        rest = remainder;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            if path.is_empty() {
                path = "..".to_string();
            } else {
                let (head, tail) = split(&path);
                path = if tail == ".." {
                    join(&join(&head, ".."), "..")
                } else {
                    head
                };
            }
            continue;
        }
        let candidate = join(&path, &name);
        let is_link = std::fs::symlink_metadata(&candidate)
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        if !is_link {
            path = candidate;
            continue;
        }
        if let Some(cached) = seen.get(&candidate) {
            if let Some(resolved) = cached {
                path.clone_from(resolved);
                continue;
            }
            return (join(&candidate, &rest), false);
        }
        seen.insert(candidate.clone(), None);
        let target = std::fs::read_link(&candidate)
            .ok()
            .and_then(|link| link.to_str().map(str::to_string))
            .unwrap_or_default();
        let (resolved, ok) = join_real_path(path, &target, seen);
        if !ok {
            return (join(&resolved, &rest), false);
        }
        seen.insert(candidate, Some(resolved.clone()));
        path = resolved;
    }
    (path, true)
}

/// `pathlib.Path.resolve` (`CPython` 3.11, non-strict): absolute, every existing symlink
/// resolved, `..` applied; a symlink loop is the `RuntimeError` Python raises.
pub fn resolve(path: &Path) -> Result<PathBuf, MediaError> {
    let text = path_str(path)?;
    let (real, _) = join_real_path(String::new(), &text, &mut HashMap::new());
    let absolute = if real.starts_with('/') {
        real
    } else {
        let cwd = std::env::current_dir().map_err(MediaError::Os)?;
        join(&path_str(&cwd)?, &real)
    };
    let resolved = normpath(&absolute);
    if let Err(error) = std::fs::metadata(&resolved)
        && error.raw_os_error() == Some(ELOOP)
    {
        return Err(MediaError::Runtime(format!(
            "Symlink loop from {}",
            repr_str(&resolved)
        )));
    }
    Ok(PathBuf::from(resolved))
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const ELOOP: i32 = 62;
#[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd")))]
const ELOOP: i32 = 40;
