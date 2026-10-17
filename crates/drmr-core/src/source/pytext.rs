//! The Python `str` behaviour the clear-HLS reader depends on: `str.splitlines`,
//! `str.strip` / `str.isspace`, the `\d` class of `re` (Unicode category Nd, the same set
//! `int` and `Decimal` accept as digits), and `int` / `float` of a string.
//!
//! The tables are those of the Unicode database (14.0, `CPython` 3.11; the
//! same table as `manifest_support::py`); `CPython` 3.13 (Unicode 15.1) adds the Kawi and Nag
//! Mundari digit blocks, which no playlist carries.

/// The first code point (the zero) of every decimal digit block of Unicode category Nd: every
/// Nd character is `zero + value` for one of these, `value` in `0..10`.
const ND_ZEROS: [u32; 66] = [
    0x30, 0x660, 0x6F0, 0x7C0, 0x966, 0x9E6, 0xA66, 0xAE6, 0xB66, 0xBE6, 0xC66, 0xCE6, 0xD66,
    0xDE6, 0xE50, 0xED0, 0xF20, 0x1040, 0x1090, 0x17E0, 0x1810, 0x1946, 0x19D0, 0x1A80, 0x1A90,
    0x1B50, 0x1BB0, 0x1C40, 0x1C50, 0xA620, 0xA8D0, 0xA900, 0xA9D0, 0xA9F0, 0xAA50, 0xABF0, 0xFF10,
    0x104A0, 0x10D30, 0x11066, 0x110F0, 0x11136, 0x111D0, 0x112F0, 0x11450, 0x114D0, 0x11650,
    0x116C0, 0x11730, 0x118E0, 0x11950, 0x11C50, 0x11D50, 0x11DA0, 0x16A60, 0x16AC0, 0x16B50,
    0x1D7CE, 0x1D7D8, 0x1D7E2, 0x1D7EC, 0x1D7F6, 0x1E140, 0x1E2F0, 0x1E950, 0x1FBF0,
];

/// The value of `c` when it is a decimal digit (`re`'s `\d`, `unicodedata.decimal`).
pub fn decimal_digit(c: char) -> Option<u32> {
    let code = u32::from(c);
    let index = ND_ZEROS.partition_point(|zero| *zero <= code);
    let zero = ND_ZEROS[index.checked_sub(1)?];
    let value = code - zero;
    (value < 10).then_some(value)
}

/// `str.isspace`: Unicode `White_Space` plus U+001C–U+001F (bidirectional class B / S).
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `str.strip`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `str.splitlines`: lines split at `\n`, `\r`, `\r\n`, `\v`, `\f`, `\x1c`, `\x1d`, `\x1e`,
/// `\x85`, U+2028 and U+2029, without the line ends; no trailing empty line.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        let boundary = matches!(
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
        if !boundary {
            continue;
        }
        lines.push(&text[start..index]);
        let mut end = index + c.len_utf8();
        if c == '\r' && chars.peek().map(|(_, next)| *next) == Some('\n') {
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

/// `re.fullmatch(r"\d+", text)` (at least one digit, every character a decimal digit).
pub fn all_digits(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| decimal_digit(c).is_some())
}

/// The digits of `text` as ASCII (every character must be a decimal digit).
pub fn ascii_digits(text: &str) -> Option<String> {
    text.chars()
        .map(|c| decimal_digit(c).and_then(|value| char::from_digit(value, 10)))
        .collect()
}

/// `int(text)` for a base-10 string as Python parses it: surrounding whitespace, an optional
/// sign, decimal digits (any Nd block) with single underscores between them. `None` is
/// Python's `ValueError`. The value saturates at `i128`'s range (Python's integers do not; no
/// count or length this tool compares comes near it).
pub fn py_int(text: &str) -> Option<i128> {
    let body = strip(text);
    let (negative, digits) = match body.chars().next()? {
        '-' => (true, &body[1..]),
        '+' => (false, &body[1..]),
        _ => (false, body),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
    {
        return None;
    }
    let mut value: i128 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        let digit = i128::from(decimal_digit(c)?);
        value = value.saturating_mul(10).saturating_add(digit);
    }
    Some(if negative { -value } else { value })
}

/// `float(text)` for a string as Python parses it: surrounding whitespace, an optional sign,
/// `inf` / `infinity` / `nan` in any case, or a decimal literal (any Nd digits, single
/// underscores between digits, an optional exponent). `None` is Python's `ValueError`.
pub fn py_float(text: &str) -> Option<f64> {
    let body = strip(text);
    let (sign, rest) = match body.chars().next()? {
        '-' => (-1.0, &body[1..]),
        '+' => (1.0, &body[1..]),
        _ => (1.0, body),
    };
    let lowered = rest.to_ascii_lowercase();
    if lowered == "inf" || lowered == "infinity" {
        return Some(sign * f64::INFINITY);
    }
    if lowered == "nan" {
        return Some(f64::NAN.copysign(sign));
    }
    let mut ascii = String::with_capacity(rest.len());
    let mut previous_digit = false;
    let chars: Vec<char> = rest.chars().collect();
    for (index, c) in chars.iter().enumerate() {
        if *c == '_' {
            let next_digit = chars
                .get(index + 1)
                .is_some_and(|next| decimal_digit(*next).is_some());
            if !previous_digit || !next_digit {
                return None;
            }
            previous_digit = false;
            continue;
        }
        if let Some(value) = decimal_digit(*c) {
            ascii.push(char::from_digit(value, 10)?);
            previous_digit = true;
        } else if matches!(c, '.' | 'e' | 'E' | '+' | '-') {
            ascii.push(*c);
            previous_digit = false;
        } else {
            return None;
        }
    }
    if !valid_float_literal(&ascii) {
        return None;
    }
    ascii.parse::<f64>().ok().map(|value| sign * value)
}

/// `digits [. digits] [e [sign] digits]` with at least one digit in the mantissa (Rust's
/// `f64::from_str` also takes `inf`/`nan` and signs that Python's grammar places elsewhere).
fn valid_float_literal(text: &str) -> bool {
    let (mantissa, exponent) = match text.find(['e', 'E']) {
        Some(index) => (&text[..index], Some(&text[index + 1..])),
        None => (text, None),
    };
    let (integral, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits_only = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if !digits_only(integral) || !digits_only(fraction) || integral.len() + fraction.len() == 0 {
        return false;
    }
    match exponent {
        None => true,
        Some(exponent) => {
            let exponent = exponent.strip_prefix(['+', '-']).unwrap_or(exponent);
            !exponent.is_empty() && digits_only(exponent)
        }
    }
}

/// Compare two decimal integer strings of ASCII digits by value.
pub fn cmp_decimal(left: &str, right: &str) -> std::cmp::Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}
