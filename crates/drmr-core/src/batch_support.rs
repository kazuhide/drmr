//! Shared clear-HLS adapters and standard-library compatibility helpers for batch jobs.
//!
//! String, numeric, path, and URL helpers use `CPython` 3.11 semantics where serialized
//! records or configuration handling depend on them.

use crate::batch::{AttemptError, HlsSource, PlaylistFetcher};
use crate::hls_cli;
use crate::source::platform::SystemPlatform;
use crate::source::{self, Failure, FetchPolicy, Fetcher};
use serde_json::Value;
#[cfg(unix)]
use std::collections::HashMap;
use std::ffi::OsStr;
#[cfg(unix)]
use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Text-mode reading (`Path.read_text`): `\r\n` and a lone `\r` become `\n`.
pub fn universal_newlines(text: &str) -> String {
    if text.contains('\r') {
        text.replace("\r\n", "\n").replace('\r', "\n")
    } else {
        text.to_string()
    }
}

// ------------------------------------------------------------------------------------------
// The clear-HLS functions `batch` imports from `hls_source` / `hls_cli`.
// ------------------------------------------------------------------------------------------

/// The clear-HLS functions `batch` calls in-process — `source::read_analysis`,
/// `source::_Fetcher(hls_cli.fetch_policy(policy), directory)`, `_lines` and `_child_url` —
/// over the source implementation ([`crate::source`]). A `SourceError` (a `ValueError` in Python) is
/// [`AttemptError::Value`] with its token; an exception `batch` does not catch as a
/// `ValueError` is [`AttemptError::Other`] (see each method).
#[derive(Clone, Copy, Debug, Default)]
pub struct DefaultHlsSource;

/// A source failure as the exception `batch` sees: a `SourceError` or a `ValueError` is a
/// `ValueError`; any other escaping class (an `OSError`, a `TypeError`, …) is unclassified.
fn failure_error(failure: Failure) -> AttemptError {
    match failure {
        Failure::Source(error) => AttemptError::Value(error.token().to_string()),
        Failure::Escaped(escaped) if escaped.class == "ValueError" => {
            AttemptError::Value(escaped.class.to_string())
        }
        Failure::Escaped(_) | Failure::Cancelled => AttemptError::Other,
    }
}

/// `hls_cli.fetch_policy(policy)`: `FetchPolicy(**dict(policy))` with the origins made a tuple
/// and the local roots made paths (Python's `dict` forms included: `""` is the default
/// policy, `[key, value]` pairs, two-character strings, two-key objects); every failure is
/// `invalid-fetch-policy` (a `ValueError`).
fn fetch_policy(policy: &Value) -> Result<FetchPolicy, AttemptError> {
    // `max_total_resources` is the Rust tool's own analysis bound, which
    // `hls_cli::fetch_policy` admits. Python's `FetchPolicy` has no such field, so
    // `FetchPolicy(**policy)` refuses it — with any value, `null` included — as an unknown
    // keyword (a `TypeError`: `invalid-fetch-policy`); the batch preflight, which follows the
    // implementation (R3), refuses it too.
    if hls_cli::py_dict(policy).is_some_and(|entries| entries.contains_key("max_total_resources")) {
        return Err(AttemptError::Value("invalid-fetch-policy".to_string()));
    }
    hls_cli::fetch_policy(policy).map_err(|error| AttemptError::Value(error.token().to_string()))
}

/// `source::_Fetcher` reading playlists for the preflight.
struct SourceFetcher(Fetcher);

impl PlaylistFetcher for SourceFetcher {
    fn read_playlist(&mut self, url: &str) -> Result<(Vec<u8>, String), AttemptError> {
        self.0.read(url, true, None).map_err(failure_error)
    }

    fn close_all(&mut self) {
        self.0.close_all();
    }
}

impl HlsSource for DefaultHlsSource {
    /// `read_analysis(path).asset_id`. `_commands` catches `(OSError, ValueError)`: a
    /// `SourceError` and an escaping `OSError` or `ValueError` are [`AttemptError::Value`]; any
    /// other escaping class (the `AttributeError` of a record that is not an object, an
    /// `OverflowError`, a symbolic-link loop's `RuntimeError`) is [`AttemptError::Other`],
    /// which escapes `_commands` as well.
    fn read_analysis(&self, path: &Path) -> Result<String, AttemptError> {
        source::read_analysis(path)
            .map(|analysis| analysis.asset_id)
            .map_err(|failure| match failure {
                Failure::Source(error) => AttemptError::Value(error.token().to_string()),
                Failure::Escaped(escaped) if matches!(escaped.class, "OSError" | "ValueError") => {
                    AttemptError::Value(escaped.class.to_string())
                }
                Failure::Escaped(_) | Failure::Cancelled => AttemptError::Other,
            })
    }

    fn open_fetcher(
        &self,
        policy: &Value,
        directory: &Path,
    ) -> Result<Box<dyn PlaylistFetcher>, AttemptError> {
        let policy = fetch_policy(policy)?;
        Ok(Box::new(SourceFetcher(Fetcher::new(
            &policy,
            directory,
            Arc::new(SystemPlatform),
        ))))
    }

    fn lines(&self, body: &[u8]) -> Result<Vec<String>, AttemptError> {
        source::lines(body).map_err(|error| AttemptError::Value(error.token().to_string()))
    }

    fn child_url(&self, base: &str, reference: &str) -> Result<String, AttemptError> {
        source::child_url(base, reference).map_err(failure_error)
    }
}

/// `bytes.fromhex(value)`: pairs of hex digits (either case); ASCII whitespace is allowed
/// between pairs, never inside one.
pub fn py_fromhex(value: &str) -> Option<Vec<u8>> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index < bytes.len() {
        if matches!(bytes[index], b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c) {
            index += 1;
            continue;
        }
        let high = char::from(bytes[index]).to_digit(16)?;
        let low = char::from(*bytes.get(index + 1)?).to_digit(16)?;
        out.push(u8::try_from(high * 16 + low).expect("two hex digits fit a byte"));
        index += 2;
    }
    Some(out)
}

// ------------------------------------------------------------------------------------------
// Python string semantics.
// ------------------------------------------------------------------------------------------

/// `str.isspace` of one character: Unicode white space plus U+001C–U+001F, which Python
/// counts and Rust's `char::is_whitespace` does not.
pub fn py_isspace(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}

/// `str.strip`.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(py_isspace)
}

/// `str.splitlines`: lines split at `\n`, `\r`, `\r\n`, `\v`, `\f`, U+001C–U+001E, U+0085,
/// U+2028 and U+2029, without the breaks; no empty last line for a trailing break.
pub fn py_splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, character)) = chars.next() {
        let is_break = matches!(
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
        if !is_break {
            continue;
        }
        lines.push(&text[start..index]);
        let mut end = index + character.len_utf8();
        if character == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
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

/// `bytes.splitlines`: lines split at `\n`, `\r` and `\r\n` only.
pub fn py_bytes_splitlines(data: &[u8]) -> Vec<&[u8]> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < data.len() {
        match data[index] {
            b'\n' => {
                lines.push(&data[start..index]);
                start = index + 1;
            }
            b'\r' => {
                lines.push(&data[start..index]);
                if data.get(index + 1) == Some(&b'\n') {
                    index += 1;
                }
                start = index + 1;
            }
            _ => {}
        }
        index += 1;
    }
    if start < data.len() {
        lines.push(&data[start..]);
    }
    lines
}

/// Python truthiness of a JSON value (`None`, `False`, `0`, `0.0`, `""`, `[]`, `{}` are false).
pub fn py_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|float| float != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

/// `int(text)` of an argument (argparse `type=int`): surrounding white space, an optional sign,
/// ASCII digits with single underscores between them. Out-of-range values saturate (every
/// caller bounds them far below `i64`).
pub fn py_int(text: &str) -> Result<i64, String> {
    let invalid = || format!("invalid int value: '{text}'");
    let trimmed = py_strip(text);
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    let digits = python_digits(digits).ok_or_else(invalid)?;
    let magnitude: i64 = digits
        .parse::<u128>()
        .map_or(i64::MAX, |value| i64::try_from(value).unwrap_or(i64::MAX));
    Ok(if negative { -magnitude } else { magnitude })
}

/// `float(text)` of an argument (argparse `type=float`): surrounding white space, decimal or
/// exponent forms with single underscores between digits, `inf` / `infinity` / `nan` in any
/// case, with an optional sign.
pub fn py_float(text: &str) -> Result<f64, String> {
    let invalid = || format!("invalid float value: '{text}'");
    let trimmed = py_strip(text);
    let (sign, body) = match trimmed.as_bytes().first() {
        Some(b'-') => (-1.0, &trimmed[1..]),
        Some(b'+') => (1.0, &trimmed[1..]),
        _ => (1.0, trimmed),
    };
    let lowered = body.to_ascii_lowercase();
    if lowered == "inf" || lowered == "infinity" {
        return Ok(sign * f64::INFINITY);
    }
    if lowered == "nan" {
        return Ok(f64::NAN);
    }
    let (mantissa, exponent) = match lowered.split_once('e') {
        Some((mantissa, exponent)) => (mantissa, Some(exponent)),
        None => (lowered.as_str(), None),
    };
    let (integral, fraction) = match mantissa.split_once('.') {
        Some((integral, fraction)) => (integral, Some(fraction)),
        None => (mantissa, None),
    };
    let integral = if integral.is_empty() {
        String::new()
    } else {
        python_digits(integral).ok_or_else(invalid)?
    };
    let fraction = match fraction {
        Some("") | None => String::new(),
        Some(digits) => python_digits(digits).ok_or_else(invalid)?,
    };
    if integral.is_empty() && fraction.is_empty() {
        return Err(invalid());
    }
    let mut normalized = format!("{integral}.{fraction}");
    if normalized.starts_with('.') {
        normalized.insert(0, '0');
    }
    if let Some(exponent) = exponent {
        let (exponent_sign, exponent_digits) = match exponent.as_bytes().first() {
            Some(b'-') => ("-", &exponent[1..]),
            Some(b'+') => ("", &exponent[1..]),
            _ => ("", exponent),
        };
        let digits = python_digits(exponent_digits).ok_or_else(invalid)?;
        normalized = format!("{normalized}e{exponent_sign}{digits}");
    }
    let value: f64 = normalized.parse().map_err(|_| invalid())?;
    Ok(sign * value)
}

/// ASCII digits with single underscores between digits, the underscores removed.
fn python_digits(text: &str) -> Option<String> {
    if text.is_empty() || text.starts_with('_') || text.ends_with('_') || text.contains("__") {
        return None;
    }
    if !text
        .bytes()
        .all(|byte| byte.is_ascii_digit() || byte == b'_')
    {
        return None;
    }
    Some(text.replace('_', ""))
}

/// A path as pathlib spells it (`Path(text)`): repeated separators collapsed (a leading `//`
/// kept), `.` components and a trailing separator dropped, the empty path `.`.
#[cfg(unix)]
pub fn py_path(text: &OsStr) -> PathBuf {
    let bytes = text.as_bytes();
    let root: &[u8] = if bytes.starts_with(b"//") && !bytes.starts_with(b"///") {
        b"//"
    } else if bytes.starts_with(b"/") {
        b"/"
    } else {
        b""
    };
    let parts: Vec<&[u8]> = bytes
        .split(|byte| *byte == b'/')
        .filter(|part| !part.is_empty() && *part != b".")
        .collect();
    let mut out = root.to_vec();
    out.extend(parts.join(&b'/'));
    if out.is_empty() {
        out.push(b'.');
    }
    PathBuf::from(OsString::from_vec(out))
}

/// Normalize native Windows separators and dot components without decoding path bytes.
#[cfg(windows)]
pub fn py_path(text: &OsStr) -> PathBuf {
    let path: PathBuf = Path::new(text).components().collect();
    if path.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        path
    }
}

// ------------------------------------------------------------------------------------------
// os.path.realpath (3.11's `_joinrealpath`), pathlib's non-strict `resolve`.
// ------------------------------------------------------------------------------------------

/// `Path(path).resolve` (non-strict): symbolic links of the existing components resolved,
/// the rest kept lexically, `..` applied to the resolved prefix, absolute.
pub fn resolve(path: &Path) -> std::io::Result<PathBuf> {
    #[cfg(windows)]
    {
        crate::native::resolve_path(path)
    }
    #[cfg(unix)]
    {
        let mut seen = HashMap::new();
        let (resolved, _) = join_realpath(b"", path.as_os_str().as_bytes(), &mut seen, 0);
        Ok(PathBuf::from(OsString::from_vec(abspath(&resolved))))
    }
}

/// `a.is_relative_to(b)` of two resolved paths (component-wise).
pub fn is_relative_to(path: &Path, base: &Path) -> bool {
    path.starts_with(base)
}

#[cfg(unix)]
type Seen = HashMap<Vec<u8>, Option<Vec<u8>>>;

#[cfg(unix)]
fn join_realpath(path: &[u8], rest: &[u8], seen: &mut Seen, depth: usize) -> (Vec<u8>, bool) {
    let mut path = path.to_vec();
    let mut rest = rest;
    if rest.starts_with(b"/") {
        rest = &rest[1..];
        path = b"/".to_vec();
    }
    while !rest.is_empty() {
        let (name, remainder) = match rest.iter().position(|byte| *byte == b'/') {
            Some(index) => (&rest[..index], &rest[index + 1..]),
            None => (rest, &rest[rest.len()..]),
        };
        rest = remainder;
        if name.is_empty() || name == b"." {
            continue;
        }
        if name == b".." {
            if path.is_empty() {
                path = b"..".to_vec();
            } else {
                let (head, tail) = posix_split(&path);
                path = head;
                if tail == b".." {
                    path = posix_join(&posix_join(&path, b".."), b"..");
                }
            }
            continue;
        }
        let newpath = posix_join(&path, name);
        let link = std::fs::symlink_metadata(OsStr::from_bytes(&newpath))
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        let target = if link {
            std::fs::read_link(OsStr::from_bytes(&newpath)).ok()
        } else {
            None
        };
        let Some(target) = target.filter(|_| depth < 64) else {
            path = newpath;
            continue;
        };
        if let Some(entry) = seen.get(&newpath) {
            match entry {
                Some(resolved) => {
                    path.clone_from(resolved);
                    continue;
                }
                None => return (posix_join(&newpath, rest), false),
            }
        }
        seen.insert(newpath.clone(), None);
        let (resolved, ok) = join_realpath(&path, target.as_os_str().as_bytes(), seen, depth + 1);
        if !ok {
            return (posix_join(&resolved, rest), false);
        }
        seen.insert(newpath, Some(resolved.clone()));
        path = resolved;
    }
    (path, true)
}

/// `posixpath.split`.
#[cfg(unix)]
fn posix_split(path: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let index = path
        .iter()
        .rposition(|byte| *byte == b'/')
        .map_or(0, |i| i + 1);
    let (head, tail) = path.split_at(index);
    let mut head = head.to_vec();
    if !head.is_empty() && !head.iter().all(|byte| *byte == b'/') {
        while head.last() == Some(&b'/') {
            head.pop();
        }
    }
    (head, tail.to_vec())
}

/// `posixpath.join(a, b)`.
#[cfg(unix)]
fn posix_join(base: &[u8], name: &[u8]) -> Vec<u8> {
    if name.starts_with(b"/") {
        return name.to_vec();
    }
    let mut out = base.to_vec();
    if !out.is_empty() && !out.ends_with(b"/") {
        out.push(b'/');
    }
    out.extend_from_slice(name);
    out
}

/// `posixpath.abspath` (`normpath` of the path joined to the working directory).
#[cfg(unix)]
fn abspath(path: &[u8]) -> Vec<u8> {
    let joined = if path.starts_with(b"/") {
        path.to_vec()
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        posix_join(cwd.as_os_str().as_bytes(), path)
    };
    normpath(&joined)
}

/// `posixpath.normpath`.
#[cfg(unix)]
fn normpath(path: &[u8]) -> Vec<u8> {
    if path.is_empty() {
        return b".".to_vec();
    }
    let initial = if path.starts_with(b"//") && !path.starts_with(b"///") {
        2
    } else {
        usize::from(path.starts_with(b"/"))
    };
    let mut parts: Vec<&[u8]> = Vec::new();
    for part in path.split(|byte| *byte == b'/') {
        if part.is_empty() || part == b"." {
            continue;
        }
        if part != b".."
            || (initial == 0 && parts.is_empty())
            || parts.last().is_some_and(|last| *last == b"..")
        {
            parts.push(part);
        } else if !parts.is_empty() {
            parts.pop();
        }
    }
    let mut out = vec![b'/'; initial];
    out.extend(parts.join(&b'/'));
    if out.is_empty() {
        out.push(b'.');
    }
    out
}

/// `os.cpu_count or 1`: the CPUs online (`sysconf(_SC_NPROCESSORS_ONLN)`), not the
/// affinity- or quota-bounded count.
pub fn cpu_count() -> usize {
    #[cfg(target_os = "linux")]
    if let Some(count) = std::fs::read_to_string("/sys/devices/system/cpu/online")
        .ok()
        .and_then(|text| online_cpus(&text))
    {
        return count;
    }
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// The number of CPUs in a sysfs CPU list (`0-3,5,7-8`).
pub fn online_cpus(list: &str) -> Option<usize> {
    let mut count = 0;
    for part in py_strip(list).split(',') {
        let (low, high) = part.split_once('-').unwrap_or((part, part));
        let (low, high) = (low.parse::<usize>().ok()?, high.parse::<usize>().ok()?);
        count += high.checked_sub(low)? + 1;
    }
    (count > 0).then_some(count)
}

/// `random.uniform(0, high)`: a jitter, not a secret (`RandomState`'s per-process random keys).
pub fn uniform(high: f64) -> f64 {
    use std::hash::{BuildHasher, Hasher};
    static DRAWS: AtomicU64 = AtomicU64::new(0);
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u64(DRAWS.fetch_add(1, Ordering::Relaxed));
    let bits = hasher.finish() >> 11;
    // 53 random bits over 2^53: exact in an f64 (`random.random`'s construction).
    let unit = f64::from(u32::try_from(bits >> 21).expect("32 bits")).mul_add(
        2_097_152.0,
        f64::from(u32::try_from(bits & 0x1f_ffff).expect("21 bits")),
    ) / 9_007_199_254_740_992.0;
    high * unit
}

// ------------------------------------------------------------------------------------------
// urllib.parse (CPython 3.11).
// ------------------------------------------------------------------------------------------

/// `ValueError` of `urlsplit` (`Invalid IPv6 URL`, an NFKC-unsafe netloc).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid-url")]
pub struct UrlError;

/// `urllib.parse.SplitResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SplitResult {
    /// Lower-cased scheme, or the default.
    pub scheme: String,
    /// Network location (user information, host, port) as written.
    pub netloc: String,
    /// Path.
    pub path: String,
    /// Query, without `?`.
    pub query: String,
    /// Fragment, without `#`.
    pub fragment: String,
}

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

impl SplitResult {
    /// `.username`.
    pub fn username(&self) -> Option<&str> {
        let (userinfo, _) = self.netloc.rsplit_once('@')?;
        Some(userinfo.split_once(':').map_or(userinfo, |(name, _)| name))
    }

    /// `.password`.
    pub fn password(&self) -> Option<&str> {
        let (userinfo, _) = self.netloc.rsplit_once('@')?;
        userinfo.split_once(':').map(|(_, password)| password)
    }

    /// `.hostname`: lower-cased (a scoped IPv6 zone keeps its case); `None` when empty.
    pub fn hostname(&self) -> Option<String> {
        let hostinfo = self
            .netloc
            .rsplit_once('@')
            .map_or(self.netloc.as_str(), |(_, host)| host);
        let hostname = match hostinfo.split_once('[') {
            Some((_, bracketed)) => bracketed.split_once(']').map_or(bracketed, |(h, _)| h),
            None => hostinfo.split_once(':').map_or(hostinfo, |(h, _)| h),
        };
        if hostname.is_empty() {
            return None;
        }
        Some(match hostname.split_once('%') {
            Some((host, zone)) => format!("{}%{zone}", host.to_lowercase()),
            None => hostname.to_lowercase(),
        })
    }
}

/// `urllib.parse.urlsplit(url, scheme)`.
pub fn urlsplit(url: &str, default_scheme: &str) -> Result<SplitResult, UrlError> {
    let url = url.trim_start_matches(|character: char| character <= ' ');
    let mut url: String = url
        .chars()
        .filter(|character| !matches!(character, '\t' | '\r' | '\n'))
        .collect();
    let mut scheme: String = default_scheme
        .trim_matches(|character: char| character <= ' ')
        .chars()
        .filter(|character| !matches!(character, '\t' | '\r' | '\n'))
        .collect();
    if let Some(index) = url.find(':')
        && index > 0
        && url.as_bytes()[0].is_ascii_alphabetic()
        && url[..index]
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
    {
        scheme = url[..index].to_ascii_lowercase();
        url = url[index + 1..].to_string();
    }
    let mut netloc = String::new();
    if url.starts_with("//") {
        let end = url[2..]
            .find(['/', '?', '#'])
            .map_or(url.len(), |offset| offset + 2);
        netloc = url[2..end].to_string();
        url = url[end..].to_string();
        if netloc.contains('[') != netloc.contains(']') {
            return Err(UrlError);
        }
        if netloc.contains('[') {
            check_bracketed_netloc(&netloc)?;
        }
    }
    let mut fragment = String::new();
    if let Some((before, after)) = url.split_once('#') {
        fragment = after.to_string();
        url = before.to_string();
    }
    let mut query = String::new();
    if let Some((before, after)) = url.split_once('?') {
        query = after.to_string();
        url = before.to_string();
    }
    check_netloc(&netloc)?;
    Ok(SplitResult {
        scheme,
        netloc,
        path: url,
        query,
        fragment,
    })
}

/// `_check_bracketed_netloc` / `_check_bracketed_host` (3.11).
fn check_bracketed_netloc(netloc: &str) -> Result<(), UrlError> {
    let host_and_port = netloc.rsplit_once('@').map_or(netloc, |(_, rest)| rest);
    let hostname = match host_and_port.split_once('[') {
        Some((before, bracketed)) => {
            if !before.is_empty() {
                return Err(UrlError);
            }
            let (hostname, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
            if !port.is_empty() && !port.starts_with(':') {
                return Err(UrlError);
            }
            hostname
        }
        None => host_and_port
            .split_once(':')
            .map_or(host_and_port, |(h, _)| h),
    };
    if let Some(future) = hostname.strip_prefix('v') {
        let valid = future.split_once('.').is_some_and(|(version, rest)| {
            !version.is_empty()
                && version.bytes().all(|byte| byte.is_ascii_hexdigit())
                && !rest.is_empty()
                && !rest.contains('\n')
        });
        return if valid { Ok(()) } else { Err(UrlError) };
    }
    if hostname.parse::<std::net::Ipv4Addr>().is_ok() {
        return Err(UrlError);
    }
    let address = match hostname.split_once('%') {
        Some((address, scope)) if !scope.is_empty() && !scope.contains('%') => address,
        Some(_) => return Err(UrlError),
        None => hostname,
    };
    address
        .parse::<std::net::Ipv6Addr>()
        .map(|_| ())
        .map_err(|_| UrlError)
}

/// `_checknetloc`: a non-ASCII netloc must not gain `/?#@:` under NFKC.
fn check_netloc(netloc: &str) -> Result<(), UrlError> {
    use unicode_normalization::UnicodeNormalization;
    if netloc.is_ascii() {
        return Ok(());
    }
    let stripped: String = netloc
        .chars()
        .filter(|character| !matches!(character, '@' | ':' | '#' | '?'))
        .collect();
    let normalized: String = stripped.nfkc().collect();
    if stripped == normalized {
        return Ok(());
    }
    if normalized.contains(['/', '?', '#', '@', ':']) {
        return Err(UrlError);
    }
    Ok(())
}

/// `urlparse`: `urlsplit` plus the `;params` of the last path segment.
fn urlparse(url: &str, default_scheme: &str) -> Result<(SplitResult, String), UrlError> {
    let mut split = urlsplit(url, default_scheme)?;
    let mut params = String::new();
    if USES_PARAMS.contains(&split.scheme.as_str()) && split.path.contains(';') {
        let start = split.path.rfind('/').unwrap_or(0);
        if let Some(offset) = split.path[start..].find(';') {
            let index = start + offset;
            params = split.path[index + 1..].to_string();
            split.path.truncate(index);
        }
    }
    Ok((split, params))
}

/// `urlunparse` with 3.11's `urlunsplit`.
fn urlunparse(split: &SplitResult, params: &str) -> String {
    let mut url = split.path.clone();
    if !params.is_empty() {
        url = format!("{url};{params}");
    }
    let scheme = split.scheme.as_str();
    if !split.netloc.is_empty()
        || (!scheme.is_empty() && USES_NETLOC.contains(&scheme))
        || url.starts_with("//")
    {
        if !url.is_empty() && !url.starts_with('/') {
            url.insert(0, '/');
        }
        url = format!("//{}{url}", split.netloc);
    }
    if !scheme.is_empty() {
        url = format!("{scheme}:{url}");
    }
    if !split.query.is_empty() {
        url = format!("{url}?{}", split.query);
    }
    if !split.fragment.is_empty() {
        url = format!("{url}#{}", split.fragment);
    }
    url
}

/// `urllib.parse.urljoin(base, url)`.
pub fn urljoin(base: &str, url: &str) -> Result<String, UrlError> {
    if base.is_empty() {
        return Ok(url.to_string());
    }
    if url.is_empty() {
        return Ok(base.to_string());
    }
    let (base_split, base_params) = urlparse(base, "")?;
    let (mut split, mut params) = urlparse(url, &base_split.scheme)?;
    if split.scheme != base_split.scheme || !USES_RELATIVE.contains(&split.scheme.as_str()) {
        return Ok(url.to_string());
    }
    if USES_NETLOC.contains(&split.scheme.as_str()) {
        if !split.netloc.is_empty() {
            return Ok(urlunparse(&split, &params));
        }
        split.netloc.clone_from(&base_split.netloc);
    }
    if split.path.is_empty() && params.is_empty() {
        split.path.clone_from(&base_split.path);
        params = base_params;
        if split.query.is_empty() {
            split.query.clone_from(&base_split.query);
        }
        return Ok(urlunparse(&split, &params));
    }
    let mut base_parts: Vec<&str> = base_split.path.split('/').collect();
    if base_parts.last().is_some_and(|last| !last.is_empty()) {
        base_parts.pop();
    }
    let segments: Vec<&str> = if split.path.starts_with('/') {
        split.path.split('/').collect()
    } else {
        let mut segments = base_parts;
        segments.extend(split.path.split('/'));
        if segments.len() > 2 {
            let last = segments.len() - 1;
            let middle: Vec<&str> = segments[1..last]
                .iter()
                .copied()
                .filter(|segment| !segment.is_empty())
                .collect();
            let tail = segments[last];
            segments.truncate(1);
            segments.extend(middle);
            segments.push(tail);
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
    if matches!(segments.last(), Some(&"." | &"..")) {
        resolved.push("");
    }
    let joined = resolved.join("/");
    split.path = if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    };
    Ok(urlunparse(&split, &params))
}

/// `urllib.parse.unquote(text)`: percent escapes decoded as UTF-8 (invalid sequences become
/// U+FFFD), runs of non-ASCII characters kept as written.
pub fn unquote(text: &str) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut run_start = 0;
    let mut in_ascii = text.as_bytes().first().is_some_and(u8::is_ascii);
    for (index, character) in text.char_indices() {
        if character.is_ascii() != in_ascii {
            push_run(&mut out, &text[run_start..index], in_ascii);
            run_start = index;
            in_ascii = character.is_ascii();
        }
    }
    push_run(&mut out, &text[run_start..], in_ascii);
    out
}

fn push_run(out: &mut String, run: &str, ascii: bool) {
    if ascii {
        out.push_str(&String::from_utf8_lossy(&unquote_to_bytes(run.as_bytes())));
    } else {
        out.push_str(run);
    }
}

/// `urllib.parse.unquote_to_bytes`.
pub fn unquote_to_bytes(data: &[u8]) -> Vec<u8> {
    let mut parts = data.split(|byte| *byte == b'%');
    let mut out = parts.next().unwrap_or_default().to_vec();
    for item in parts {
        let hex = item
            .get(..2)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .filter(|pair| pair.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        if let Some(byte) = hex {
            out.push(byte);
            out.extend_from_slice(&item[2..]);
        } else {
            out.push(b'%');
            out.extend_from_slice(item);
        }
    }
    out
}

/// `urllib.parse.unquote_plus(text)`.
pub fn unquote_plus(text: &str) -> String {
    unquote(&text.replace('+', " "))
}

/// `urllib.parse.quote_from_bytes(data, safe)`: every byte outside `A-Za-z0-9_.-~` and `safe`
/// as an upper-case `%XX`.
pub fn quote_from_bytes(data: &[u8], safe: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(data.len());
    for byte in data {
        if byte.is_ascii_alphanumeric() || b"_.-~".contains(byte) || safe.contains(byte) {
            out.push(char::from(*byte));
        } else {
            write!(out, "%{byte:02X}").expect("String never fails to write");
        }
    }
    out
}

/// `PurePosixPath.as_uri` of an absolute path: `file://` and the path's bytes quoted.
pub fn as_uri(path: &Path) -> Result<String, source::Escaped> {
    source::pypath::as_uri(path)
}
