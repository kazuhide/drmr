//! JSON formatting and local HTTP compatibility helpers.
use drmr_core::canonical::Dumps;
use serde_json::Value;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub fn dumps(value: &Value) -> String {
    Dumps::DEFAULT.dumps(value)
}
pub fn jsonify(value: &Value) -> Vec<u8> {
    (Dumps::CANONICAL.dumps(value) + "\n").into_bytes()
}

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

/// `os.path.abspath(path)`: joined to the working directory when relative, then normalised.
#[cfg(unix)]
pub fn abspath(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    normpath(&joined)
}

#[cfg(windows)]
pub fn abspath(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
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

pub fn is_file(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
}

/// `Path.is_dir()`.
/// Python's surrogate escapes would carry instead).
pub fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// `type(exc).__name__` of the `OSError` subclass Python raises for this error.
/// Seconds since the epoch as Python's `time.time()` reads them.
pub fn time_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64())
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

/// `int(text)` for a decimal string: surrounding whitespace, a sign and single underscores
/// between digits allowed (ASCII digits only in this implementation).
pub fn python_int_text(text: &str) -> Option<i128> {
    let text = drmr_core::source::pytext::strip(text);
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
    #[test]
    fn literal_port_numbers_keep_decimal_sign_and_separator_rules() {
        for (text, expected) in [
            (" 9_090 ", Some(9090)),
            ("+80", Some(80)),
            ("-1", Some(-1)),
            ("1__2", None),
            ("http", None),
            ("_1", None),
            ("1_", None),
        ] {
            assert_eq!(python_int_text(text), expected, "{text}");
        }
    }
    #[test]
    fn path_names_are_normalized_lexically_without_parent_escape() {
        assert_eq!(
            normpath(Path::new("one/./two/../asset.js")),
            PathBuf::from("one/asset.js")
        );
        assert_eq!(
            normpath(Path::new("../../private")),
            PathBuf::from("../../private")
        );
        assert_eq!(
            normpath(Path::new("/one/../../asset")),
            PathBuf::from("/asset")
        );
    }
    #[test]
    fn http_dates_and_checksums_round_trip() {
        for value in [0, 1, 86_400, 1_792_202_235] {
            assert_eq!(parse_http_date(&http_date(value)), Some(value));
        }
        assert_eq!(adler32(b"Wikipedia"), 0x11e6_0398);
        assert_eq!(percent_decode("a%20b%2Fc"), b"a b/c");
        assert_eq!(quote("a b/c", "/"), "a%20b/c");
    }
    #[test]
    fn json_and_diagnostic_strings_keep_data_escaped() {
        assert_eq!(
            String::from_utf8(jsonify(&serde_json::json!({"b":2,"a":"\n"}))).unwrap(),
            "{\"a\":\"\\n\",\"b\":2}\n"
        );
        assert_eq!(repr_str("line\nnext"), "'line\\nnext'");
    }
}
