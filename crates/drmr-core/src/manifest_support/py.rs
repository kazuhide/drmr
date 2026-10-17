//! Python built-in semantics the manifest modules depend on, as `CPython` 3.11 (reference behavior for
//! ) implements them: the exception classes with their `str(exc)`, `int` /
//! `float` of a `str`, `repr` of a `str`, `str.isspace` / `strip` / `splitlines` /
//! `isdigit`, `str(float)`, `f"{x:.Nf}"`, `int(float)`, `bytes.decode("utf-8")`,
//! `Path.read_text` (universal newlines) and `sorted` over float keys.

use crate::canonical::python_float_repr;
use num_bigint::{BigInt, Sign};
use num_traits::{FromPrimitive, Zero};
use std::cmp::Ordering;
use std::fmt::{self, Write as _};
use std::io;
use std::path::Path;
use std::sync::LazyLock;

/// A Python exception by class, carrying `str(exc)`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PyErr {
    /// `ValueError`.
    #[error("{0}")]
    Value(String),
    /// `UnicodeDecodeError` (a `ValueError` subclass).
    #[error("{0}")]
    UnicodeDecode(String),
    /// `OverflowError`.
    #[error("{0}")]
    Overflow(String),
    /// `ZeroDivisionError`.
    #[error("{0}")]
    ZeroDivision(String),
    /// `IndexError`.
    #[error("{0}")]
    Index(String),
    /// `RuntimeError` (what the media tools raise).
    #[error("{0}")]
    Runtime(String),
    /// `OSError` and its subclasses.
    #[error("{0}")]
    Os(PyOsError),
    /// An exception of another class returned by a dependency (`KeyError` from
    /// a media probe, `ParseError` from `config`): never caught by the implementations, carried with its
    /// class name.
    #[error("{message}")]
    Other {
        /// `type(exc).__name__`.
        class: &'static str,
        /// `str(exc)`.
        message: String,
    },
}

impl PyErr {
    /// Whether Python's `except ValueError` catches it.
    pub fn is_value_error(&self) -> bool {
        matches!(self, PyErr::Value(_) | PyErr::UnicodeDecode(_))
    }

    /// The exception's class name, as `type(exc).__name__` prints it.
    pub fn class_name(&self) -> &'static str {
        match self {
            PyErr::Value(_) => "ValueError",
            PyErr::UnicodeDecode(_) => "UnicodeDecodeError",
            PyErr::Overflow(_) => "OverflowError",
            PyErr::ZeroDivision(_) => "ZeroDivisionError",
            PyErr::Index(_) => "IndexError",
            PyErr::Runtime(_) => "RuntimeError",
            PyErr::Os(_) => "OSError",
            PyErr::Other { class, .. } => class,
        }
    }
}

/// An `OSError`: `str(exc)` is `[Errno N] <strerror>: '<filename>'` as Python prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PyOsError {
    /// The kind, for callers that branch on it (`FileNotFoundError` is `NotFound`).
    pub kind: io::ErrorKind,
    /// `errno`, when the operating system reimplemented one.
    pub errno: Option<i32>,
    /// `str(exc)`.
    pub message: String,
}

impl PyOsError {
    /// The `OSError` Python raises for `err` on `filename`.
    pub fn from_io(err: &io::Error, filename: &Path) -> Self {
        let message = match err.raw_os_error() {
            Some(code) => {
                let text = err.to_string();
                let suffix = format!(" (os error {code})");
                let strerror = text.strip_suffix(&suffix).unwrap_or(&text);
                format!(
                    "[Errno {code}] {strerror}: {}",
                    repr_str(&filename.to_string_lossy())
                )
            }
            None => err.to_string(),
        };
        PyOsError {
            kind: err.kind(),
            errno: err.raw_os_error(),
            message,
        }
    }
}

impl fmt::Display for PyOsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// `Path.read_bytes`.
pub fn read_bytes(path: &Path) -> Result<Vec<u8>, PyErr> {
    std::fs::read(path).map_err(|err| PyErr::Os(PyOsError::from_io(&err, path)))
}

/// `Path.read_bytes.decode("utf-8")`: no newline translation.
pub fn read_utf8(path: &Path) -> Result<String, PyErr> {
    decode_utf8(read_bytes(path)?)
}

/// `Path.read_text(encoding="utf-8")`: strict UTF-8, then universal newlines (`\r\n` and a
/// lone `\r` become `\n`).
pub fn read_text(path: &Path) -> Result<String, PyErr> {
    Ok(universal_newlines(&read_utf8(path)?))
}

/// `Path.write_bytes(data)`.
pub fn write_bytes(path: &Path, data: &[u8]) -> Result<(), PyErr> {
    std::fs::write(path, data).map_err(|err| PyErr::Os(PyOsError::from_io(&err, path)))
}

/// `Path.mkdir(parents=True, exist_ok=True)`.
pub fn mkdir_parents(path: &Path) -> Result<(), PyErr> {
    std::fs::create_dir_all(path).map_err(|err| PyErr::Os(PyOsError::from_io(&err, path)))
}

/// `Path.stat.st_size`.
pub fn file_size(path: &Path) -> Result<u64, PyErr> {
    std::fs::metadata(path)
        .map(|metadata| metadata.len())
        .map_err(|err| PyErr::Os(PyOsError::from_io(&err, path)))
}

/// The text mode reader's newline translation (`newline=None`).
pub fn universal_newlines(text: &str) -> String {
    if !text.contains('\r') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(character);
        }
    }
    out
}

/// `bytes.decode("utf-8")` with `CPython`'s `UnicodeDecodeError` message.
pub fn decode_utf8(bytes: Vec<u8>) -> Result<String, PyErr> {
    String::from_utf8(bytes).map_err(|err| {
        let detail = err.utf8_error();
        let bytes = err.as_bytes();
        let start = detail.valid_up_to();
        let (end, reason) = match detail.error_len() {
            None => (bytes.len(), "unexpected end of data"),
            // A valid lead byte (C2..F4) followed by a byte that cannot continue it is an
            // invalid continuation; anything else at the start is an invalid start byte.
            Some(1) if !(0xC2..=0xF4).contains(&bytes[start]) => (start + 1, "invalid start byte"),
            Some(length) => (start + length, "invalid continuation byte"),
        };
        let message = if end - start == 1 {
            format!(
                "'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}",
                bytes[start]
            )
        } else {
            format!(
                "'utf-8' codec can't decode bytes in position {start}-{}: {reason}",
                end - 1
            )
        };
        PyErr::UnicodeDecode(message)
    })
}

/// `str.isspace` for one character (Python adds U+001C..U+001F to Unicode `White_Space`).
pub fn is_space(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}

/// `str.strip`.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `str.lower`. Rust's full lowercase mapping (special casing and the final sigma included)
/// is Python's; only characters newer than the reference fixture's Unicode 14.0 can differ.
pub fn lower(text: &str) -> String {
    text.to_lowercase()
}

/// `str.splitlines` (no `keepends`): every Python line boundary, a final boundary yields no
/// empty last line.
pub fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        let boundary = matches!(
            character,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
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
        let mut next = index + character.len_utf8();
        if character == '\r' && chars.peek().map(|(_, c)| *c) == Some('\n') {
            chars.next();
            next += 1;
        }
        start = next;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// The code point of `0` of every run of ten Unicode decimal digits (`str.isdecimal`), from
/// the Unicode 14.0 database of `CPython` 3.11.
const DECIMAL_ZEROS: [u32; 66] = [
    0x0030,
    0x0660,
    0x06F0,
    0x07C0,
    0x0966,
    0x09E6,
    0x0A66,
    0x0AE6,
    0x0B66,
    0x0BE6,
    0x0C66,
    0x0CE6,
    0x0D66,
    0x0DE6,
    0x0E50,
    0x0ED0,
    0x0F20,
    0x1040,
    0x1090,
    0x17E0,
    0x1810,
    0x1946,
    0x19D0,
    0x1A80,
    0x1A90,
    0x1B50,
    0x1BB0,
    0x1C40,
    0x1C50,
    0xA620,
    0xA8D0,
    0xA900,
    0xA9D0,
    0xA9F0,
    0xAA50,
    0xABF0,
    0xFF10,
    0x0001_04A0,
    0x0001_0D30,
    0x0001_1066,
    0x0001_10F0,
    0x0001_1136,
    0x0001_11D0,
    0x0001_12F0,
    0x0001_1450,
    0x0001_14D0,
    0x0001_1650,
    0x0001_16C0,
    0x0001_1730,
    0x0001_18E0,
    0x0001_1950,
    0x0001_1C50,
    0x0001_1D50,
    0x0001_1DA0,
    0x0001_6A60,
    0x0001_6AC0,
    0x0001_6B50,
    0x0001_D7CE,
    0x0001_D7D8,
    0x0001_D7E2,
    0x0001_D7EC,
    0x0001_D7F6,
    0x0001_E140,
    0x0001_E2F0,
    0x0001_E950,
    0x0001_FBF0,
];

/// Characters that are `isdigit` but not `isdecimal` (superscripts, circled digits, …),
/// as inclusive ranges, from `CPython` 3.11.
const DIGIT_ONLY: [(u32, u32); 20] = [
    (0x00B2, 0x00B3),
    (0x00B9, 0x00B9),
    (0x1369, 0x1371),
    (0x19DA, 0x19DA),
    (0x2070, 0x2070),
    (0x2074, 0x2079),
    (0x2080, 0x2089),
    (0x2460, 0x2468),
    (0x2474, 0x247C),
    (0x2488, 0x2490),
    (0x24EA, 0x24EA),
    (0x24F5, 0x24FD),
    (0x24FF, 0x24FF),
    (0x2776, 0x277E),
    (0x2780, 0x2788),
    (0x278A, 0x2792),
    (0x0001_0A40, 0x0001_0A43),
    (0x0001_0E60, 0x0001_0E68),
    (0x0001_1052, 0x0001_105A),
    (0x0001_F100, 0x0001_F10A),
];

/// The value of a Unicode decimal digit (`str.isdecimal` / `re`'s `\d`), else `None`.
pub fn decimal_value(character: char) -> Option<u32> {
    let code = u32::from(character);
    let index = DECIMAL_ZEROS.partition_point(|zero| *zero <= code);
    let zero = *DECIMAL_ZEROS.get(index.checked_sub(1)?)?;
    (code - zero < 10).then_some(code - zero)
}

/// `str.isdigit` for one character.
pub fn is_digit_char(character: char) -> bool {
    let code = u32::from(character);
    decimal_value(character).is_some()
        || DIGIT_ONLY
            .iter()
            .any(|(low, high)| (*low..=*high).contains(&code))
}

/// `str.isdigit`.
pub fn isdigit(text: &str) -> bool {
    !text.is_empty() && text.chars().all(is_digit_char)
}

static NON_PRINTABLE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[\p{C}\p{Z}]$").expect("static pattern"));
static WORD: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[\p{L}\p{N}_]$").expect("static pattern"));

/// A word character of Python's `re` on a `str` (`\w`: `str.isalnum` or `_`). Rust's `\w`
/// also counts combining marks and connector punctuation, so `\b` is never the regex crate's.
pub fn is_word_char(character: char) -> bool {
    WORD.is_match(character.encode_utf8(&mut [0u8; 4]))
}

/// `\b` at byte offset `at` of `text` where the character before it is a word character:
/// the next character is not one (or the text ends).
pub fn word_boundary_after_word(text: &str, at: usize) -> bool {
    text[at..]
        .chars()
        .next()
        .is_none_or(|next| !is_word_char(next))
}

/// `repr` of a `str` (`CPython`'s `unicode_repr`): single quotes unless the text holds a `'`
/// and no `"`, the quote and the backslash escaped, `\t` `\n` `\r`, other control characters
/// and non-printable characters (`str.isprintable`) as `\xhh` / `\uhhhh` / `\Uhhhhhhhh`.
pub fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    let mut buffer = [0u8; 4];
    for character in text.chars() {
        let code = u32::from(character);
        match character {
            _ if character == quote || character == '\\' => {
                out.push('\\');
                out.push(character);
            }
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if (0x20..0x7f).contains(&code) => out.push(character),
            _ if code >= 0x80 && !NON_PRINTABLE.is_match(character.encode_utf8(&mut buffer)) => {
                out.push(character);
            }
            _ if code <= 0xff => write!(out, "\\x{code:02x}").expect("writing to a String"),
            _ if code <= 0xffff => write!(out, "\\u{code:04x}").expect("writing to a String"),
            _ => write!(out, "\\U{code:08x}").expect("writing to a String"),
        }
    }
    out.push(quote);
    out
}

/// `repr` of an optional `str` (`None` when absent).
pub fn repr_opt(text: Option<&str>) -> String {
    text.map_or_else(|| "None".to_string(), repr_str)
}

/// `repr` of a list of ints: `[0, 2, 1]`.
pub fn repr_int_list(values: &[usize]) -> String {
    let items: Vec<String> = values.iter().map(ToString::to_string).collect();
    format!("[{}]", items.join(", "))
}

/// The ASCII form `_PyUnicode_TransformDecimalAndSpaceToASCII` gives `int` / `float`:
/// ASCII kept, other whitespace a space, other decimal digits their ASCII digit; `None` when a
/// character is none of these (Python then fails the conversion).
fn ascii_digits_and_spaces(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        if u32::from(character) < 127 {
            out.push(character);
        } else if is_space(character) {
            out.push(' ');
        } else {
            let digit = decimal_value(character)?;
            out.push(char::from_digit(digit, 10).expect("a decimal digit"));
        }
    }
    Some(out)
}

/// `Py_ISSPACE`: the ASCII whitespace `int` and `float` strip after the transformation.
fn is_c_space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r' | '\u{0b}' | '\u{0c}')
}

/// The digits of a run of digits and single underscores between digits (`1_000`), or `None`
/// when an underscore is not between two digits.
fn digits_with_underscores(text: &str, radix: u32) -> Option<String> {
    let mut digits = String::with_capacity(text.len());
    let mut previous_underscore = true;
    for character in text.chars() {
        if character == '_' {
            if previous_underscore {
                return None;
            }
            previous_underscore = true;
        } else if character.is_digit(radix) {
            digits.push(character);
            previous_underscore = false;
        } else {
            return None;
        }
    }
    (!previous_underscore).then_some(digits)
}

/// `CPython`'s limit on the digits of a decimal `int` (`sys.get_int_max_str_digits`).
pub const INT_MAX_STR_DIGITS: usize = 4300;

/// `int(text)` (base 10) or `int(text, 16)`, with `CPython`'s `ValueError` messages.
pub fn parse_int_radix(text: &str, radix: u32) -> Result<BigInt, PyErr> {
    let invalid = || {
        let repr: String = repr_str(text).chars().take(200).collect();
        PyErr::Value(format!(
            "invalid literal for int() with base {radix}: {repr}"
        ))
    };
    let ascii = ascii_digits_and_spaces(text).ok_or_else(invalid)?;
    let trimmed = ascii.trim_matches(is_c_space);
    let (negative, mut body) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    if radix == 16 {
        // `int(x, 16)` accepts the `0x` prefix, and one underscore right after it.
        if let Some(rest) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            body = rest.strip_prefix('_').unwrap_or(rest);
        }
    }
    let digits = digits_with_underscores(body, radix).ok_or_else(invalid)?;
    if radix == 10 && digits.len() > INT_MAX_STR_DIGITS {
        return Err(PyErr::Value(format!(
            "Exceeds the limit ({INT_MAX_STR_DIGITS} digits) for integer string conversion: \
             value has {} digits; use sys.set_int_max_str_digits() to increase the limit",
            digits.len()
        )));
    }
    let magnitude = BigInt::parse_bytes(digits.as_bytes(), radix).ok_or_else(invalid)?;
    Ok(if negative { -magnitude } else { magnitude })
}

/// `int(text)`.
pub fn parse_int(text: &str) -> Result<BigInt, PyErr> {
    parse_int_radix(text, 10)
}

/// `float(text)`, with `CPython`'s grammar (whitespace, `_` between digits, `inf` / `infinity` /
/// `nan` in any case, Unicode digits) and its `ValueError` message.
pub fn parse_float(text: &str) -> Result<f64, PyErr> {
    let invalid = || {
        PyErr::Value(format!(
            "could not convert string to float: {}",
            repr_str(text)
        ))
    };
    let ascii = ascii_digits_and_spaces(text).ok_or_else(invalid)?;
    if ascii.contains('\0') {
        return Err(invalid());
    }
    let ascii = if ascii.contains('_') {
        strip_float_underscores(&ascii).ok_or_else(invalid)?
    } else {
        ascii
    };
    let trimmed = ascii.trim_matches(is_c_space);
    let (negative, body) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let lowered = body.to_ascii_lowercase();
    let magnitude = match lowered.as_str() {
        "inf" | "infinity" => f64::INFINITY,
        "nan" => f64::NAN,
        _ if is_decimal_float(body) => body.parse::<f64>().map_err(|_| invalid())?,
        _ => return Err(invalid()),
    };
    Ok(if negative { -magnitude } else { magnitude })
}

/// `_Py_string_to_number_with_underscores`: an underscore only between two ASCII digits.
fn strip_float_underscores(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut previous = '\0';
    for character in text.chars() {
        if character == '_' {
            if !previous.is_ascii_digit() {
                return None;
            }
        } else {
            if previous == '_' && !character.is_ascii_digit() {
                return None;
            }
            out.push(character);
        }
        previous = character;
    }
    (previous != '_').then_some(out)
}

/// `digits [. digits?] |. digits`, then an optional `[eE][+-]?digits`.
fn is_decimal_float(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut position = 0;
    let integral = count_ascii_digits(&bytes[position..]);
    position += integral;
    let mut fraction = 0;
    if bytes.get(position) == Some(&b'.') {
        position += 1;
        fraction = count_ascii_digits(&bytes[position..]);
        position += fraction;
    }
    if integral + fraction == 0 {
        return false;
    }
    if matches!(bytes.get(position), Some(b'e' | b'E')) {
        position += 1;
        if matches!(bytes.get(position), Some(b'+' | b'-')) {
            position += 1;
        }
        let exponent = count_ascii_digits(&bytes[position..]);
        if exponent == 0 {
            return false;
        }
        position += exponent;
    }
    position == bytes.len()
}

fn count_ascii_digits(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count()
}

/// `str(x)` / `repr(x)` of a Python float, `inf` / `-inf` / `nan` included.
pub fn float_str(value: f64) -> String {
    if value.is_nan() {
        "nan".to_string()
    } else if value.is_infinite() {
        if value > 0.0 { "inf" } else { "-inf" }.to_string()
    } else {
        python_float_repr(value)
    }
}

/// `f"{value:.{precision}f}"`: correctly rounded, ties to even (Rust's `{:.N}` rounds the same
/// way); `nan` / `inf` / `-inf` as Python spells them.
pub fn format_fixed(value: f64, precision: usize) -> String {
    if value.is_nan() {
        "nan".to_string()
    } else if value.is_infinite() {
        if value > 0.0 { "inf" } else { "-inf" }.to_string()
    } else {
        format!("{value:.precision$}")
    }
}

/// `int(x)` of a float: truncated toward zero, exact however large.
pub fn int_of_float(value: f64) -> Result<BigInt, PyErr> {
    if value.is_nan() {
        return Err(PyErr::Value(
            "cannot convert float NaN to integer".to_string(),
        ));
    }
    if value.is_infinite() {
        return Err(PyErr::Overflow(
            "cannot convert float infinity to integer".to_string(),
        ));
    }
    Ok(BigInt::from_f64(value.trunc()).unwrap_or_else(BigInt::zero))
}

/// Whether a `BigInt` is non-zero (Python truthiness).
pub fn truthy(value: &BigInt) -> bool {
    value.sign() != Sign::NoSign
}

/// `sorted(range(len(values)), key=lambda k: values[k])` as `CPython` 3.11 sorts a short list:
/// the first run (strictly descending runs reversed), then binary insertion with `<` — which
/// is the stable order for finite keys and `CPython`'s exact order around a NaN key (lists of
/// 64 or more keys with a NaN are merged differently by `CPython`; see the module notes).
pub fn sorted_indices(values: &[f64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..values.len()).collect();
    if order.len() < 2 {
        return order;
    }
    let less = |left: usize, right: usize| values[left] < values[right];
    let mut run = 2;
    if less(order[1], order[0]) {
        while run < order.len() && less(order[run], order[run - 1]) {
            run += 1;
        }
        order[..run].reverse();
    } else {
        while run < order.len() && !less(order[run], order[run - 1]) {
            run += 1;
        }
    }
    for start in run..order.len() {
        let pivot = order[start];
        let (mut low, mut high) = (0, start);
        while low < high {
            let middle = low + (high - low) / 2;
            if less(pivot, order[middle]) {
                high = middle;
            } else {
                low = middle + 1;
            }
        }
        order[low..=start].rotate_right(1);
    }
    order
}

/// `a == b` for two Python floats (IEEE equality; clippy's `float_cmp` is about approximate
/// values, while these are compared).
pub fn float_eq(left: f64, right: f64) -> bool {
    left.partial_cmp(&right) == Some(Ordering::Equal)
}
