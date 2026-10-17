//! `CPython` 3.11 compatibility for numeric, text, binary, path, and URL operations.
//!
//! These helpers stabilize parsing and serialization where standard-library details
//! affect media metadata or reports. Integer results are bounded to 64 bits; Unicode
//! category and non-UTF-8 path handling have the limitations documented on each helper.

use regex::Regex;
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt::{self, Write as _};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

// ------------------------------------------------------------------------------------------
// str
// ------------------------------------------------------------------------------------------

/// `str.isspace` for one character: Unicode `White_Space` plus U+001C–U+001F, which Python
/// counts (bidirectional classes B and S) and `char::is_whitespace` does not.
pub fn is_space(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}

/// `str.strip` with no argument.
pub fn strip(text: &str) -> &str {
    text.trim_matches(is_space)
}

/// `repr(text)` of a `str`: single quotes unless the text holds a `'` and no `"`; the quote,
/// the backslash, `\t`, `\n`, `\r` escaped; other control and non-printable characters as
/// `\xNN`, `\uNNNN` or `\UNNNNNNNN`.
pub fn repr_str(text: &str) -> String {
    let quote = if text.contains('\'') && !text.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(text.len() + 2);
    out.push(quote);
    for character in text.chars() {
        let code = u32::from(character);
        if character == quote || character == '\\' {
            out.push('\\');
            out.push(character);
        } else if character == '\t' {
            out.push_str("\\t");
        } else if character == '\n' {
            out.push_str("\\n");
        } else if character == '\r' {
            out.push_str("\\r");
        } else if code < 0x20 || code == 0x7f {
            write!(out, "\\x{code:02x}").expect("String never fails to write");
        } else if code < 0x7f || is_printable(character) {
            out.push(character);
        } else if code <= 0xff {
            write!(out, "\\x{code:02x}").expect("String never fails to write");
        } else if code <= 0xffff {
            write!(out, "\\u{code:04x}").expect("String never fails to write");
        } else {
            write!(out, "\\U{code:08x}").expect("String never fails to write");
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable` for a non-ASCII character: not a control (Cc), format (Cf), private
/// use (Co) or separator (Zs, Zl, Zp) character. Unassigned code points (Cn), which Python
/// escapes, count as printable here (the limit the module documentation names).
fn is_printable(character: char) -> bool {
    const FORMAT: &[(u32, u32)] = &[
        (0x00ad, 0x00ad),
        (0x0600, 0x0605),
        (0x061c, 0x061c),
        (0x06dd, 0x06dd),
        (0x070f, 0x070f),
        (0x0890, 0x0891),
        (0x08e2, 0x08e2),
        (0x180e, 0x180e),
        (0x200b, 0x200f),
        (0x202a, 0x202e),
        (0x2060, 0x2064),
        (0x2066, 0x206f),
        (0xfeff, 0xfeff),
        (0xfff9, 0xfffb),
        (0x110bd, 0x110bd),
        (0x110cd, 0x110cd),
        (0x13430, 0x13438),
        (0x1bca0, 0x1bca3),
        (0x1d173, 0x1d17a),
        (0xe0001, 0xe0001),
        (0xe0020, 0xe007f),
        // Private use (Co).
        (0xe000, 0xf8ff),
        (0xf0000, 0xffffd),
        (0x10_0000, 0x10_fffd),
    ];
    let code = u32::from(character);
    !(character.is_control()
        || character.is_whitespace()
        || FORMAT
            .iter()
            .any(|&(low, high)| (low..=high).contains(&code)))
}

/// `repr` of a list of ints: `[1, 3]`.
pub fn repr_int_list(values: &[i64]) -> String {
    let items: Vec<String> = values.iter().map(ToString::to_string).collect();
    format!("[{}]", items.join(", "))
}

// ------------------------------------------------------------------------------------------
// int
// ------------------------------------------------------------------------------------------

static DECIMAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\p{Nd}$").expect("a fixed pattern compiles"));

fn is_decimal(character: char) -> bool {
    let mut buffer = [0u8; 4];
    DECIMAL.is_match(character.encode_utf8(&mut buffer))
}

/// The value of a Unicode decimal digit (general category Nd), as `int` reads it. Unicode
/// encodes every decimal digit in a contiguous run from 0 to 9, so a digit's value is its
/// distance from the start of the run of consecutive digits it sits in, modulo ten.
pub fn decimal_value(character: char) -> Option<u32> {
    if character.is_ascii_digit() {
        return character.to_digit(10);
    }
    if !is_decimal(character) {
        return None;
    }
    let code = u32::from(character);
    let mut start = code;
    while let Some(previous) = start.checked_sub(1).and_then(char::from_u32) {
        if !is_decimal(previous) {
            break;
        }
        start -= 1;
    }
    Some((code - start) % 10)
}

/// `int(text, base)`'s grammar: the sign and the digit values, or `None` for an invalid
/// literal. Non-ASCII text is first mapped as `_PyUnicode_TransformDecimalAndSpaceToASCII`
/// does (characters below U+007F kept, other whitespace to a space, decimal digits to ASCII,
/// anything else ends the text); then `PyLong_FromString` reads ASCII whitespace, a sign, the
/// `0x` prefix for base 16 (one `_` may follow it), digits with single `_` separators, and
/// trailing ASCII whitespace.
fn int_digits(text: &str, base: u32) -> Option<(bool, Vec<u32>)> {
    let ascii: Vec<u8> = if text.is_ascii() {
        text.as_bytes().to_vec()
    } else {
        let mut mapped = Vec::with_capacity(text.len());
        for character in text.chars() {
            if u32::from(character) < 0x7f {
                mapped.push(u8::try_from(character).expect("below 0x7f"));
            } else if is_space(character) {
                mapped.push(b' ');
            } else if let Some(digit) = decimal_value(character) {
                mapped.push(b'0' + u8::try_from(digit).expect("a digit"));
            } else {
                mapped.push(b'?');
                break;
            }
        }
        mapped
    };
    let is_ascii_space = |byte: &u8| matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
    let mut position = ascii.iter().take_while(|byte| is_ascii_space(byte)).count();
    let mut negative = false;
    match ascii.get(position) {
        Some(b'+') => position += 1,
        Some(b'-') => {
            negative = true;
            position += 1;
        }
        _ => {}
    }
    if base == 16
        && ascii.get(position) == Some(&b'0')
        && matches!(ascii.get(position + 1), Some(b'x' | b'X'))
    {
        position += 2;
        if ascii.get(position) == Some(&b'_') {
            position += 1;
        }
    }
    if ascii.get(position) == Some(&b'_') {
        return None;
    }
    let mut digits = Vec::new();
    let mut previous = 0u8;
    while let Some(&byte) = ascii.get(position) {
        let value = char::from(byte).to_digit(base);
        if value.is_none() && byte != b'_' {
            break;
        }
        if byte == b'_' && previous == b'_' {
            return None;
        }
        digits.extend(value);
        previous = byte;
        position += 1;
    }
    if previous == b'_' || digits.is_empty() {
        return None;
    }
    position += ascii[position..]
        .iter()
        .take_while(|byte| is_ascii_space(byte))
        .count();
    (position == ascii.len()).then_some((negative, digits))
}

/// `int(text)`: the value, or the `ValueError` message Python raises
/// (`invalid literal for int with base 10: '<repr>'`). A valid literal beyond the 64-bit
/// range is an error here where Python returns a long (the module documentation's limit).
pub fn int(text: &str) -> Result<i64, String> {
    let Some((negative, digits)) = int_digits(text, 10) else {
        // `%.200R`: the repr cut to 200 characters.
        let shown: String = repr_str(text).chars().take(200).collect();
        return Err(format!("invalid literal for int() with base 10: {shown}"));
    };
    let mut value: i64 = 0;
    for digit in digits {
        let digit = i64::from(digit);
        value = value
            .checked_mul(10)
            .and_then(|scaled| {
                if negative {
                    scaled.checked_sub(digit)
                } else {
                    scaled.checked_add(digit)
                }
            })
            .ok_or_else(|| format!("int() value out of the 64-bit range: {}", repr_str(text)))?;
    }
    Ok(value)
}

/// `int(text, 16)` limited to the non-negative 128-bit values `uuid.UUID` accepts; `None`
/// for an invalid literal or a value outside `0..2**128`.
pub fn int16_u128(text: &str) -> Option<u128> {
    let (negative, digits) = int_digits(text, 16)?;
    let mut value: u128 = 0;
    for digit in digits {
        value = value.checked_mul(16)?.checked_add(u128::from(digit))?;
    }
    (!negative || value == 0).then_some(value)
}

// ------------------------------------------------------------------------------------------
// bytes
// ------------------------------------------------------------------------------------------

/// `bytes.fromhex(text)`: pairs of hex digits, ASCII whitespace allowed between pairs; the
/// error is Python's `ValueError` message with the character position.
pub fn fromhex(text: &str) -> Result<Vec<u8>, String> {
    let error = |position: usize| {
        format!("non-hexadecimal number found in fromhex() arg at position {position}")
    };
    if let Some(position) = text.chars().position(|character| !character.is_ascii()) {
        return Err(error(position));
    }
    let bytes = text.as_bytes();
    let is_space = |byte: u8| matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
    let digit = |position: usize| {
        bytes
            .get(position)
            .and_then(|byte| char::from(*byte).to_digit(16))
            .map(|value| u8::try_from(value).expect("a hex digit"))
            .ok_or_else(|| error(position))
    };
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut position = 0;
    while position < bytes.len() {
        if is_space(bytes[position]) {
            while position < bytes.len() && is_space(bytes[position]) {
                position += 1;
            }
            if position >= bytes.len() {
                break;
            }
        }
        let top = digit(position)?;
        let bottom = digit(position + 1)?;
        out.push((top << 4) | bottom);
        position += 2;
    }
    Ok(out)
}

/// `base64.b64decode(text, validate=validate)`: `binascii.a2b_base64` in its lenient mode
/// (characters outside the alphabet skipped, decoding stopped by a complete pad sequence) or
/// its strict mode. The error is The classified error message (`binascii.Error`, or the `ValueError` a
/// non-ASCII `str` raises).
pub fn b64decode(text: &str, validate: bool) -> Result<Vec<u8>, String> {
    if !text.is_ascii() {
        return Err("string argument should contain only ASCII characters".to_string());
    }
    a2b_base64(text.as_bytes(), validate)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn a2b_base64(data: &[u8], strict: bool) -> Result<Vec<u8>, String> {
    if strict && data.first() == Some(&b'=') {
        return Err("Leading padding not allowed".to_string());
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 3);
    let (mut quad, mut left, mut pads) = (0u8, 0u8, 0u8);
    let mut padding_started = false;
    for (index, &byte) in data.iter().enumerate() {
        if byte == b'=' {
            // CPython 3.11: a pad after a complete quad is skipped even in strict mode (the
            // "Excess padding not allowed" rule came with a later version).
            padding_started = true;
            if quad >= 2 {
                pads += 1;
                if quad + pads >= 4 {
                    if strict && index + 1 < data.len() {
                        return Err("Excess data after padding".to_string());
                    }
                    return Ok(out);
                }
            }
            continue;
        }
        let Some(value) = base64_value(byte) else {
            if strict {
                return Err("Only base64 data is allowed".to_string());
            }
            continue;
        };
        if strict && padding_started {
            return Err("Discontinuous padding not allowed".to_string());
        }
        pads = 0;
        match quad {
            0 => {
                quad = 1;
                left = value;
            }
            1 => {
                quad = 2;
                out.push((left << 2) | (value >> 4));
                left = value & 0x0f;
            }
            2 => {
                quad = 3;
                out.push((left << 4) | (value >> 2));
                left = value & 0x03;
            }
            _ => {
                quad = 0;
                out.push((left << 6) | value);
                left = 0;
            }
        }
    }
    match quad {
        0 => Ok(out),
        1 => Err(format!(
            "Invalid base64-encoded string: number of data characters ({}) cannot be 1 more \
             than a multiple of 4",
            out.len() / 3 * 4 + 1
        )),
        _ => Err("Incorrect padding".to_string()),
    }
}

/// `data.decode("utf-16-le")` (strict): the text, or the `UnicodeDecodeError` message
/// (`truncated data`, `unexpected end of data`, `illegal encoding`, `illegal UTF-16
/// surrogate`) with the byte positions `CPython` reports. A BOM is kept as U+FEFF.
pub fn decode_utf16le(data: &[u8]) -> Result<String, String> {
    let error = |start: usize, end: usize, reason: &str| {
        if end == start + 1 {
            format!(
                "'utf-16-le' codec can't decode byte 0x{:02x} in position {start}: {reason}",
                data[start]
            )
        } else {
            format!(
                "'utf-16-le' codec can't decode bytes in position {start}-{}: {reason}",
                end - 1
            )
        }
    };
    let unit = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
    let mut out = String::with_capacity(data.len() / 2);
    let mut position = 0;
    while data.len() - position >= 2 {
        let first = unit(position);
        position += 2;
        if !(0xd800..=0xdfff).contains(&first) {
            out.push(char::from_u32(u32::from(first)).expect("not a surrogate"));
            continue;
        }
        if first >= 0xdc00 {
            return Err(error(position - 2, position, "illegal encoding"));
        }
        if data.len() - position < 2 {
            return Err(error(position - 2, data.len(), "unexpected end of data"));
        }
        let second = unit(position);
        position += 2;
        if !(0xdc00..=0xdfff).contains(&second) {
            return Err(error(
                position - 4,
                position - 2,
                "illegal UTF-16 surrogate",
            ));
        }
        let joined = 0x1_0000 + ((u32::from(first) - 0xd800) << 10) + (u32::from(second) - 0xdc00);
        out.push(char::from_u32(joined).expect("a supplementary code point"));
    }
    if position != data.len() {
        return Err(error(position, data.len(), "truncated data"));
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------------
// pathlib
// ------------------------------------------------------------------------------------------

/// `pathlib.PurePosixPath`: a root (`""`, `"/"`, or `"//"` — exactly two leading slashes are
/// kept, POSIX's implementation-defined case) and the parts, with empty and `.` parts dropped
/// and `..` kept.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PurePosixPath {
    root: &'static str,
    parts: Vec<String>,
}

impl PurePosixPath {
    /// `PurePosixPath(text)`.
    pub fn new(text: &str) -> Self {
        let (root, relative) = if text.starts_with('/') {
            let stripped = text.trim_start_matches('/');
            let root = if text.len() - stripped.len() == 2 {
                "//"
            } else {
                "/"
            };
            (root, stripped)
        } else {
            ("", text)
        };
        PurePosixPath {
            root,
            parts: relative
                .split('/')
                .filter(|part| !part.is_empty() && *part != ".")
                .map(str::to_string)
                .collect(),
        }
    }

    /// `PurePosixPath(str(path))` of a native path (lossy for a path that is not UTF-8).
    pub fn from_path(path: &Path) -> Self {
        PurePosixPath::new(&path.to_string_lossy())
    }

    /// `path.parts`: the root (when there is one), then each part.
    pub fn parts(&self) -> Vec<&str> {
        let mut parts: Vec<&str> = Vec::with_capacity(self.parts.len() + 1);
        if !self.root.is_empty() {
            parts.push(self.root);
        }
        parts.extend(self.parts.iter().map(String::as_str));
        parts
    }

    /// `path.name`: the last part, or `""` for a bare root or `.`.
    pub fn name(&self) -> &str {
        self.parts.last().map_or("", String::as_str)
    }

    /// `path.parent`: the path without its last part (a bare root or `.` is its own parent).
    #[must_use]
    pub fn parent(&self) -> PurePosixPath {
        let mut parent = self.clone();
        parent.parts.pop();
        parent
    }

    /// `path.suffix`: the name from its last dot, unless the dot leads or ends the name.
    pub fn suffix(&self) -> &str {
        let name = self.name();
        match name.rfind('.') {
            Some(index) if index > 0 && index < name.len() - 1 => &name[index..],
            _ => "",
        }
    }

    /// `path.stem`: the name without its suffix.
    pub fn stem(&self) -> &str {
        let name = self.name();
        &name[..name.len() - self.suffix().len()]
    }

    /// `path.is_absolute`: whether there is a root.
    pub fn is_absolute(&self) -> bool {
        !self.root.is_empty()
    }

    /// `path / other`: `other`'s parts appended, or `other` itself when it has a root.
    #[must_use]
    pub fn join(&self, other: &str) -> PurePosixPath {
        let other = PurePosixPath::new(other);
        if other.is_absolute() {
            return other;
        }
        let mut joined = self.clone();
        joined.parts.extend(other.parts);
        joined
    }

    /// `repr(Path(...))` on POSIX: `PosixPath('<str>')`.
    pub fn repr(&self) -> String {
        format!("PosixPath({})", repr_str(&self.to_string()))
    }

    /// The path as a native `PathBuf` (its `str` form).
    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(self.to_string())
    }
}

impl fmt::Display for PurePosixPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.root.is_empty() && self.parts.is_empty() {
            return f.write_str(".");
        }
        f.write_str(self.root)?;
        f.write_str(&self.parts.join("/"))
    }
}

/// `str(Path(path))`: the path as Python prints it (repeated slashes and `.` parts dropped,
/// no trailing slash).
pub fn path_str(path: &Path) -> String {
    PurePosixPath::from_path(path).to_string()
}

/// Why `Path.resolve` failed: a symlink loop (Python 3.11 raises `RuntimeError` even in
/// non-strict mode) or an I/O error reading a link.
#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    /// `RuntimeError("Symlink loop from '<path>'")`.
    #[error("Symlink loop from {0}")]
    SymlinkLoop(String),
    /// `OSError` from reading a symbolic link.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `posixpath.split`.
fn posix_split(path: &str) -> (String, String) {
    let index = path.rfind('/').map_or(0, |index| index + 1);
    let (head, tail) = path.split_at(index);
    let head = if !head.is_empty() && head.chars().any(|character| character != '/') {
        head.trim_end_matches('/')
    } else {
        head
    };
    (head.to_string(), tail.to_string())
}

/// `posixpath.join(path, name)`.
fn posix_join(path: &str, name: &str) -> String {
    if name.starts_with('/') {
        name.to_string()
    } else if path.is_empty() || path.ends_with('/') {
        format!("{path}{name}")
    } else {
        format!("{path}/{name}")
    }
}

/// `posixpath.normpath`.
fn normpath(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let slashes = if path.starts_with("//") && !path.starts_with("///") {
        2
    } else {
        usize::from(path.starts_with('/'))
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part != ".." || (slashes == 0 && parts.is_empty()) || parts.last() == Some(&"..") {
            parts.push(part);
        } else if !parts.is_empty() {
            parts.pop();
        }
    }
    let joined = format!("{}{}", "/".repeat(slashes), parts.join("/"));
    if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

type Seen = HashMap<String, Option<String>>;

/// `posixpath._joinrealpath` in non-strict mode: symbolic links resolved as they are met,
/// missing components kept as written, a loop ending the walk with the rest unresolved.
fn join_realpath(
    mut path: String,
    rest: &str,
    seen: &mut Seen,
) -> Result<(String, bool), std::io::Error> {
    let mut rest = rest;
    if let Some(stripped) = rest.strip_prefix('/') {
        rest = stripped;
        path = "/".to_string();
    }
    while !rest.is_empty() {
        let (name, remainder) = rest.split_once('/').unwrap_or((rest, ""));
        rest = remainder;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            if path.is_empty() {
                path = "..".to_string();
            } else {
                let (head, tail) = posix_split(&path);
                path = if tail == ".." {
                    posix_join(&posix_join(&head, ".."), "..")
                } else {
                    head
                };
            }
            continue;
        }
        let candidate = posix_join(&path, name);
        let is_link = std::fs::symlink_metadata(&candidate)
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        if !is_link {
            path = candidate;
            continue;
        }
        if let Some(known) = seen.get(&candidate) {
            match known {
                Some(resolved) => {
                    path.clone_from(resolved);
                    continue;
                }
                None => return Ok((posix_join(&candidate, rest), false)),
            }
        }
        seen.insert(candidate.clone(), None);
        let target = std::fs::read_link(&candidate)?;
        let (resolved, complete) = join_realpath(path, &target.to_string_lossy(), seen)?;
        if !complete {
            return Ok((posix_join(&resolved, rest), false));
        }
        seen.insert(candidate, Some(resolved.clone()));
        path = resolved;
    }
    Ok((path, true))
}

/// `Path(path).resolve` (non-strict, `CPython` 3.11): `os.path.realpath`, made absolute against
/// the working directory, then a `stat` that turns a symlink loop into an error.
pub fn resolve(path: &Path) -> Result<PathBuf, ResolveError> {
    let text = path.to_string_lossy();
    let (joined, _) = join_realpath(String::new(), &text, &mut Seen::new())?;
    let absolute = if joined.starts_with('/') {
        joined
    } else {
        let cwd = std::env::current_dir()?;
        posix_join(&cwd.to_string_lossy(), &joined)
    };
    let resolved = PurePosixPath::new(&normpath(&absolute));
    let resolved_text = resolved.to_string();
    if let Err(error) = std::fs::metadata(&resolved_text)
        && is_loop(&error)
    {
        return Err(ResolveError::SymlinkLoop(repr_str(&resolved_text)));
    }
    Ok(resolved.to_path_buf())
}

// ------------------------------------------------------------------------------------------
// glob / fnmatch
// ------------------------------------------------------------------------------------------

/// Why `Path.glob` failed: an invalid pattern (`ValueError`, The classified error message) or an I/O
/// error other than the ones the selectors ignore.
#[derive(Debug, thiserror::Error)]
pub enum GlobError {
    /// `ValueError`: `Unacceptable pattern: ''`, or `'**'` inside a component.
    #[error("{0}")]
    Value(String),
    /// `NotImplementedError("Non-relative patterns are unsupported")`.
    #[error("Non-relative patterns are unsupported")]
    NotRelative,
    /// `OSError` from listing a directory.
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// `EBADF` (the same number on every Unix).
const EBADF: i32 = 9;
/// `ELOOP` (`ErrorKind::FilesystemLoop` is not stable): Linux numbers it 40, the BSDs and
/// macOS 62.
#[cfg(any(target_os = "linux", target_os = "android"))]
const ELOOP: i32 = 40;
#[cfg(not(any(target_os = "linux", target_os = "android")))]
const ELOOP: i32 = 62;

fn is_loop(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(ELOOP)
}

/// `ENOENT`, `ENOTDIR`, `EBADF` and `ELOOP`: what `pathlib` treats as "not there".
fn ignorable(error: &std::io::Error) -> bool {
    matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
        || matches!(error.raw_os_error(), Some(EBADF | ELOOP))
}

/// `Path.exists` / `Path.is_dir`: `stat`, with the ignorable errors meaning "no".
fn stat_is(path: &Path, directory_only: bool) -> Result<bool, std::io::Error> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(!directory_only || metadata.is_dir()),
        Err(error) if ignorable(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// A directory's entries as `os.scandir` lists them: `None` when listing is not permitted
/// (the selectors stop there).
fn scandir(directory: &Path) -> Result<Option<Vec<std::fs::DirEntry>>, std::io::Error> {
    match std::fs::read_dir(directory) {
        Ok(entries) => match entries.collect::<Result<Vec<_>, _>>() {
            Ok(entries) => Ok(Some(entries)),
            Err(error) if error.kind() == ErrorKind::PermissionDenied => Ok(None),
            Err(error) => Err(error),
        },
        Err(error) if error.kind() == ErrorKind::PermissionDenied => Ok(None),
        Err(error) => Err(error),
    }
}

/// `DirEntry.is_dir(follow_symlinks=...)`, the ignorable errors meaning "no".
fn entry_is_dir(entry: &std::fs::DirEntry, follow: bool) -> Result<bool, std::io::Error> {
    let kind = entry.file_type()?;
    if kind.is_symlink() && follow {
        return stat_is(&entry.path(), true);
    }
    Ok(kind.is_dir())
}

/// `Path(directory).glob(pattern)`: the paths in `pathlib`'s order (directory listing order,
/// which the callers sort). Wildcards match every kind of entry, hidden ones included; a
/// part without wildcards is looked up directly; `**` is the directory and every directory
/// below it (symbolic links not followed).
pub fn glob(directory: &Path, pattern: &str) -> Result<Vec<PathBuf>, GlobError> {
    if pattern.is_empty() {
        return Err(GlobError::Value(format!(
            "Unacceptable pattern: {}",
            repr_str(pattern)
        )));
    }
    let parsed = PurePosixPath::new(pattern);
    if parsed.is_absolute() {
        return Err(GlobError::NotRelative);
    }
    let mut parts = parsed.parts;
    if pattern.ends_with('/') {
        parts.push(String::new());
    }
    if parts.iter().any(|part| part != "**" && part.contains("**")) {
        return Err(GlobError::Value(
            "Invalid pattern: '**' can only be an entire path component".to_string(),
        ));
    }
    if !stat_is(directory, true)? {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    select(directory, &parts, &mut found)?;
    Ok(found)
}

fn select(parent: &Path, parts: &[String], found: &mut Vec<PathBuf>) -> Result<(), GlobError> {
    let Some((pattern, rest)) = parts.split_first() else {
        found.push(parent.to_path_buf());
        return Ok(());
    };
    let directory_only = !rest.is_empty();
    if pattern.is_empty() {
        found.push(parent.to_path_buf());
    } else if pattern == "**" {
        let mut starts = Vec::new();
        directories(parent, &mut starts)?;
        let mut yielded: HashSet<PathBuf> = HashSet::new();
        for start in starts {
            let mut below = Vec::new();
            select(&start, rest, &mut below)?;
            for path in below {
                if yielded.insert(path.clone()) {
                    found.push(path);
                }
            }
        }
    } else if pattern.contains(['*', '?', '[']) {
        let Some(entries) = scandir(parent)? else {
            return Ok(());
        };
        let matcher = Fnmatch::new(pattern);
        for entry in entries {
            if directory_only {
                match entry_is_dir(&entry, true) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) if ignorable(&error) => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            if matcher.matches(&entry.file_name().to_string_lossy()) {
                select(&parent.join(entry.file_name()), rest, found)?;
            }
        }
    } else {
        let path = parent.join(pattern);
        match stat_is(&path, directory_only) {
            Ok(true) => select(&path, rest, found)?,
            Ok(false) => {}
            Err(error) if error.kind() == ErrorKind::PermissionDenied => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// `_RecursiveWildcardSelector._iterate_directories`: the directory, then every directory
/// below it depth first (symbolic links not followed).
fn directories(parent: &Path, out: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    out.push(parent.to_path_buf());
    let Some(entries) = scandir(parent)? else {
        return Ok(());
    };
    for entry in entries {
        let is_dir = match entry_is_dir(&entry, false) {
            Ok(is_dir) => is_dir,
            Err(error) if ignorable(&error) => false,
            Err(error) => return Err(error),
        };
        if is_dir {
            directories(&parent.join(entry.file_name()), out)?;
        }
    }
    Ok(())
}

/// One element of a translated shell pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Star,
    Any,
    Never,
    Literal(char),
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

impl Token {
    fn matches(&self, character: char) -> bool {
        match self {
            Token::Star | Token::Any => true,
            Token::Never => false,
            Token::Literal(expected) => *expected == character,
            Token::Class { negated, ranges } => {
                ranges
                    .iter()
                    .any(|&(low, high)| (low..=high).contains(&character))
                    != *negated
            }
        }
    }
}

/// A shell pattern as `fnmatch.translate` turns it into a regular expression, matched as
/// `re.fullmatch` matches that expression.
struct Fnmatch {
    tokens: Vec<Token>,
}

impl Fnmatch {
    fn new(pattern: &str) -> Self {
        let pattern: Vec<char> = pattern.chars().collect();
        let mut tokens = Vec::new();
        let mut index = 0;
        while index < pattern.len() {
            let character = pattern[index];
            index += 1;
            match character {
                '*' => {
                    if tokens.last() != Some(&Token::Star) {
                        tokens.push(Token::Star);
                    }
                }
                '?' => tokens.push(Token::Any),
                '[' => tokens.push(bracket(&pattern, &mut index)),
                _ => tokens.push(Token::Literal(character)),
            }
        }
        Fnmatch { tokens }
    }

    /// The classic single-star backtracking match: every token but `*` matches exactly one
    /// character, so retrying from the last star is complete.
    fn matches(&self, name: &str) -> bool {
        let name: Vec<char> = name.chars().collect();
        let (mut token, mut position) = (0, 0);
        let mut star: Option<(usize, usize)> = None;
        while position < name.len() {
            if let Some(current) = self.tokens.get(token) {
                if *current == Token::Star {
                    star = Some((token, position));
                    token += 1;
                    continue;
                }
                if current.matches(name[position]) {
                    token += 1;
                    position += 1;
                    continue;
                }
            }
            match star {
                Some((star_token, star_position)) => {
                    token = star_token + 1;
                    position = star_position + 1;
                    star = Some((star_token, star_position + 1));
                }
                None => return false,
            }
        }
        self.tokens[token..].iter().all(|rest| *rest == Token::Star)
    }
}

/// `fnmatch.fnmatchcase(name, pattern)`: the name matched against the pattern exactly as
/// `re.fullmatch(fnmatch.translate(pattern), name)` matches it (`pathlib` on POSIX).
pub fn fnmatch(name: &str, pattern: &str) -> bool {
    Fnmatch::new(pattern).matches(name)
}

/// A `[` in a shell pattern, `index` just past it: a class, or a literal `[` when no `]`
/// closes it. The class text is built as `fnmatch.translate` builds it (chunks split at
/// range hyphens, empty ranges removed, `\`, `-`, `&`, `~`, `|` escaped) and then read as
/// `re`'s parser reads a set.
fn bracket(pattern: &[char], index: &mut usize) -> Token {
    let start = *index;
    let mut end = start;
    if pattern.get(end) == Some(&'!') {
        end += 1;
    }
    if pattern.get(end) == Some(&']') {
        end += 1;
    }
    while end < pattern.len() && pattern[end] != ']' {
        end += 1;
    }
    if end >= pattern.len() {
        return Token::Literal('[');
    }
    let stuff = class_text(pattern, start, end);
    *index = end + 1;
    if stuff.is_empty() {
        return Token::Never;
    }
    if stuff == ['!'] {
        return Token::Any;
    }
    let mut body = Vec::with_capacity(stuff.len() + 1);
    if stuff[0] == '!' {
        body.push('^');
        body.extend_from_slice(&stuff[1..]);
    } else {
        if matches!(stuff[0], '^' | '[') {
            body.push('\\');
        }
        body.extend_from_slice(&stuff);
    }
    parse_set(&body)
}

/// The class text `fnmatch.translate` writes between the brackets.
fn class_text(pattern: &[char], start: usize, end: usize) -> Vec<char> {
    let stuff = &pattern[start..end];
    let mut text: Vec<char> = Vec::with_capacity(stuff.len() * 2);
    if stuff.contains(&'-') {
        let mut chunks: Vec<Vec<char>> = Vec::new();
        let mut from = start;
        let mut search = if pattern[start] == '!' {
            start + 2
        } else {
            start + 1
        };
        while let Some(hyphen) = (search..end).find(|&at| pattern[at] == '-') {
            chunks.push(pattern[from..hyphen].to_vec());
            from = hyphen + 1;
            search = hyphen + 3;
        }
        let last = pattern[from..end].to_vec();
        if last.is_empty() {
            chunks.last_mut().expect("a hyphen ended a chunk").push('-');
        } else {
            chunks.push(last);
        }
        for at in (1..chunks.len()).rev() {
            if let (Some(&tail), Some(&head)) = (chunks[at - 1].last(), chunks[at].first())
                && tail > head
            {
                let next = chunks.remove(at);
                chunks[at - 1].pop();
                chunks[at - 1].extend_from_slice(&next[1..]);
            }
        }
        for (position, chunk) in chunks.iter().enumerate() {
            if position > 0 {
                text.push('-');
            }
            for &character in chunk {
                if matches!(character, '\\' | '-') {
                    text.push('\\');
                }
                text.push(character);
            }
        }
    } else {
        for &character in stuff {
            if character == '\\' {
                text.push('\\');
            }
            text.push(character);
        }
    }
    let mut escaped = Vec::with_capacity(text.len());
    for character in text {
        if matches!(character, '&' | '~' | '|') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// `re`'s reading of a set body (the text between `[` and `]`): `^` negates; `]` first is a
/// literal; `\x` is the literal `x`; `a-b` is a range; a `-` before the closing bracket is a
/// literal.
fn parse_set(body: &[char]) -> Token {
    let mut position = 0;
    let negated = body.first() == Some(&'^');
    if negated {
        position = 1;
    }
    let item = |position: &mut usize| -> Option<char> {
        let character = *body.get(*position)?;
        *position += 1;
        if character == '\\' {
            let escaped = *body.get(*position)?;
            *position += 1;
            return Some(escaped);
        }
        Some(character)
    };
    let mut ranges: Vec<(char, char)> = Vec::new();
    while let Some(low) = item(&mut position) {
        if body.get(position) == Some(&'-') {
            position += 1;
            if let Some(high) = item(&mut position) {
                ranges.push((low, high));
            } else {
                ranges.push((low, low));
                ranges.push(('-', '-'));
            }
        } else {
            ranges.push((low, low));
        }
    }
    Token::Class { negated, ranges }
}

// ------------------------------------------------------------------------------------------
// urllib.parse
// ------------------------------------------------------------------------------------------

const USES_RELATIVE: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "imap", "wais", "file", "https", "shttp", "mms",
    "prospero", "rtsp", "rtsps", "rtspu", "sftp", "svn", "svn+ssh", "ws", "wss",
];
const USES_NETLOC: &[&str] = &[
    "", "ftp", "http", "gopher", "nntp", "telnet", "imap", "wais", "file", "mms", "https", "shttp",
    "snews", "prospero", "rtsp", "rtsps", "rtspu", "rsync", "svn", "svn+ssh", "sftp", "nfs", "git",
    "git+ssh", "ws", "wss",
];
const USES_PARAMS: &[&str] = &[
    "", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu",
    "sip", "sips", "mms", "sftp", "tel",
];

/// `urllib.parse.ParseResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParseResult {
    /// The scheme, lower-cased.
    pub scheme: String,
    /// The authority.
    pub netloc: String,
    /// The path (without the `;params` of a scheme that uses them).
    pub path: String,
    /// The `;params` of the last path segment.
    pub params: String,
    /// The query.
    pub query: String,
    /// The fragment.
    pub fragment: String,
}

fn is_scheme_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
}

/// `urllib.parse.urlsplit` (with `allow_fragments`), as five strings.
fn urlsplit(url: &str, scheme: &str) -> Result<ParseResult, String> {
    let is_c0_or_space = |character: char| character <= ' ';
    let removed = |character: &char| !matches!(character, '\t' | '\r' | '\n');
    let mut url: String = url
        .trim_start_matches(is_c0_or_space)
        .chars()
        .filter(removed)
        .collect();
    let mut result = ParseResult {
        scheme: scheme
            .trim_matches(is_c0_or_space)
            .chars()
            .filter(removed)
            .collect(),
        ..ParseResult::default()
    };
    if let Some(colon) = url.find(':')
        && colon > 0
        && url.starts_with(|first: char| first.is_ascii_alphabetic())
        && url[..colon].chars().all(is_scheme_char)
    {
        result.scheme = url[..colon].to_ascii_lowercase();
        url = url[colon + 1..].to_string();
    }
    if let Some(after) = url.strip_prefix("//") {
        let end = after.find(['/', '?', '#']).unwrap_or(after.len());
        result.netloc = after[..end].to_string();
        url = after[end..].to_string();
        let (open, close) = (result.netloc.contains('['), result.netloc.contains(']'));
        if open != close {
            return Err("Invalid IPv6 URL".to_string());
        }
        if open {
            check_bracketed_netloc(&result.netloc)?;
        }
    }
    if let Some((before, fragment)) = url.split_once('#') {
        result.fragment = fragment.to_string();
        url = before.to_string();
    }
    if let Some((before, query)) = url.split_once('?') {
        result.query = query.to_string();
        url = before.to_string();
    }
    result.path = url;
    Ok(result)
}

/// `urllib.parse._check_bracketed_netloc` and `_check_bracketed_host`.
fn check_bracketed_netloc(netloc: &str) -> Result<(), String> {
    let host_and_port = netloc.rsplit_once('@').map_or(netloc, |(_, host)| host);
    let hostname = match host_and_port.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return Err("Invalid IPv6 URL".to_string());
            }
            let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return Err("Invalid IPv6 URL".to_string());
            }
            hostname
        }
        None => host_and_port
            .split_once(':')
            .map_or(host_and_port, |(host, _)| host),
    };
    if let Some(future) = hostname.strip_prefix('v') {
        let valid = future.split_once('.').is_some_and(|(version, rest)| {
            !version.is_empty()
                && version
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
                && !rest.is_empty()
                && !rest.contains('\n')
        });
        return if valid {
            Ok(())
        } else {
            Err("IPvFuture address is invalid".to_string())
        };
    }
    if hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err("An IPv4 address cannot be in brackets".to_string());
    }
    let (address, scope) = match hostname.split_once('%') {
        Some((address, scope)) => (address, Some(scope)),
        None => (hostname, None),
    };
    let scope_valid = scope.is_none_or(|scope| !scope.is_empty() && !scope.contains('%'));
    if scope_valid && address.parse::<std::net::Ipv6Addr>().is_ok() {
        return Ok(());
    }
    Err(format!(
        "{} does not appear to be an IPv4 or IPv6 address",
        repr_str(hostname)
    ))
}

/// `urllib.parse.urlparse(url, scheme)`: `urlsplit` plus the `;params` of a scheme that uses
/// them. The error is Python's `ValueError` message.
pub fn urlparse(url: &str, scheme: &str) -> Result<ParseResult, String> {
    let mut result = urlsplit(url, scheme)?;
    if USES_PARAMS.contains(&result.scheme.as_str()) && result.path.contains(';') {
        let from = result.path.rfind('/').unwrap_or(0);
        if let Some(semicolon) = result.path[from..].find(';').map(|offset| from + offset) {
            result.params = result.path[semicolon + 1..].to_string();
            result.path.truncate(semicolon);
        }
    }
    Ok(result)
}

/// `urllib.parse.urlunparse` (`CPython` 3.11's `urlunsplit`).
pub fn urlunparse(parts: &ParseResult) -> String {
    let mut url = parts.path.clone();
    if !parts.params.is_empty() {
        url = format!("{url};{}", parts.params);
    }
    if !parts.netloc.is_empty()
        || (!parts.scheme.is_empty() && USES_NETLOC.contains(&parts.scheme.as_str()))
        || url.starts_with("//")
    {
        if !url.is_empty() && !url.starts_with('/') {
            url.insert(0, '/');
        }
        url = format!("//{}{url}", parts.netloc);
    }
    if !parts.scheme.is_empty() {
        url = format!("{}:{url}", parts.scheme);
    }
    if !parts.query.is_empty() {
        url = format!("{url}?{}", parts.query);
    }
    if !parts.fragment.is_empty() {
        url = format!("{url}#{}", parts.fragment);
    }
    url
}

/// `urllib.parse.urljoin(base, url)`: RFC 3986 resolution as `CPython` 3.11 implements it. The
/// error is Python's `ValueError` message.
pub fn urljoin(base: &str, url: &str) -> Result<String, String> {
    if base.is_empty() {
        return Ok(url.to_string());
    }
    if url.is_empty() {
        return Ok(base.to_string());
    }
    let base_parts = urlparse(base, "")?;
    let mut target = urlparse(url, &base_parts.scheme)?;
    if target.scheme != base_parts.scheme || !USES_RELATIVE.contains(&target.scheme.as_str()) {
        return Ok(url.to_string());
    }
    if USES_NETLOC.contains(&target.scheme.as_str()) {
        if !target.netloc.is_empty() {
            return Ok(urlunparse(&target));
        }
        target.netloc.clone_from(&base_parts.netloc);
    }
    if target.path.is_empty() && target.params.is_empty() {
        target.path.clone_from(&base_parts.path);
        target.params.clone_from(&base_parts.params);
        if target.query.is_empty() {
            target.query.clone_from(&base_parts.query);
        }
        return Ok(urlunparse(&target));
    }
    let mut base_segments: Vec<&str> = base_parts.path.split('/').collect();
    if base_segments.last() != Some(&"") {
        base_segments.pop();
    }
    let segments: Vec<&str> = if target.path.starts_with('/') {
        target.path.split('/').collect()
    } else {
        let mut segments = base_segments;
        segments.extend(target.path.split('/'));
        // Drop the empty inner segments that would double the slashes on re-joining
        // (`segments[1:-1] = filter(None, segments[1:-1])`).
        if segments.len() > 2 {
            let last = segments.len() - 1;
            let inner: Vec<&str> = segments[1..last]
                .iter()
                .copied()
                .filter(|segment| !segment.is_empty())
                .collect();
            let (first, final_segment) = (segments[0], segments[last]);
            segments = std::iter::once(first)
                .chain(inner)
                .chain(std::iter::once(final_segment))
                .collect();
        }
        segments
    };
    let mut resolved: Vec<&str> = Vec::new();
    for segment in &segments {
        match *segment {
            ".." => {
                resolved.pop();
            }
            "." => {}
            other => resolved.push(other),
        }
    }
    if matches!(segments.last(), Some(&("." | ".."))) {
        resolved.push("");
    }
    let joined = resolved.join("/");
    target.path = if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    };
    Ok(urlunparse(&target))
}

// ------------------------------------------------------------------------------------------
// xml.etree.ElementTree
// ------------------------------------------------------------------------------------------

/// `element.text`: the character data before the element's first child element, joined across
/// comments and processing instructions (`ElementTree`'s default tree builder drops them),
/// CDATA and entity references included; `""` where Python has `None`.
pub fn etree_text(element: roxmltree::Node<'_, '_>) -> String {
    let mut text = String::new();
    for child in element.children() {
        if child.is_element() {
            break;
        }
        if child.is_text() {
            text.push_str(child.text().unwrap_or(""));
        }
    }
    text
}

/// `element.get(name)`: the attribute WITHOUT a namespace. (`roxmltree`'s `attribute(name)`
/// matches the local name in any namespace, so `x:startNumber` would stand in for
/// `startNumber`.)
pub fn etree_get<'a>(element: roxmltree::Node<'a, '_>, name: &str) -> Option<&'a str> {
    element
        .attributes()
        .find(|attribute| attribute.namespace().is_none() && attribute.name() == name)
        .map(|attribute| attribute.value())
}

/// The text an XML processor parses (XML 1.0 §2.11): `\r\n` and a lone `\r` read as `\n`
/// before parsing. [`etree_parse`] takes text normalized this way.
pub fn xml_line_ends(text: &str) -> Cow<'_, str> {
    if text.contains('\r') {
        Cow::Owned(text.replace("\r\n", "\n").replace('\r', "\n"))
    } else {
        Cow::Borrowed(text)
    }
}

/// The deepest element nesting [`etree_parse`] accepts. `expat` has no limit; the Rust parser
/// recurses once per level, so a deeper document is refused (with a message) rather than
/// risking the stack — every `PlayReady` header fits (a 64 KiB PRO nests at most ~4,700 deep).
pub const MAX_XML_DEPTH: usize = 20_000;

/// The parse runs on its own thread, its stack sized for the document's depth: about 1 KiB
/// per level unoptimized-free, 16–32 KiB per level in a debug build (measured).
const PARSE_STACK_BASE: usize = 1 << 20;
const PARSE_STACK_PER_LEVEL: usize = if cfg!(debug_assertions) {
    40 << 10
} else {
    4 << 10
};

/// `ET.fromstring(text)`: the document, or the text of the `ParseError` Python raises
/// (`<reason>: line L, column C`, the column 0-based as `expat` counts it).
///
/// `text` must have its line ends normalized ([`xml_line_ends`]). A linear pre-scan reports
/// what `expat` rejects and the Rust parser does not (or not the same way): references to
/// characters XML does not allow, a token cut by the end of the input (`unclosed token` at its
/// start), content after the root element, an undeclared or reserved namespace prefix, a
/// duplicate attribute, a colon in a processing-instruction target, a malformed `standalone`.
/// The earliest finding of the pre-scan and of the parser is the error. Remaining differences
/// (documented): names valid only under the XML 1.0 fifth edition, elements with the `xml`
/// prefix, encoding declarations, and the position of some parser errors.
pub fn etree_parse(text: &str) -> Result<roxmltree::Document<'_>, String> {
    debug_assert!(
        !text.contains('\r'),
        "normalize line ends first (xml_line_ends)"
    );
    let scan = XmlScanner::new(text).run();
    if let Some(too_deep) = scan.too_deep_at {
        // Never parse: the parser could recurse past any stack. The earliest finding wins.
        return Err(match scan.error {
            Some(found) if found.construct < too_deep => located(text, found.at, found.reason),
            _ => located(
                text,
                too_deep,
                &format!("nesting deeper than {MAX_XML_DEPTH} elements is not supported"),
            ),
        });
    }
    let stack = PARSE_STACK_BASE + (scan.depth + 16) * PARSE_STACK_PER_LEVEL;
    let parsed = std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("xml-parse".to_string())
            .stack_size(stack)
            .spawn_scoped(scope, || roxmltree::Document::parse(text))
            .map(|handle| {
                handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
            })
    })
    .map_err(|error| format!("could not start the XML parser: {error}"))?;
    match (parsed, scan.error) {
        (Ok(document), None) => Ok(document),
        (Ok(_), Some(found)) => Err(located(text, found.at, found.reason)),
        (Err(error), found) => {
            // The pre-scan's verdict stands for any construct it judged that starts no later
            // than where the parser stopped (the parser may stop at a reference's `&` where
            // expat points at the offending character).
            let (line, column, reason) = parser_error(&error, text);
            match found {
                Some(found) if position(text, found.construct) <= (line, column) => {
                    Err(located(text, found.at, found.reason))
                }
                _ => Err(format!("{reason}: line {line}, column {column}")),
            }
        }
    }
}

/// `<reason>: line L, column C` at a byte offset of `text`.
fn located(text: &str, at: usize, reason: &str) -> String {
    let (line, column) = position(text, at);
    format!("{reason}: line {line}, column {column}")
}

/// The 1-based line and 0-based character column of a byte offset, as `expat` counts them.
fn position(text: &str, at: usize) -> (u32, u32) {
    let count = |value: usize| u32::try_from(value).unwrap_or(u32::MAX);
    let mut end = at.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let before = &text[..end];
    let line = count(before.matches('\n').count() + 1);
    let column = count(
        before
            .rsplit('\n')
            .next()
            .map_or(0, |last| last.chars().count()),
    );
    (line, column)
}

/// The `expat` reason and position for a Rust parser error: the reasons of the common cases
/// are `expat`'s own; the end-of-input and mismatched-tag positions match it, the others are
/// where the Rust parser stopped.
fn parser_error(error: &roxmltree::Error, text: &str) -> (u32, u32, &'static str) {
    use roxmltree::Error as E;
    let reason = match error {
        E::UnknownToken(_) | E::DtdDetected | E::NodesLimitReached => "syntax error",
        E::UnexpectedCloseTag(..) | E::UnexpectedEntityCloseTag(_) => "mismatched tag",
        E::UnknownEntityReference(..) => "undefined entity",
        E::EntityReferenceLoop(_) => "recursive entity reference",
        E::DuplicatedAttribute(..) | E::DuplicatedNamespace(..) => "duplicate attribute",
        E::UnknownNamespace(..) | E::InvalidElementNamePrefix(_) => "unbound prefix",
        E::InvalidXmlPrefixUri(_) | E::UnexpectedXmlUri(_) | E::UnexpectedXmlnsUri(_) => {
            "reserved prefix (xml) must not be undeclared or bound to another namespace name"
        }
        E::UnexpectedDeclaration(_) => "XML or text declaration not at start of entity",
        E::NoRootNode | E::UnclosedRootNode => "no element found",
        E::UnexpectedEndOfStream => "unclosed token",
        _ => "not well-formed (invalid token)",
    };
    let (line, column) = match error {
        E::NoRootNode | E::UnclosedRootNode | E::UnexpectedEndOfStream => {
            position(text, text.len())
        }
        // expat points at the end tag's name (0-based); the Rust parser at its `<` (1-based),
        // two characters before it.
        E::UnexpectedCloseTag(_, _, at) => (at.row, at.col + 1),
        _ => {
            let at = error.pos();
            (at.row, at.col.saturating_sub(1))
        }
    };
    (line, column, reason)
}

const UNCLOSED: &str = "unclosed token";
const INVALID: &str = "not well-formed (invalid token)";

/// The first `expat` error the pre-scan recognizes: where `expat` reports it, where the
/// construct it judges starts (a reference's `&`, a tag's `<`), and the reason.
#[derive(Clone, Copy)]
struct Finding {
    at: usize,
    construct: usize,
    reason: &'static str,
}

/// What the pre-scan found: the deepest nesting, the first error it recognizes, and where the
/// nesting first went past [`MAX_XML_DEPTH`].
struct XmlScan {
    depth: usize,
    error: Option<Finding>,
    too_deep_at: Option<usize>,
}

/// One linear, non-recursive pass over the markup (tags, attribute values, references,
/// comments, CDATA sections, processing instructions). It stops at the end of the input or at
/// markup it does not model — always malformed markup, which the parser then reports.
struct XmlScanner<'t> {
    text: &'t str,
    bytes: &'t [u8],
    at: usize,
    depth: usize,
    max_depth: usize,
    root_done: bool,
    error: Option<Finding>,
    too_deep_at: Option<usize>,
}

fn is_xml_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// XML 1.0 `Char`: what a character reference may name.
fn is_xml_char(code: u32) -> bool {
    matches!(code, 0x9 | 0xa | 0xd | 0x20..=0xd7ff | 0xe000..=0xfffd | 0x1_0000..=0x10_ffff)
}

fn find_bytes(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

fn find_byte(haystack: &[u8], from: usize, to: usize, needle: u8) -> Option<usize> {
    haystack
        .get(from..to)?
        .iter()
        .position(|byte| *byte == needle)
        .map(|offset| from + offset)
}

impl<'t> XmlScanner<'t> {
    fn new(text: &'t str) -> Self {
        XmlScanner {
            text,
            bytes: text.as_bytes(),
            at: 0,
            depth: 0,
            max_depth: 0,
            root_done: false,
            error: None,
            too_deep_at: None,
        }
    }

    fn fail(&mut self, at: usize, reason: &'static str) {
        self.fail_within(at, at, reason);
    }

    /// A finding reimplemented at `at` about the construct starting at `construct`.
    fn fail_within(&mut self, at: usize, construct: usize, reason: &'static str) {
        if self.error.is_none() {
            self.error = Some(Finding {
                at,
                construct,
                reason,
            });
        }
    }

    fn run(mut self) -> XmlScan {
        if self.text.starts_with('\u{feff}') {
            self.at = '\u{feff}'.len_utf8();
        }
        if self.bytes[self.at..].starts_with(b"<?xml")
            && self
                .bytes
                .get(self.at + 5)
                .is_some_and(|byte| is_xml_space(*byte))
        {
            self.xml_declaration(self.at);
        }
        while self.at < self.bytes.len() && self.too_deep_at.is_none() {
            let keep_going = match self.bytes[self.at] {
                b'<' => self.markup(),
                b'&' if self.depth == 0 => {
                    if self.root_done {
                        self.fail(self.at, INVALID);
                    }
                    false // a reference outside the root: the parser decides
                }
                b'&' => self.reference(self.at, self.bytes.len()),
                b']' if self.depth > 0 && self.bytes[self.at..].starts_with(b"]]>") => {
                    self.fail_within(self.at + 2, self.at, INVALID);
                    self.at += 3;
                    true
                }
                byte => {
                    if self.depth == 0 && self.root_done && !is_xml_space(byte) {
                        self.fail(self.at, "junk after document element");
                    }
                    self.at += 1;
                    true
                }
            };
            if !keep_going {
                break;
            }
        }
        XmlScan {
            depth: self.max_depth,
            error: self.error,
            too_deep_at: self.too_deep_at,
        }
    }

    /// A `<`: a comment, a CDATA section, a processing instruction, an end tag or a start tag.
    fn markup(&mut self) -> bool {
        let start = self.at;
        let rest = &self.bytes[start..];
        for marker in [&b"<!--"[..], b"<![CDATA["] {
            if rest.len() < marker.len() && marker.starts_with(rest) {
                self.fail(start, UNCLOSED); // the input ends inside the marker
                return false;
            }
        }
        if rest.starts_with(b"<!--") {
            return self.skip_to(start + 4, b"-->", start);
        }
        if rest.starts_with(b"<![CDATA[") {
            let content = start + 9;
            if let Some(end) = find_bytes(self.bytes, content, b"]]>") {
                self.at = end + 3;
                return true;
            }
            // expat holds back up to two trailing `]` (they could open `]]>`).
            let held = self.bytes[content..]
                .iter()
                .rev()
                .take_while(|byte| **byte == b']')
                .count()
                .min(2);
            self.fail(self.bytes.len() - held, "unclosed CDATA section");
            return false;
        }
        if rest.starts_with(b"<!") {
            return false; // a DOCTYPE (refused before parsing) or malformed: the parser decides
        }
        if rest.starts_with(b"<?") {
            return self.processing_instruction(start);
        }
        if rest.starts_with(b"</") {
            let Some(end) = find_byte(self.bytes, start + 2, self.bytes.len(), b'>') else {
                self.fail(start, UNCLOSED);
                return false;
            };
            if self.depth > 0 {
                self.depth -= 1;
                self.root_done |= self.depth == 0;
            }
            self.at = end + 1;
            return true;
        }
        self.start_tag(start)
    }

    fn skip_to(&mut self, from: usize, marker: &[u8], start: usize) -> bool {
        if let Some(end) = find_bytes(self.bytes, from, marker) {
            self.at = end + marker.len();
            true
        } else {
            self.fail(start, UNCLOSED);
            false
        }
    }

    fn processing_instruction(&mut self, start: usize) -> bool {
        let target_start = start + 2;
        let target_end = target_start
            + self.bytes[target_start..]
                .iter()
                .take_while(|byte| !is_xml_space(**byte) && **byte != b'?')
                .count();
        let Some(end) = find_bytes(self.bytes, target_start, b"?>") else {
            self.fail(start, UNCLOSED);
            return false;
        };
        let target = &self.text[target_start..target_end];
        if let Some(colon) = target.find(':') {
            // Namespace processing: no colon in a processing-instruction target.
            self.fail_within(target_start + colon, start, INVALID);
        } else if target.eq_ignore_ascii_case("xml") && target != "xml" {
            self.fail_within(target_end, start, INVALID); // a reserved target in another case
        }
        self.at = end + 2;
        true
    }

    /// The XML declaration at the start: `standalone` must be `yes` or `no`.
    fn xml_declaration(&mut self, start: usize) {
        let Some(end) = find_bytes(self.bytes, start, b"?>") else {
            return; // cut: the processing-instruction path reports it
        };
        let mut at = start + 5;
        while at < end {
            while at < end && is_xml_space(self.bytes[at]) {
                at += 1;
            }
            let name_start = at;
            while at < end && !is_xml_space(self.bytes[at]) && self.bytes[at] != b'=' {
                at += 1;
            }
            let name = &self.text[name_start..at];
            while at < end && is_xml_space(self.bytes[at]) {
                at += 1;
            }
            if at >= end || self.bytes[at] != b'=' {
                return; // malformed: the parser decides
            }
            at += 1;
            while at < end && is_xml_space(self.bytes[at]) {
                at += 1;
            }
            let Some(&quote) = self
                .bytes
                .get(at)
                .filter(|byte| matches!(**byte, b'"' | b'\''))
            else {
                return;
            };
            let Some(value_end) = find_byte(self.bytes, at + 1, end, quote) else {
                return;
            };
            let value = &self.text[at + 1..value_end];
            if name == "standalone" && value != "yes" && value != "no" {
                self.fail_within(at + 1, start, "XML declaration not well-formed");
            }
            at = value_end + 1;
        }
    }

    fn start_tag(&mut self, start: usize) -> bool {
        if self.depth == 0 && self.root_done {
            self.fail(start, "junk after document element");
        }
        let name_start = start + 1;
        let mut at = name_start
            + self.bytes[name_start..]
                .iter()
                .take_while(|byte| !is_xml_space(**byte) && !matches!(**byte, b'/' | b'>'))
                .count();
        if self.bytes.get(name_start) == Some(&b':') {
            // Namespace processing: a name cannot start with ':'.
            self.fail_within(name_start, start, INVALID);
        }
        let mut seen: Vec<&str> = Vec::new();
        loop {
            while at < self.bytes.len() && is_xml_space(self.bytes[at]) {
                at += 1;
            }
            match self.bytes.get(at) {
                None => {
                    self.fail(start, UNCLOSED);
                    return false;
                }
                Some(b'>') => {
                    self.open(start);
                    self.at = at + 1;
                    return true;
                }
                Some(b'/') => match self.bytes.get(at + 1) {
                    Some(b'>') => {
                        self.root_done |= self.depth == 0;
                        self.at = at + 2;
                        return true;
                    }
                    None => {
                        self.fail(start, UNCLOSED);
                        return false;
                    }
                    Some(_) => return false, // malformed: the parser decides
                },
                Some(_) => match self.attribute(start, at, &mut seen) {
                    Some(next) => at = next,
                    None => return false,
                },
            }
        }
    }

    /// One `name="value"` of a start tag at `at`; the offset after it, or `None` to stop.
    fn attribute(&mut self, start: usize, at: usize, seen: &mut Vec<&'t str>) -> Option<usize> {
        let name_end = at
            + self.bytes[at..]
                .iter()
                .take_while(|byte| !is_xml_space(**byte) && !matches!(**byte, b'=' | b'>' | b'/'))
                .count();
        let name = &self.text[at..name_end];
        if name.starts_with(':') {
            self.fail_within(at, start, INVALID);
        }
        if seen.contains(&name) {
            self.fail_within(at, start, "duplicate attribute");
        }
        seen.push(name);
        if name == "xmlns:xmlns" {
            self.fail(
                start,
                "reserved prefix (xmlns) must not be declared or undeclared",
            );
        }
        let mut next = name_end;
        while next < self.bytes.len() && is_xml_space(self.bytes[next]) {
            next += 1;
        }
        if next >= self.bytes.len() {
            self.fail(start, UNCLOSED);
            return None;
        }
        if self.bytes[next] != b'=' {
            return None; // malformed: the parser decides
        }
        next += 1;
        while next < self.bytes.len() && is_xml_space(self.bytes[next]) {
            next += 1;
        }
        let Some(&quote) = self.bytes.get(next) else {
            self.fail(start, UNCLOSED);
            return None;
        };
        if !matches!(quote, b'"' | b'\'') {
            return None;
        }
        let Some(value_end) = find_byte(self.bytes, next + 1, self.bytes.len(), quote) else {
            self.fail(start, UNCLOSED);
            return None;
        };
        if let Some(less) = find_byte(self.bytes, next + 1, value_end, b'<') {
            self.fail_within(less, start, INVALID); // `<` is not allowed in an attribute value
        }
        let mut reference_at = next + 1;
        while let Some(amp) = find_byte(self.bytes, reference_at, value_end, b'&') {
            if !self.reference(amp, value_end) {
                return None;
            }
            reference_at = self.at;
        }
        if name.starts_with("xmlns:") && value_end == next + 1 {
            self.fail(start, "must not undeclare prefix");
        }
        Some(value_end + 1)
    }

    fn open(&mut self, start: usize) {
        self.depth += 1;
        self.max_depth = self.max_depth.max(self.depth);
        if self.depth > MAX_XML_DEPTH && self.too_deep_at.is_none() {
            self.too_deep_at = Some(start);
        }
    }

    /// A reference at `amp` that must end before `limit` (the end of the input in content, the
    /// closing quote in an attribute value). Sets the scan position after it; `false` stops.
    fn reference(&mut self, amp: usize, limit: usize) -> bool {
        let truncated = limit == self.bytes.len();
        if self.bytes.get(amp + 1) != Some(&b'#') {
            if let Some(semicolon) = find_byte(self.bytes, amp + 1, limit, b';') {
                self.at = semicolon + 1;
                return true;
            }
            if truncated {
                self.fail(amp, UNCLOSED);
            }
            return false;
        }
        let hex = self.bytes.get(amp + 2) == Some(&b'x');
        let radix = if hex { 16 } else { 10 };
        let digits = amp + 2 + usize::from(hex);
        let mut value: Option<u32> = Some(0);
        let mut at = digits;
        loop {
            let Some(&byte) = self.bytes.get(at).filter(|_| at < limit) else {
                if truncated {
                    self.fail(amp, UNCLOSED);
                } else {
                    self.fail_within(at, amp, INVALID);
                }
                return false;
            };
            if byte == b';' {
                if at == digits {
                    self.fail_within(at, amp, INVALID);
                } else if !value.is_some_and(is_xml_char) {
                    self.fail(amp, "reference to invalid character number");
                }
                self.at = at + 1;
                return true;
            }
            let Some(digit) = char::from(byte).to_digit(radix) else {
                self.fail_within(at, amp, INVALID);
                self.at = at;
                return true;
            };
            value = value
                .and_then(|value| value.checked_mul(radix))
                .and_then(|value| value.checked_add(digit));
            at += 1;
        }
    }
}
