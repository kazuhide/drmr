//! `urllib.parse` as the clear-HLS reader uses it: `urlsplit` (and its `hostname`, `port`,
//! `username`, `password`), `urlunsplit`, `urljoin`, `unquote` and `quote_from_bytes`, implemented
//! from `CPython` 3.11, the reference. The effective URLs these produce are recorded in
//! the frozen snapshot, so its digest depends on every byte of them
//! (`tests/fixtures/hls_source/urls.json`, written by 3.11, pins the implementation against the reference;
//! 3.13 differs only in `urlunsplit`, see there).

use super::pytext::decimal_digit;
use std::fmt::Write as _;
use std::net::IpAddr;

/// Python's `ValueError` from the parser (an invalid bracketed host, a netloc that NFKC
/// normalization would change into one containing `/?#@:`, a port that is not a number).
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid URL")]
pub struct UrlError;

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

/// Code points whose NFKC form contains one of `/?#@:` (Unicode 14, the `unicodedata`;
/// unchanged through 15.1): `_checknetloc` refuses
/// a non-ASCII netloc holding one, since IDNA would turn it into a delimiter.
const NFKC_DELIMITERS: [u32; 19] = [
    0x2047, 0x2048, 0x2049, 0x2100, 0x2101, 0x2105, 0x2106, 0x2A74, 0xFE13, 0xFE16, 0xFE55, 0xFE56,
    0xFE5F, 0xFE6B, 0xFF03, 0xFF0F, 0xFF1A, 0xFF1F, 0xFF20,
];

/// A `SplitResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SplitResult {
    /// `scheme` (lower case).
    pub scheme: String,
    /// `netloc`.
    pub netloc: String,
    /// `path`.
    pub path: String,
    /// `query`.
    pub query: String,
    /// `fragment`.
    pub fragment: String,
}

impl SplitResult {
    fn hostinfo(&self) -> (&str, Option<&str>) {
        let hostinfo = rpartition(&self.netloc, '@').2;
        let (hostname, port) = match hostinfo.split_once('[') {
            Some((_, bracketed)) => {
                let (hostname, after) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                (hostname, after.split_once(':').map_or("", |(_, port)| port))
            }
            None => hostinfo.split_once(':').unwrap_or((hostinfo, "")),
        };
        (hostname, (!port.is_empty()).then_some(port))
    }

    fn userinfo(&self) -> (Option<&str>, Option<&str>) {
        let (userinfo, have_info, _) = rpartition(&self.netloc, '@');
        if !have_info {
            return (None, None);
        }
        match userinfo.split_once(':') {
            Some((username, password)) => (Some(username), Some(password)),
            None => (Some(userinfo), None),
        }
    }

    /// `hostname`: lower case (a scoped IPv6 zone keeps its case), `None` when empty.
    pub fn hostname(&self) -> Option<String> {
        let (hostname, _) = self.hostinfo();
        if hostname.is_empty() {
            return None;
        }
        Some(match hostname.split_once('%') {
            Some((host, zone)) => format!("{}%{zone}", host.to_lowercase()),
            None => hostname.to_lowercase(),
        })
    }

    /// `port`: `Ok(None)` when absent; ASCII digits within `0..=65535`, else `ValueError`.
    pub fn port(&self) -> Result<Option<u16>, UrlError> {
        match self.hostinfo().1 {
            None => Ok(None),
            Some(port) if port.bytes().all(|b| b.is_ascii_digit()) => {
                let digits = port.trim_start_matches('0');
                if digits.len() > 5 {
                    return Err(UrlError);
                }
                let value: u32 = if digits.is_empty() {
                    0
                } else {
                    digits.parse().map_err(|_| UrlError)?
                };
                u16::try_from(value).map(Some).map_err(|_| UrlError)
            }
            Some(_) => Err(UrlError),
        }
    }

    /// `username`.
    pub fn username(&self) -> Option<&str> {
        self.userinfo().0
    }

    /// `password`.
    pub fn password(&self) -> Option<&str> {
        self.userinfo().1
    }
}

/// `str.rpartition(separator)`.
fn rpartition(text: &str, separator: char) -> (&str, bool, &str) {
    match text.rfind(separator) {
        Some(index) => (&text[..index], true, &text[index + separator.len_utf8()..]),
        None => ("", false, text),
    }
}

fn is_c0_or_space(c: char) -> bool {
    c <= ' '
}

/// `urlsplit(url, scheme, allow_fragments=True)`.
pub fn urlsplit(url: &str, scheme: &str) -> Result<SplitResult, UrlError> {
    let mut url: String = url
        .trim_start_matches(is_c0_or_space)
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut scheme: String = scheme
        .trim_matches(is_c0_or_space)
        .chars()
        .filter(|c| !matches!(c, '\t' | '\r' | '\n'))
        .collect();
    let mut netloc = String::new();
    let mut query = String::new();
    let mut fragment = String::new();
    if let Some(colon) = url.find(':') {
        let first = url.chars().next();
        if colon > 0
            && first.is_some_and(|c| c.is_ascii_alphabetic())
            && url[..colon]
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        {
            scheme = url[..colon].to_ascii_lowercase();
            url = url[colon + 1..].to_string();
        }
    }
    if url.starts_with("//") {
        let end = url[2..]
            .find(['/', '?', '#'])
            .map_or(url.len(), |index| index + 2);
        netloc = url[2..end].to_string();
        url = url[end..].to_string();
        let open = netloc.contains('[');
        let close = netloc.contains(']');
        if open != close {
            return Err(UrlError);
        }
        if open && close {
            check_bracketed_netloc(&netloc)?;
        }
    }
    if let Some((before, after)) = url.split_once('#') {
        fragment = after.to_string();
        url = before.to_string();
    }
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

fn check_netloc(netloc: &str) -> Result<(), UrlError> {
    if netloc.is_ascii() {
        return Ok(());
    }
    let refused = netloc
        .chars()
        .filter(|c| !matches!(c, '@' | ':' | '#' | '?'))
        .any(|c| NFKC_DELIMITERS.binary_search(&u32::from(c)).is_ok());
    if refused { Err(UrlError) } else { Ok(()) }
}

fn check_bracketed_netloc(netloc: &str) -> Result<(), UrlError> {
    let host_and_port = rpartition(netloc, '@').2;
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
            .map_or(host_and_port, |(host, _)| host),
    };
    check_bracketed_host(hostname)
}

fn check_bracketed_host(hostname: &str) -> Result<(), UrlError> {
    if hostname.starts_with(['v', 'V']) {
        // \A[vV][a-fA-F0-9]+\..+\Z
        let rest = &hostname[1..];
        let hex = rest.bytes().take_while(u8::is_ascii_hexdigit).count();
        let after = &rest[hex..];
        let valid =
            hex > 0 && after.starts_with('.') && after.len() > 1 && !after[1..].contains('\n');
        return if valid { Ok(()) } else { Err(UrlError) };
    }
    match parse_ip(hostname) {
        Some(IpAddr::V6(_)) => Ok(()),
        _ => Err(UrlError),
    }
}

/// `ipaddress.ip_address(text)` for the forms it accepts: dotted-quad IPv4 (no leading zeros)
/// and IPv6, which may carry a `%zone`.
pub fn parse_ip(text: &str) -> Option<IpAddr> {
    if let Ok(v4) = text.parse::<std::net::Ipv4Addr>() {
        return Some(IpAddr::V4(v4));
    }
    let (address, zone) = text.split_once('%').unwrap_or((text, ""));
    if text.contains('%') && (zone.is_empty() || zone.contains('%')) {
        return None;
    }
    address.parse::<std::net::Ipv6Addr>().ok().map(IpAddr::V6)
}

/// `urlunsplit((scheme, netloc, path, query, fragment))` of `CPython` 3.11.15, the reference: a
/// `uses_netloc` scheme always gets its `//` and a leading `/` on the path (`file:///x.ts` for
/// the path `x.ts`); 3.13 leaves such a path bare (`file:x.ts`), which the clear-HLS reader
/// meets only when a reference climbs above the root of a `file:` base. A path that starts with
/// `//` keeps it on both (`file:////x`).
pub fn urlunsplit(scheme: &str, netloc: &str, path: &str, query: &str, fragment: &str) -> String {
    let mut url = path.to_string();
    if !netloc.is_empty()
        || (!scheme.is_empty() && USES_NETLOC.contains(&scheme))
        || url.starts_with("//")
    {
        if !url.is_empty() && !url.starts_with('/') {
            url.insert(0, '/');
        }
        url = format!("//{netloc}{url}");
    }
    if !scheme.is_empty() {
        url = format!("{scheme}:{url}");
    }
    if !query.is_empty() {
        url.push('?');
        url.push_str(query);
    }
    if !fragment.is_empty() {
        url.push('#');
        url.push_str(fragment);
    }
    url
}

/// `urlparse`: `urlsplit` plus the `;params` of the last path segment for the schemes that
/// use them.
fn urlparse(url: &str, scheme: &str) -> Result<(SplitResult, String), UrlError> {
    let mut parts = urlsplit(url, scheme)?;
    let mut params = String::new();
    if USES_PARAMS.contains(&parts.scheme.as_str()) && parts.path.contains(';') {
        let path = std::mem::take(&mut parts.path);
        let index = match path.rfind('/') {
            Some(slash) => path[slash..].find(';').map(|offset| slash + offset),
            None => path.find(';'),
        };
        match index {
            Some(index) => {
                params = path[index + 1..].to_string();
                parts.path = path[..index].to_string();
            }
            None => parts.path = path,
        }
    }
    Ok((parts, params))
}

fn urlunparse(parts: &SplitResult, params: &str) -> String {
    let path = if params.is_empty() {
        parts.path.clone()
    } else {
        format!("{};{params}", parts.path)
    };
    urlunsplit(
        &parts.scheme,
        &parts.netloc,
        &path,
        &parts.query,
        &parts.fragment,
    )
}

/// `urljoin(base, url)`.
pub fn urljoin(base: &str, url: &str) -> Result<String, UrlError> {
    if base.is_empty() {
        return Ok(url.to_string());
    }
    if url.is_empty() {
        return Ok(base.to_string());
    }
    let (b, bparams) = urlparse(base, "")?;
    let (mut parts, mut params) = urlparse(url, &b.scheme)?;
    if parts.scheme != b.scheme || !USES_RELATIVE.contains(&parts.scheme.as_str()) {
        return Ok(url.to_string());
    }
    if USES_NETLOC.contains(&parts.scheme.as_str()) {
        if !parts.netloc.is_empty() {
            return Ok(urlunparse(&parts, &params));
        }
        parts.netloc.clone_from(&b.netloc);
    }
    if parts.path.is_empty() && params.is_empty() {
        parts.path.clone_from(&b.path);
        params = bparams;
        if parts.query.is_empty() {
            parts.query.clone_from(&b.query);
        }
        return Ok(urlunparse(&parts, &params));
    }
    let mut base_parts: Vec<&str> = b.path.split('/').collect();
    if base_parts.last() != Some(&"") {
        base_parts.pop();
    }
    let segments: Vec<&str> = if parts.path.starts_with('/') {
        parts.path.split('/').collect()
    } else {
        let mut segments: Vec<&str> = base_parts;
        segments.extend(parts.path.split('/'));
        if segments.len() > 2 {
            let last = segments.len() - 1;
            let middle: Vec<&str> = segments[1..last]
                .iter()
                .copied()
                .filter(|segment| !segment.is_empty())
                .collect();
            let mut filtered = vec![segments[0]];
            filtered.extend(middle);
            filtered.push(segments[last]);
            filtered
        } else {
            segments
        }
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
    parts.path = if joined.is_empty() {
        "/".to_string()
    } else {
        joined
    };
    Ok(urlunparse(&parts, &params))
}

/// `unquote(text)`: `%XX` escapes decoded within each ASCII run, as UTF-8 with invalid
/// sequences replaced; non-ASCII characters kept as they are.
pub fn unquote(text: &str) -> String {
    if !text.contains('%') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut run_start: Option<usize> = None;
    for (index, c) in text.char_indices() {
        if c.is_ascii() {
            run_start.get_or_insert(index);
        } else {
            if let Some(start) = run_start.take() {
                out.push_str(&unquote_ascii(&text[start..index]));
            }
            out.push(c);
        }
    }
    if let Some(start) = run_start {
        out.push_str(&unquote_ascii(&text[start..]));
    }
    out
}

fn unquote_ascii(run: &str) -> String {
    let bytes = run.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
            && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit)
        {
            let pair = std::str::from_utf8(&bytes[index + 1..index + 3]).expect("ASCII");
            decoded.push(u8::from_str_radix(pair, 16).expect("two hex digits"));
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

/// `quote_from_bytes(data, safe="/")`: ASCII letters, digits, `_.-~` and `/` as they are,
/// every other byte as `%XX` (upper-case hex).
pub fn quote_from_bytes(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len());
    for byte in data {
        if byte.is_ascii_alphanumeric() || b"_.-~/".contains(byte) {
            out.push(char::from(*byte));
        } else {
            write!(out, "%{byte:02X}").expect("String never fails to write");
        }
    }
    out
}

/// `int(match)` of a `\d+` match (digits of any script; `None` when `text` is not one);
/// saturating, as no length or offset this tool compares comes near `u128::MAX`.
pub fn digits_value(text: &str) -> Option<u128> {
    if text.is_empty() {
        return None;
    }
    let mut value: u128 = 0;
    for c in text.chars() {
        value = value
            .saturating_mul(10)
            .saturating_add(u128::from(decimal_digit(c)?));
    }
    Some(value)
}

#[cfg(test)]
mod urlunsplit_tests {
    use super::urlunsplit;

    /// Measured on `CPython` 3.11.15 (the reference) and 3.13.16: they agree except on the first
    /// case; the `itms-services` case agrees by accident of 3.13's algorithm.
    /// case, where 3.13 prints `file:x.ts`.
    #[test]
    fn urlunsplit_is_cpython_3_11s() {
        assert_eq!(urlunsplit("file", "", "x.ts", "", ""), "file:///x.ts");
        assert_eq!(urlunsplit("file", "", "//x", "", ""), "file:////x");
        assert_eq!(urlunsplit("http", "", "/p", "", ""), "http:///p");
        assert_eq!(urlunsplit("mailto", "", "a@b", "", ""), "mailto:a@b");
        // `itms-services` joined `uses_netloc` in 3.12; 3.11.15 has 26 entries without it
        assert_eq!(
            urlunsplit("itms-services", "", "x", "", ""),
            "itms-services:x"
        );
        assert_eq!(urlunsplit("file", "", "", "", ""), "file://");
        assert_eq!(urlunsplit("http", "h", "p", "q", "f"), "http://h/p?q#f");
    }
}
