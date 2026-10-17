//! JSON, string, path and timestamp compatibility helpers.

use drmr_core::canonical::{Dumps, python_float_repr};
use serde_json::Value;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

// ------------------------------------------------------------------------------------------
// JSON
// ------------------------------------------------------------------------------------------

/// `json.dumps(value)` (insertion order, `", "` / `": "`, ASCII only): log lines, SSE payloads,
/// private configs, job records.
pub fn dumps(value: &Value) -> String {
    Dumps::DEFAULT.dumps(value)
}

pub fn jsonify(value: &Value) -> Vec<u8> {
    let mut text = Dumps::CANONICAL.dumps(value);
    // The marker as `write_string` escapes it: the NUL becomes `\u0000`.
    let quoted = format!("\"\\u0000{}", &BIG_INT_MARK[1..]);
    if text.contains(&quoted) {
        text = unmark_big_ints(&text, &quoted);
    }
    text.push('\n');
    text.into_bytes()
}

/// What [`big_int`] puts in a value tree: a NUL (never in manifest text, which is XML) and a
/// per-process nonce, so no string from anywhere else can be read as one.
static BIG_INT_MARK: LazyLock<String> = LazyLock::new(|| format!("\u{0}int:{}:", token_hex(16)));

/// A Python `int` beyond the 64 bits a JSON number holds here (`serde_json` without arbitrary
/// precision), for [`jsonify`] to write as the bare digits, as `json.dumps` writes any int.
/// `digits` is an optional `-` and ASCII digits (`BigInt`'s `Display`).
pub fn big_int(digits: &str) -> Value {
    Value::String(format!("{}{digits}", *BIG_INT_MARK))
}

/// Every serialized [`big_int`] marker (`"<quoted mark><digits>"`) replaced by its digits.
fn unmark_big_ints(text: &str, quoted: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(quoted) {
        out.push_str(&rest[..start]);
        let after = &rest[start + quoted.len()..];
        let end = after.find('"').unwrap_or(after.len());
        out.push_str(&after[..end]);
        rest = after.get(end + 1..).unwrap_or_default();
    }
    out.push_str(rest);
    out
}

/// `json.loads(data)` of a request body or a file read as bytes: the encoding is detected as
/// Python's `json.detect_encoding` does (a UTF-8 BOM is dropped; UTF-16 and UTF-32 with or
/// without a BOM). Python-only extensions (`NaN`, `Infinity`, a lone surrogate escape, a number
/// beyond a double) are refused, a documented divergence.
pub fn loads_bytes(data: &[u8]) -> Option<Value> {
    let text = decode_json_bytes(data)?;
    serde_json::from_str(&text).ok()
}

/// `json.loads(text)` where only the shape of the result matters to the caller: Python's
/// non-finite constants (`NaN`, `Infinity`, `-Infinity`, valid JSON values for Python) read as
/// `null`. The two callers ([`crate::jobs::ensure_jsonl`]: is the line JSON at all; the clear-HLS
/// allowlist: a non-finite float is neither a string, an int, finite, nor unhashable, exactly as
/// `None`) see the same outcome either way. Other Python-only inputs (a lone surrogate escape, a
/// number beyond a double) are still refused.
pub fn loads_nonfinite_as_null(text: &str) -> Option<Value> {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let (mut start, mut index) = (0, 0);
    let mut in_string = false;
    while index < bytes.len() {
        if in_string {
            match bytes[index] {
                b'\\' => index += 2,
                b'"' => {
                    in_string = false;
                    index += 1;
                }
                _ => index += 1,
            }
            continue;
        }
        if bytes[index] == b'"' {
            in_string = true;
            index += 1;
            continue;
        }
        let rest = &bytes[index..];
        let token = [&b"NaN"[..], b"Infinity", b"-Infinity"]
            .into_iter()
            .find(|token| rest.starts_with(token))
            .map_or(0, <[u8]>::len);
        if token == 0 {
            index += 1;
            continue;
        }
        out.push_str(&text[start..index]);
        out.push_str("null");
        index += token;
        start = index;
    }
    out.push_str(&text[start.min(text.len())..]);
    serde_json::from_str(&out).ok()
}

fn decode_json_bytes(data: &[u8]) -> Option<String> {
    let starts = |prefix: &[u8]| data.starts_with(prefix);
    if starts(b"\xef\xbb\xbf") {
        return String::from_utf8(data[3..].to_vec()).ok();
    }
    if starts(b"\xff\xfe\x00\x00") || starts(b"\x00\x00\xfe\xff") {
        return decode_utf32(data, starts(b"\xff\xfe\x00\x00"), 4);
    }
    if starts(b"\xff\xfe") || starts(b"\xfe\xff") {
        return decode_utf16(&data[2..], starts(b"\xff\xfe"));
    }
    if data.len() >= 4 {
        match (data[0] == 0, data[1] == 0, data[2] == 0, data[3] == 0) {
            (true, true, true, false) => return decode_utf32(data, false, 0),
            (false, true, true, true) => return decode_utf32(data, true, 0),
            (true, false, _, _) => return decode_utf16(data, false),
            (false, true, _, _) => return decode_utf16(data, true),
            _ => {}
        }
    } else if data.len() == 2 {
        if data[0] == 0 {
            return decode_utf16(data, false);
        }
        if data[1] == 0 {
            return decode_utf16(data, true);
        }
    }
    String::from_utf8(data.to_vec()).ok()
}

fn decode_utf16(data: &[u8], little: bool) -> Option<String> {
    if !data.len().is_multiple_of(2) {
        return None;
    }
    let units = data.as_chunks::<2>().0.iter().map(|pair| {
        if little {
            u16::from_le_bytes(*pair)
        } else {
            u16::from_be_bytes(*pair)
        }
    });
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .ok()
}

fn decode_utf32(data: &[u8], little: bool, skip: usize) -> Option<String> {
    let data = &data[skip..];
    if !data.len().is_multiple_of(4) {
        return None;
    }
    data.as_chunks::<4>()
        .0
        .iter()
        .map(|quad| {
            let code = if little {
                u32::from_le_bytes(*quad)
            } else {
                u32::from_be_bytes(*quad)
            };
            char::from_u32(code)
        })
        .collect()
}

// ------------------------------------------------------------------------------------------
// repr()
// ------------------------------------------------------------------------------------------

/// `repr(text)`: single quotes unless the text holds a `'` and no `"`; backslash, the quote,
/// `\t`, `\n`, `\r` escaped; other control characters and the non-printable characters this
/// port knows (C1 controls, the Unicode space and format characters, private use) as `\xHH`,
/// `\uHHHH` or `\UHHHHHHHH`. Python's full printability table (unassigned code points) is not
/// carried: such a character is written as it is.
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
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ if c == quote => {
                out.push('\\');
                out.push(c);
            }
            _ if u32::from(c) < 0x20 || u32::from(c) == 0x7f => {
                write!(out, "\\x{:02x}", u32::from(c)).expect("String never fails to write");
            }
            _ if u32::from(c) < 0x7f || printable(c) => out.push(c),
            _ => {
                let code = u32::from(c);
                if code <= 0xff {
                    write!(out, "\\x{code:02x}")
                } else if code <= 0xffff {
                    write!(out, "\\u{code:04x}")
                } else {
                    write!(out, "\\U{code:08x}")
                }
                .expect("String never fails to write");
            }
        }
    }
    out.push(quote);
    out
}

/// `str.isprintable()` for a non-ASCII character, as far as the categories this implementation knows:
/// `Cc`, `Cf`, `Co`, `Zl`, `Zp` and `Zs` are not printable.
fn printable(c: char) -> bool {
    !matches!(u32::from(c),
        0x80..=0xa0 | 0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x890..=0x891 | 0x8e2
        | 0x1680 | 0x180e | 0x2000..=0x200f | 0x2028..=0x202f | 0x205f..=0x2064
        | 0x2066..=0x206f | 0x3000 | 0xe000..=0xf8ff | 0xfeff | 0xfff9..=0xfffb
        | 0x110bd | 0x110cd | 0x13430..=0x13438 | 0x1bca0..=0x1bca3 | 0x1d173..=0x1d17a
        | 0xe0001 | 0xe0020..=0xe007f | 0xf0000..=0xffffd | 0x10_0000..=0x10_fffd)
}

/// `repr(value)` of a value `json.loads` produced: `None`, `True` / `False`, an int, a float
/// (`float.__repr__`), a string ([`repr_str`]), a list or a dict.
pub fn repr_value(value: &Value) -> String {
    match value {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(number) => {
            if let Some(integer) = number.as_i64() {
                integer.to_string()
            } else if let Some(integer) = number.as_u64() {
                integer.to_string()
            } else {
                python_float_repr(number.as_f64().unwrap_or(0.0))
            }
        }
        Value::String(text) => repr_str(text),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(repr_value).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(key, item)| format!("{}: {}", repr_str(key), repr_value(item)))
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

// ------------------------------------------------------------------------------------------
// Paths
// ------------------------------------------------------------------------------------------

/// Why `Path.resolve()` raised instead of returning a path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    /// `ValueError: embedded null byte` (from `os.lstat`).
    #[error("embedded null byte")]
    Nul,
    /// `RuntimeError: Symlink loop from …` (`Path.resolve` re-checks with `stat`).
    #[error("symlink loop")]
    Loop,
}

/// `os.path.abspath(path)`: joined to the working directory when relative, then normalised.
pub fn abspath(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    normpath(&joined)
}

/// `os.path.normpath(path)` on POSIX: `.` and empty components dropped, `..` applied
/// lexically, two leading slashes kept (three or more become one).
pub fn normpath(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text.is_empty() {
        return PathBuf::from(".");
    }
    let initial = if text.starts_with("//") && !text.starts_with("///") {
        "//"
    } else if text.starts_with('/') {
        "/"
    } else {
        ""
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in text.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if !parts.is_empty() && parts.last() != Some(&"..") {
                    parts.pop();
                } else if initial.is_empty() {
                    parts.push("..");
                }
            }
            _ => parts.push(part),
        }
    }
    let joined = format!("{initial}{}", parts.join("/"));
    PathBuf::from(if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    })
}

/// `pathlib.Path(path).resolve()` (`CPython` 3.11, non-strict): every symlink resolved, a missing
/// component kept as written, `..` applied to the resolved prefix; a symlink loop is
/// [`ResolveError::Loop`] (Python's `RuntimeError`), a NUL byte [`ResolveError::Nul`].
pub fn resolve(path: &Path) -> Result<PathBuf, ResolveError> {
    let text = path.to_string_lossy();
    if text.contains('\0') {
        return Err(ResolveError::Nul);
    }
    let mut seen: Vec<(PathBuf, Option<PathBuf>)> = Vec::new();
    let (resolved, _) = join_realpath(PathBuf::new(), &text, &mut seen)?;
    let result = abspath(&resolved);
    // `Path.resolve` re-stats in non-strict mode so that a loop raises.
    if let Err(error) = std::fs::metadata(&result)
        && error.raw_os_error() == Some(ELOOP)
    {
        return Err(ResolveError::Loop);
    }
    Ok(result)
}

#[cfg(target_os = "linux")]
const ELOOP: i32 = 40;
#[cfg(not(target_os = "linux"))]
const ELOOP: i32 = 62;

/// `posixpath._joinrealpath(path, rest, strict=False, seen)`.
fn join_realpath(
    mut path: PathBuf,
    rest: &str,
    seen: &mut Vec<(PathBuf, Option<PathBuf>)>,
) -> Result<(PathBuf, bool), ResolveError> {
    let mut rest = rest.to_string();
    if rest.starts_with('/') {
        rest.remove(0);
        path = PathBuf::from("/");
    }
    while !rest.is_empty() {
        let (name, tail) = match rest.split_once('/') {
            Some((name, tail)) => (name.to_string(), tail.to_string()),
            None => (rest.clone(), String::new()),
        };
        rest = tail;
        if name.is_empty() || name == "." {
            continue;
        }
        if name == ".." {
            path = parent_join(&path);
            continue;
        }
        let candidate = posix_join(&path, &name);
        let is_link = std::fs::symlink_metadata(&candidate)
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        if !is_link {
            path = candidate;
            continue;
        }
        if let Some(index) = seen.iter().position(|(link, _)| *link == candidate) {
            if let Some(resolved) = seen[index].1.clone() {
                path = resolved;
                continue;
            }
            return Ok((posix_join(&candidate, &rest), false));
        }
        seen.push((candidate.clone(), None));
        let target = std::fs::read_link(&candidate).unwrap_or_default();
        let (resolved, ok) = join_realpath(path, &target.to_string_lossy(), seen)?;
        if !ok {
            return Ok((posix_join(&resolved, &rest), false));
        }
        if let Some(entry) = seen.iter_mut().find(|(link, _)| *link == candidate) {
            entry.1 = Some(resolved.clone());
        }
        path = resolved;
    }
    Ok((path, true))
}

/// The `..` step of `_joinrealpath`: `split(path)`, keeping a `..` that is already there.
fn parent_join(path: &Path) -> PathBuf {
    let text = path.to_string_lossy().to_string();
    if text.is_empty() {
        return PathBuf::from("..");
    }
    let (head, tail) = match text.rfind('/') {
        Some(index) => {
            let head = &text[..=index];
            let trimmed = head.trim_end_matches('/');
            let head = if trimmed.is_empty() { head } else { trimmed };
            (head.to_string(), text[index + 1..].to_string())
        }
        None => (String::new(), text.clone()),
    };
    if tail == ".." {
        return posix_join(&posix_join(Path::new(&head), ".."), "..");
    }
    PathBuf::from(head)
}

/// `posixpath.join(a, b)`: `b` replaces `a` when absolute, otherwise one slash between them.
fn posix_join(base: &Path, name: &str) -> PathBuf {
    if name.starts_with('/') {
        return PathBuf::from(name);
    }
    let text = base.to_string_lossy();
    if text.is_empty() || text.ends_with('/') {
        PathBuf::from(format!("{text}{name}"))
    } else if name.is_empty() {
        PathBuf::from(format!("{text}/"))
    } else {
        PathBuf::from(format!("{text}/{name}"))
    }
}

/// `path.is_relative_to(base)` (`PurePath`): `base`'s components are a prefix of `path`'s.
pub fn is_relative_to(path: &Path, base: &Path) -> bool {
    path.starts_with(base)
}

/// `path != base and base in path.parents`.
pub fn strictly_under(path: &Path, base: &Path) -> bool {
    path != base && path.starts_with(base)
}

/// `Path.is_file()`: `stat` (symlinks followed) says a regular file; any error is `False`.
pub fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// `Path.is_dir()`.
pub fn is_dir(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// `Path.is_symlink()`.
pub fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

/// `Path.exists()`.
pub fn exists(path: &Path) -> bool {
    std::fs::metadata(path).is_ok()
}

/// `str(path)` of a path built from text: the path as text (non-UTF-8 bytes replaced, which
/// Python's surrogate escapes would carry instead).
pub fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `type(exc).__name__` of the `OSError` subclass Python raises for this error.
pub fn exception_name(error: &std::io::Error) -> &'static str {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::PermissionDenied => "PermissionError",
        ErrorKind::NotFound => "FileNotFoundError",
        ErrorKind::AlreadyExists => "FileExistsError",
        ErrorKind::NotADirectory => "NotADirectoryError",
        ErrorKind::IsADirectory => "IsADirectoryError",
        ErrorKind::Interrupted => "InterruptedError",
        ErrorKind::TimedOut => "TimeoutError",
        ErrorKind::WouldBlock => "BlockingIOError",
        ErrorKind::BrokenPipe => "BrokenPipeError",
        ErrorKind::ConnectionRefused => "ConnectionRefusedError",
        ErrorKind::ConnectionReset => "ConnectionResetError",
        ErrorKind::ConnectionAborted => "ConnectionAbortedError",
        _ => "OSError",
    }
}

/// `str(OSError)` for a failed file operation: `[Errno 13] Permission denied: '/path'`.
pub fn oserror_text(error: &std::io::Error, path: &Path) -> String {
    let text = error.to_string();
    match error.raw_os_error() {
        Some(code) => {
            let detail = text
                .strip_suffix(&format!(" (os error {code})"))
                .unwrap_or(&text)
                .to_string();
            format!("[Errno {code}] {detail}: {}", repr_str(&path_text(path)))
        }
        None => text,
    }
}

/// A path with only normal components (no root, `.` or `..`), relative.
pub fn plain_relative(path: &Path) -> bool {
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
}

// ------------------------------------------------------------------------------------------
// Time
// ------------------------------------------------------------------------------------------

/// Seconds since the epoch as Python's `time.time()` reads them.
pub fn time_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
}

/// `datetime.fromtimestamp(seconds, timezone.utc).isoformat()`: microseconds rounded half to
/// even, as `CPython` rounds a float timestamp.
pub fn iso_from_timestamp(seconds: f64) -> String {
    let (whole, micros) = split_timestamp(seconds);
    drmr_core::events::format_utc(whole, micros)
}

/// A float timestamp as whole seconds and microseconds (`round-half-even` of the fraction).
#[allow(clippy::cast_possible_truncation)] // reason: whole seconds of a real timestamp fit i64
pub fn split_timestamp(seconds: f64) -> (i64, u32) {
    let whole = seconds.floor();
    let mut micros = ((seconds - whole) * 1e6).round_ties_even();
    let mut whole = whole as i64;
    if micros >= 1e6 {
        whole += 1;
        micros -= 1e6;
    }
    (whole, u32::try_from(micros as i64).unwrap_or(0))
}

/// `email.utils.formatdate(seconds, usegmt=True)`: `Sat, 17 Oct 2026 01:57:15 GMT`.
pub fn http_date(seconds: i64) -> String {
    let iso = drmr_core::events::format_utc(seconds, 0);
    // `YYYY-MM-DDTHH:MM:SS+00:00`
    let (year, month, day) = (&iso[0..4], &iso[5..7], &iso[8..10]);
    let time = &iso[11..19];
    let days = seconds.div_euclid(86_400);
    let weekday = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]
        [usize::try_from(days.rem_euclid(7)).expect("0..7")];
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = months[month.parse::<usize>().unwrap_or(1) - 1];
    format!("{weekday}, {day} {month} {year} {time} GMT")
}

pub fn parse_http_date(text: &str) -> Option<i64> {
    let words: Vec<&str> = text
        .split([' ', ',', '\t'])
        .filter(|word| !word.is_empty())
        .collect();
    let month_index = |name: &str| {
        let lower = name.to_ascii_lowercase();
        [
            "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
        ]
        .iter()
        .position(|month| lower.starts_with(month))
    };
    // Skip a leading weekday name.
    let words: Vec<&str> = if words
        .first()
        .is_some_and(|w| w.chars().all(char::is_alphabetic))
        && words.first().and_then(|w| month_index(w)).is_none()
    {
        words[1..].to_vec()
    } else {
        words
    };
    let (day, month, year, time, zone) = match words.as_slice() {
        // `17 Oct 2026 01:57:15 GMT`
        [day, month, year, time, zone @ ..] if day.parse::<u32>().is_ok() => {
            (*day, *month, *year, *time, zone.first().copied())
        }
        // `17-Oct-26 01:57:15 GMT`
        [date, time, zone @ ..] if date.contains('-') => {
            let mut parts = date.split('-');
            let (day, month, year) = (parts.next()?, parts.next()?, parts.next()?);
            (day, month, year, *time, zone.first().copied())
        }
        // asctime: `Oct 17 01:57:15 2026`
        [month, day, time, year, ..] => (*day, *month, *year, *time, None),
        _ => return None,
    };
    let day: i64 = day.parse().ok()?;
    let month = i64::try_from(month_index(month)? + 1).ok()?;
    let mut year: i64 = year.parse().ok()?;
    if year < 100 {
        year += if year > 68 { 1900 } else { 2000 };
    }
    let mut clock = time.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next().unwrap_or("0").parse().ok()?;
    let offset = match zone {
        Some(zone) if zone.starts_with('+') || zone.starts_with('-') => {
            let sign = if zone.starts_with('-') { -1 } else { 1 };
            let digits: i64 = zone[1..].parse().ok()?;
            sign * ((digits / 100) * 3600 + (digits % 100) * 60)
        }
        _ => 0,
    };
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 24
        || minute > 59
        || second > 61
    {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// `datetime.fromisoformat(text)` (`CPython` 3.11's C parser) succeeds.
pub fn fromisoformat_valid(text: &str) -> bool {
    iso::parse_datetime(text).is_some()
}

mod iso {
    //! A port of the 3.11 `_datetimemodule.c` ISO parser, kept to its acceptance: the fields are
    //! parsed exactly as C does (ASCII digits only, reading on into the rest of the string), then
    //! range-checked as `new_datetime` and `new_timezone` check them.

    /// The byte at `index`, or NUL past the end (a C string).
    fn at(bytes: &[u8], index: usize) -> u8 {
        bytes.get(index).copied().unwrap_or(0)
    }

    fn digits(bytes: &[u8], pos: &mut usize, count: usize) -> Option<i64> {
        let mut value = 0i64;
        for _ in 0..count {
            let byte = at(bytes, *pos);
            *pos += 1;
            if !byte.is_ascii_digit() {
                return None;
            }
            value = value * 10 + i64::from(byte - b'0');
        }
        Some(value)
    }

    fn separator(bytes: &[u8]) -> usize {
        let len = bytes.len();
        if len == 7 {
            return 7;
        }
        if at(bytes, 4) == b'-' {
            if at(bytes, 5) == b'W' {
                if len > 8 && at(bytes, 8) == b'-' {
                    if len > 10 && at(bytes, 10).is_ascii_digit() {
                        return 8;
                    }
                    return 10;
                }
                return 8;
            }
            return 10;
        }
        if at(bytes, 4) == b'W' {
            let mut index = 7;
            while index < len && at(bytes, index).is_ascii_digit() {
                index += 1;
            }
            if index < 9 {
                return index;
            }
            return if index % 2 == 0 { 7 } else { 8 };
        }
        8
    }

    fn date(bytes: &[u8], len: usize) -> Option<(i64, i64, i64)> {
        let mut pos = 0;
        let year = digits(bytes, &mut pos, 4)?;
        let uses_separator = at(bytes, pos) == b'-';
        if uses_separator {
            pos += 1;
        }
        if at(bytes, pos) == b'W' {
            pos += 1;
            let week = digits(bytes, &mut pos, 2)?;
            let day = if pos < len {
                if uses_separator {
                    let sep = at(bytes, pos);
                    pos += 1;
                    if sep != b'-' {
                        return None;
                    }
                }
                digits(bytes, &mut pos, 1)?
            } else {
                1
            };
            return iso_to_ymd(year, week, day);
        }
        let month = digits(bytes, &mut pos, 2)?;
        if uses_separator {
            let sep = at(bytes, pos);
            pos += 1;
            if sep != b'-' {
                return None;
            }
        }
        let day = digits(bytes, &mut pos, 2)?;
        Some((year, month, day))
    }

    fn ordinal(year: i64, month: i64, day: i64) -> i64 {
        let y = year - 1;
        let before_year = y * 365 + y / 4 - y / 100 + y / 400;
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let before_month = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334]
            [usize::try_from(month - 1).unwrap_or(0)]
            + i64::from(leap && month > 2);
        before_year + before_month + day
    }

    fn iso_to_ymd(year: i64, week: i64, day: i64) -> Option<(i64, i64, i64)> {
        if !(1..53).contains(&week) {
            let first_weekday = ordinal(year, 1, 1) % 7;
            let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
            if week != 53 || !(first_weekday == 4 || (first_weekday == 3 && leap)) {
                return None;
            }
        }
        if !(1..8).contains(&day) {
            return None;
        }
        let first_day = ordinal(year, 1, 1);
        let first_weekday = (first_day + 6) % 7;
        let mut monday = first_day - first_weekday;
        if first_weekday > 3 {
            monday += 7;
        }
        Some(from_ordinal(monday + (week - 1) * 7 + day - 1))
    }

    fn from_ordinal(ordinal: i64) -> (i64, i64, i64) {
        // Days since 0001-01-01 → civil date (the inverse of `ordinal`).
        let days = ordinal - 719_163; // 1970-01-01 is ordinal 719 163
        let shifted = days + 719_468;
        let era = shifted.div_euclid(146_097);
        let day_of_era = shifted.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        (year_of_era + era * 400 + i64::from(month <= 2), month, day)
    }

    /// `parse_hh_mm_ss_ff(tstr, tstr_end)`: `(h, m, s, us, rv)` with C's return value.
    fn clock(bytes: &[u8], start: usize, end: usize) -> Result<([i64; 4], bool), ()> {
        let mut values = [0i64; 4];
        let mut pos = start;
        let mut has_separator = true;
        for index in 0..3 {
            values[index] = digits(bytes, &mut pos, 2).ok_or(())?;
            let c = at(bytes, pos);
            pos += 1;
            if index == 0 {
                has_separator = c == b':';
            }
            if pos >= end {
                return Ok((values, c != 0));
            }
            if c == b'.' || c == b',' {
                break;
            } else if !has_separator {
                pos -= 1;
            } else if c != b':' {
                return Err(());
            }
        }
        let remaining = end.saturating_sub(pos);
        let to_parse = remaining.min(6);
        let mut micros = digits(bytes, &mut pos, to_parse).ok_or(())?;
        if to_parse < 6 && to_parse > 0 {
            micros *= [100_000, 10_000, 1000, 100, 10][to_parse - 1];
        }
        values[3] = micros;
        while at(bytes, pos).is_ascii_digit() {
            pos += 1;
        }
        Ok((values, at(bytes, pos) != 0))
    }

    /// The time part: `(h, m, s, us, Some(offset_us) | None)`.
    fn time(bytes: &[u8], start: usize) -> Option<([i64; 4], Option<i64>)> {
        let end = bytes.len();
        let mut tz = start;
        loop {
            if matches!(at(bytes, tz), b'Z' | b'+' | b'-') {
                break;
            }
            tz += 1;
            if tz >= end {
                break;
            }
        }
        let (values, trailing) = clock(bytes, start, tz).ok()?;
        if tz >= end {
            return if trailing { None } else { Some((values, None)) };
        }
        if at(bytes, tz) == b'Z' {
            return if at(bytes, tz + 1) == 0 {
                Some((values, Some(0)))
            } else {
                None
            };
        }
        let sign = if at(bytes, tz) == b'-' { -1 } else { 1 };
        let (zone, trailing) = clock(bytes, tz + 1, end).ok()?;
        if trailing {
            return None;
        }
        let offset = sign * ((zone[0] * 3600 + zone[1] * 60 + zone[2]) * 1_000_000 + zone[3]);
        Some((values, Some(offset)))
    }

    /// `(year, month, day, hour, minute, second, microsecond)` and the zone check passed.
    pub(super) fn parse_datetime(text: &str) -> Option<()> {
        if text.chars().count() < 7 {
            return None;
        }
        let bytes = text.as_bytes();
        let sep = separator(bytes);
        let (year, month, day) = date(bytes, sep)?;
        let mut values = [0i64; 4];
        let mut offset = None;
        if bytes.len() > sep {
            let width = match at(bytes, sep) {
                byte if byte & 0x80 == 0 => 1,
                byte if byte & 0xf0 == 0xe0 => 3,
                byte if byte & 0xf0 == 0xf0 => 4,
                _ => 2,
            };
            let (clock, zone) = time(bytes, sep + width)?;
            values = clock;
            offset = zone;
        }
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let month_days = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];
        let valid_date = (1..=9999).contains(&year)
            && (1..=12).contains(&month)
            && (1..=month_days[usize::try_from(month - 1).ok()?]).contains(&day);
        let valid_time = (0..=23).contains(&values[0])
            && (0..=59).contains(&values[1])
            && (0..=59).contains(&values[2])
            && (0..=999_999).contains(&values[3]);
        let valid_zone = offset.is_none_or(|micros| micros.abs() < 86_400_000_000);
        (valid_date && valid_time && valid_zone).then_some(())
    }
}

// ------------------------------------------------------------------------------------------
// Identifiers and checksums
// ------------------------------------------------------------------------------------------

/// `secrets.token_hex(bytes)`.
pub fn token_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    getrandom::fill(&mut buffer).expect("the operating system supplies random bytes");
    hex::encode(buffer)
}

/// `uuid.uuid4().hex`: 32 lower-case hex digits with the version and variant bits of a v4 UUID.
pub fn uuid4_hex() -> String {
    let mut buffer = [0u8; 16];
    getrandom::fill(&mut buffer).expect("the operating system supplies random bytes");
    buffer[6] = (buffer[6] & 0x0f) | 0x40;
    buffer[8] = (buffer[8] & 0x3f) | 0x80;
    hex::encode(buffer)
}

/// `zlib.adler32(data) & 0xFFFFFFFF`.
pub fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for byte in chunk {
            a += u32::from(*byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

// ------------------------------------------------------------------------------------------
// URLs
// ------------------------------------------------------------------------------------------

/// `urllib.parse.quote(text, safe=…)`: UTF-8 bytes outside the unreserved set and `safe`
/// percent-encoded (upper-case hex).
pub fn quote(text: &str, safe: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric()
            || b"_.-~".contains(&byte)
            || safe.as_bytes().contains(&byte)
        {
            out.push(char::from(byte));
        } else {
            write!(out, "%{byte:02X}").expect("String never fails to write");
        }
    }
    out
}

/// Percent-decoding of a URL path or query component as bytes (`%` not followed by two hex
/// digits is kept as it is).
pub fn percent_decode(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let (Some(high), Some(low)) = (
                char::from(bytes[index + 1]).to_digit(16),
                char::from(bytes[index + 2]).to_digit(16),
            )
        {
            out.push(u8::try_from(high * 16 + low).expect("two hex digits are a byte"));
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    out
}

/// `urllib.parse.ParseResult`: [`drmr_core::hls_cli::SplitResult`] with `;params` split off
/// the last path segment for the schemes that use them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParseResult {
    /// The scheme.
    pub scheme: String,
    /// The authority.
    pub netloc: String,
    /// The path without `;params`.
    pub path: String,
    /// The params.
    pub params: String,
    /// The query.
    pub query: String,
    /// The fragment.
    pub fragment: String,
}

/// `urllib.parse.uses_params`.
const USES_PARAMS: [&str; 16] = [
    "", "ftp", "hdl", "prospero", "http", "imap", "https", "shttp", "rtsp", "rtsps", "rtspu",
    "sip", "sips", "mms", "sftp", "tel",
];

/// `urllib.parse.urlparse(url)` on top of `drmr_core::hls_cli::urlsplit` (its `ValueError` is
/// [`drmr_core::hls_cli::UrlError`]).
pub fn urlparse(url: &str) -> Result<ParseResult, drmr_core::hls_cli::UrlError> {
    let drmr_core::hls_cli::SplitResult {
        scheme,
        netloc,
        path,
        query,
        fragment,
    } = drmr_core::hls_cli::urlsplit(url)?;
    let (path, params) = if USES_PARAMS.contains(&scheme.as_str()) && path.contains(';') {
        let split_at = match path.rfind('/') {
            Some(slash) => path[slash..].find(';').map(|offset| slash + offset),
            None => path.find(';'),
        };
        match split_at {
            Some(index) => (path[..index].to_string(), path[index + 1..].to_string()),
            None => (path, String::new()),
        }
    } else {
        (path, String::new())
    };
    Ok(ParseResult {
        scheme,
        netloc,
        path,
        params,
        query,
        fragment,
    })
}

// ------------------------------------------------------------------------------------------
// int()
// ------------------------------------------------------------------------------------------

/// `int(text)` for a decimal string: surrounding whitespace, a sign and single underscores
/// between digits allowed (ASCII digits only in this implementation).
pub fn python_int_text(text: &str) -> Option<i128> {
    let text = drmr_core::hls_cli::py_strip(text);
    let (negative, digits) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'_')
    {
        return None;
    }
    let value: i128 = digits.replace('_', "").parse().unwrap_or(i128::MAX);
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_big_int_is_written_as_its_digits_and_a_lookalike_string_stays_text() {
        let value = json!({
            "b": big_int("99999999999999999999"),
            "a": [big_int("-18446744073709551617")],
            "c": "\u{0}int:00:1",
        });
        assert_eq!(
            String::from_utf8(jsonify(&value)).unwrap(),
            "{\"a\":[-18446744073709551617],\"b\":99999999999999999999,\"c\":\"\\u0000int:00:1\"}\n"
        );
    }

    #[test]
    fn urlparse_splits_params_from_the_last_segment() {
        let parsed = urlparse("https://cdn/a/b/c;v=1?q").unwrap();
        assert_eq!(
            (parsed.path.as_str(), parsed.params.as_str()),
            ("/a/b/c", "v=1")
        );
        let parsed = urlparse("https://cdn/a/b;x=1/c").unwrap();
        assert_eq!(
            (parsed.path.as_str(), parsed.params.as_str()),
            ("/a/b;x=1/c", "")
        );
    }

    #[test]
    fn python_int_text_follows_int() {
        assert_eq!(python_int_text(" 1_000 "), Some(1000));
        assert_eq!(python_int_text("6.0"), None);
        assert_eq!(python_int_text("+5"), Some(5));
        assert_eq!(python_int_text("1__0"), None);
    }

    #[test]
    fn python_non_finite_constants_read_as_null_outside_strings_only() {
        assert_eq!(
            loads_nonfinite_as_null(
                r#"{"a": NaN, "b": [Infinity, -Infinity], "c": "NaN \" Infinity"}"#
            ),
            Some(json!({"a": null, "b": [null, null], "c": "NaN \" Infinity"}))
        );
        assert_eq!(loads_nonfinite_as_null("-Infinity"), Some(Value::Null));
        // Still invalid where Python refuses them too.
        for invalid in [
            "[-NaN]",
            "{NaN: 1}",
            "[1 NaN]",
            "[NaNa]",
            "nan",
            "[Infinityx]",
            "{\"a\": é}",
        ] {
            assert_eq!(loads_nonfinite_as_null(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn repr_follows_python() {
        assert_eq!(repr_str("a'b"), "\"a'b\"");
        assert_eq!(repr_str("a'b\""), "'a\\'b\"'");
        assert_eq!(repr_str("../../x"), "'../../x'");
        assert_eq!(
            repr_str("a\u{0}b\tc\u{7f}\u{85}é"),
            "'a\\x00b\\tc\\x7f\\x85é'"
        );
        assert_eq!(
            repr_value(&json!([1, "x", null, true, {"k": 1.5}])),
            "[1, 'x', None, True, {'k': 1.5}]"
        );
    }

    #[test]
    fn normpath_and_resolve_follow_posixpath() {
        assert_eq!(
            normpath(Path::new("/a//b/./c/../d")),
            PathBuf::from("/a/b/d")
        );
        assert_eq!(normpath(Path::new("//a")), PathBuf::from("//a"));
        assert_eq!(normpath(Path::new("///a")), PathBuf::from("/a"));
        assert_eq!(normpath(Path::new("/..")), PathBuf::from("/"));
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        assert_eq!(
            resolve(&root.join("link/missing/../x")).unwrap(),
            root.join("real/x")
        );
        std::os::unix::fs::symlink(root.join("loop"), root.join("loop")).unwrap();
        assert_eq!(
            resolve(&root.join("loop/x")).unwrap_err(),
            ResolveError::Loop
        );
        assert_eq!(
            resolve(Path::new("/a\u{0}b")).unwrap_err(),
            ResolveError::Nul
        );
    }

    #[test]
    fn fromisoformat_accepts_what_cpython_3_11_accepts() {
        for valid in [
            "2026-10-16T00:00:00+00:00",
            "2026-10-16T01:57:15.123456+00:00",
            "2026-10-16",
            "20261016T010203",
            "2026-10-16 01:02",
            "2026-W42-5T00:00Z",
            "2026-10-16T01:02:03.1234567",
        ] {
            assert!(fromisoformat_valid(valid), "{valid}");
        }
        for invalid in [
            "",
            "2026-13-01",
            "2026-02-30",
            "2026-10-16T25:00",
            "x",
            "2026-10-16T00:00+24:00",
            "2026-10-16T00:00Zx",
        ] {
            assert!(!fromisoformat_valid(invalid), "{invalid}");
        }
    }

    #[test]
    fn dates_and_checksums() {
        assert_eq!(http_date(1_792_202_235), "Sat, 17 Oct 2026 01:57:15 GMT");
        assert_eq!(
            parse_http_date("Sat, 17 Oct 2026 01:57:15 GMT"),
            Some(1_792_202_235)
        );
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(split_timestamp(1.999_999_6), (2, 0));
        assert_eq!(uuid4_hex().len(), 32);
        assert_eq!(token_hex(4).len(), 8);
        assert_eq!(percent_decode("a%2Fb%zz%2"), b"a/b%zz%2".to_vec());
        assert_eq!(quote("/a b/é", "/"), "/a%20b/%C3%A9");
    }

    #[test]
    fn json_bodies_detect_their_encoding() {
        assert_eq!(
            loads_bytes(b"\xef\xbb\xbf{\"a\": 1}"),
            Some(json!({"a": 1}))
        );
        assert_eq!(
            loads_bytes(
                &"{\"a\": 1}"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>()
            ),
            Some(json!({"a": 1}))
        );
        assert_eq!(loads_bytes(b"{bad"), None);
        assert_eq!(
            jsonify(&json!({"b": 1, "a": [1, 2]})),
            b"{\"a\":[1,2],\"b\":1}\n".to_vec()
        );
    }
}
