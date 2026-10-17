//! HTTP responses, conditional requests, range handling and file metadata.

use crate::pyutil;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// A request as the handlers see it: the method, the percent-decoded path, the query
/// arguments, the headers and (for the routes that read it) the body.
#[derive(Clone, Debug, Default)]
pub struct Req {
    /// `GET`, `POST`, …
    pub method: String,
    /// `request.path` (decoded).
    pub path: String,
    /// `request.args` in order.
    pub args: Vec<(String, String)>,
    /// The headers, names lower-cased, in order.
    pub headers: Vec<(String, String)>,
    /// The body, when the route reads it.
    pub body: Vec<u8>,
}

impl Req {
    /// The first value of a header (case-insensitive name).
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.as_str())
    }

    /// `request.args.get(name)`.
    pub fn arg(&self, name: &str) -> Option<&str> {
        self.args
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// `request.mimetype`: the media type of `Content-Type`, lower-cased, without parameters.
    pub fn mimetype(&self) -> String {
        self.header("content-type")
            .unwrap_or_default()
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
    }

    /// `request.is_json`: `application/json` or `application/*+json`.
    pub fn is_json(&self) -> bool {
        let mimetype = self.mimetype();
        mimetype == "application/json"
            || (mimetype.starts_with("application/") && mimetype.ends_with("+json"))
    }

    /// `request.content_length` (a non-negative integer header, else `None`).
    pub fn content_length(&self) -> Option<u64> {
        let value = self.header("content-length")?.trim();
        (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| value.parse().ok())
            .flatten()
    }
}

/// A response body.
#[derive(Debug)]
pub enum Body {
    /// Bytes in memory.
    Bytes(Vec<u8>),
    /// A slice of a file, read when the response is sent.
    File {
        /// The file.
        path: PathBuf,
        /// The first byte.
        start: u64,
        /// The number of bytes.
        length: u64,
    },
    /// A stream (Server-Sent Events, the log download): chunks as they are produced.
    Stream(tokio::sync::mpsc::Receiver<Result<Vec<u8>, std::io::Error>>),
}

#[derive(Debug)]
pub struct Reply {
    /// The status code.
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// The body.
    pub body: Body,
}

impl Reply {
    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Reply {
        Reply {
            status,
            headers: vec![
                ("Content-Type".into(), content_type.into()),
                ("Content-Length".into(), body.len().to_string()),
            ],
            body: Body::Bytes(body),
        }
    }

    /// `jsonify(value)` with this status.
    pub fn json(status: u16, value: &Value) -> Reply {
        Reply::bytes(status, "application/json", pyutil::jsonify(value))
    }

    /// `jsonify({"error": message})`.
    pub fn json_error(status: u16, message: &str) -> Reply {
        let mut map = Map::new();
        map.insert("error".into(), Value::from(message));
        Reply::json(status, &Value::Object(map))
    }

    /// `Response(status=204)`: no body, no `Content-Length`.
    pub fn no_content() -> Reply {
        Reply {
            status: 204,
            headers: vec![("Content-Type".into(), "text/html; charset=utf-8".into())],
            body: Body::Bytes(Vec::new()),
        }
    }

    /// A streamed response of this content type (no `Content-Length`).
    pub fn stream(
        content_type: &str,
        receiver: tokio::sync::mpsc::Receiver<Result<Vec<u8>, std::io::Error>>,
    ) -> Reply {
        Reply {
            status: 200,
            headers: vec![("Content-Type".into(), content_type.into())],
            body: Body::Stream(receiver),
        }
    }

    /// Set a header, replacing every earlier value (`response.headers[name] = value`).
    pub fn set_header(&mut self, name: &str, value: &str) {
        self.headers
            .retain(|(key, _)| !key.eq_ignore_ascii_case(name));
        self.headers.push((name.to_string(), value.to_string()));
    }

    /// A header's value.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// `response.content_length` as `after_request` sees it.
    pub fn content_length(&self) -> Option<u64> {
        self.header("Content-Length")
            .and_then(|value| value.parse().ok())
    }
}

/// `werkzeug.utils.get_content_type(mimetype, "utf-8")`: a charset for `text/*`, the known
/// text-like application types and `*+xml`.
pub fn content_type(mimetype: &str) -> String {
    const CHARSET_TYPES: [&str; 6] = [
        "application/ecmascript",
        "application/javascript",
        "application/sql",
        "application/xml",
        "application/xml-dtd",
        "application/xml-external-parsed-entity",
    ];
    if mimetype.starts_with("text/")
        || CHARSET_TYPES.contains(&mimetype)
        || mimetype.ends_with("+xml")
    {
        format!("{mimetype}; charset=utf-8")
    } else {
        mimetype.to_string()
    }
}

/// An `HTTPException`'s HTML page.
pub fn html_error(status: u16) -> Reply {
    let (name, description) = match status {
        400 => (
            "Bad Request",
            "The browser (or proxy) sent a request that this server could not understand.",
        ),
        404 => (
            "Not Found",
            "The requested URL was not found on the server. If you entered the URL manually please check your spelling and try again.",
        ),
        405 => (
            "Method Not Allowed",
            "The method is not allowed for the requested URL.",
        ),
        413 => (
            "Request Entity Too Large",
            "The data value transmitted exceeds the capacity limit.",
        ),
        416 => (
            "Requested Range Not Satisfiable",
            "The server cannot provide the requested range.",
        ),
        _ => (
            "Internal Server Error",
            "The server encountered an internal error and was unable to complete your request. Either the server is overloaded or there is an error in the application.",
        ),
    };
    let status = if matches!(status, 400 | 404 | 405 | 413 | 416) {
        status
    } else {
        500
    };
    let body = format!(
        "<!doctype html>\n<html lang=en>\n<title>{status} {name}</title>\n<h1>{name}</h1>\n<p>{description}</p>\n"
    );
    Reply::bytes(status, "text/html; charset=utf-8", body.into_bytes())
}

/// `405 Method Not Allowed` with its `Allow` header.
pub fn method_not_allowed(allowed: &[&str]) -> Reply {
    let mut reply = html_error(405);
    reply.set_header("Allow", &allowed.join(", "));
    reply
}

/// The automatic `OPTIONS` answer: `200`, empty, `Allow`.
pub fn options(allowed: &[&str]) -> Reply {
    Reply {
        status: 200,
        headers: vec![
            ("Content-Type".into(), "text/html; charset=utf-8".into()),
            ("Allow".into(), allowed.join(", ")),
        ],
        body: Body::Bytes(Vec::new()),
    }
}

pub fn redirect(location: &str) -> Reply {
    let escaped = html_escape(location);
    let body = format!(
        "<!doctype html>\n<html lang=en>\n<title>Redirecting...</title>\n<h1>Redirecting...</h1>\n<p>You should be redirected automatically to the target URL: <a href=\"{escaped}\">{escaped}</a>. If not, click the link.\n"
    );
    let mut reply = Reply::bytes(308, "text/html; charset=utf-8", body.into_bytes());
    reply.set_header("Location", location);
    reply
}

/// `markupsafe.escape`.
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&#34;")
        .replace('\'', "&#39;")
}

/// What `send_file` needs to know about the file it sends.
#[derive(Clone, Debug)]
pub struct FileFacts {
    /// `os.path.abspath(path)`: names the file in `Content-Disposition` and the `ETag`.
    pub path: PathBuf,
    /// Where the bytes come from (the same file, or an embedded copy).
    pub source: FileSource,
    /// The size in bytes.
    pub size: u64,
    pub mtime: f64,
}

/// Where a sent file's bytes are.
#[derive(Clone, Debug)]
pub enum FileSource {
    /// On disk.
    Disk(PathBuf),
    /// Embedded in the binary.
    Embedded(&'static [u8]),
}

impl FileFacts {
    /// `os.stat(path)` of a file on disk.
    pub fn stat(path: &Path) -> std::io::Result<FileFacts> {
        let metadata = std::fs::metadata(path)?;
        Ok(FileFacts {
            path: pyutil::abspath(path),
            source: FileSource::Disk(path.to_path_buf()),
            size: metadata.len(),
            mtime: st_mtime(&metadata),
        })
    }
}

#[allow(clippy::cast_precision_loss)] // reason: CPython converts the timespec to a double the same way
fn st_mtime(metadata: &std::fs::Metadata) -> f64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mtime() as f64 + metadata.mtime_nsec() as f64 * 1e-9
    }
    #[cfg(not(unix))]
    {
        metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0.0, |elapsed| elapsed.as_secs_f64())
    }
}

fn content_disposition(name: &str) -> String {
    const TOKEN: &str =
        "!#$%&'*+-.0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ^_`abcdefghijklmnopqrstuvwxyz|~";
    let quote = |value: &str| {
        if value.is_empty() {
            return "\"\"".to_string();
        }
        if value.chars().all(|c| TOKEN.contains(c)) {
            return value.to_string();
        }
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    };
    if name.is_ascii() {
        format!("inline; filename={}", quote(name))
    } else {
        // NFKD then ASCII only: this implementation drops the non-ASCII characters without decomposing.
        let simple: String = name.chars().filter(char::is_ascii).collect();
        let encoded = pyutil::quote(name, "!#$&+-.^_`|~");
        format!(
            "inline; filename={}; filename*={}",
            quote(&simple),
            quote(&format!("UTF-8''{encoded}"))
        )
    }
}

pub fn send_file(req: &Req, facts: &FileFacts, mimetype: &str) -> Reply {
    let now = pyutil::split_timestamp(pyutil::time_now()).0;
    let name = facts
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (mtime_seconds, _) = pyutil::split_timestamp(facts.mtime);
    let last_modified = pyutil::http_date(mtime_seconds);
    let etag_text = format!(
        "{}-{}-{}",
        drmr_core::canonical::python_float_repr(facts.mtime),
        facts.size,
        pyutil::adler32(pyutil::path_text(&facts.path).as_bytes())
    );
    let mut headers = vec![
        (
            "Content-Disposition".to_string(),
            content_disposition(&name),
        ),
        ("Content-Type".to_string(), content_type(mimetype)),
        ("Content-Length".to_string(), facts.size.to_string()),
        ("Last-Modified".to_string(), last_modified.clone()),
        (
            "Cache-Control".to_string(),
            "no-cache, max-age=0".to_string(),
        ),
        ("Expires".to_string(), pyutil::http_date(now)),
        ("ETag".to_string(), format!("\"{etag_text}\"")),
        ("Date".to_string(), pyutil::http_date(now)),
        ("Accept-Ranges".to_string(), "bytes".to_string()),
    ];
    let mut status = 200;
    let (mut start, mut length) = (0, facts.size);
    if facts.size > 0 && range_processable(req, &etag_text, &last_modified) {
        let range = req
            .header("range")
            .and_then(|value| byte_range(value, facts.size));
        let Some((first, end)) = range else {
            let mut reply = html_error(416);
            reply.set_header("Content-Range", &format!("bytes */{}", facts.size));
            return reply;
        };
        status = 206;
        start = first;
        length = end - first;
        set(&mut headers, "Content-Length", &length.to_string());
        headers.push((
            "Content-Range".to_string(),
            format!("bytes {first}-{}/{}", end - 1, facts.size),
        ));
    } else if !resource_modified(req, &etag_text, &last_modified, false) {
        if etags(req.header("if-match")).any() {
            status = 412;
        } else {
            status = 304;
        }
    }
    let body = if status == 304 || req.method == "HEAD" {
        Body::Bytes(Vec::new())
    } else {
        match &facts.source {
            FileSource::Disk(path) => Body::File {
                path: path.clone(),
                start,
                length,
            },
            FileSource::Embedded(data) => {
                let first = usize::try_from(start).unwrap_or(0);
                let count = usize::try_from(length).unwrap_or(0);
                Body::Bytes(data[first..first + count].to_vec())
            }
        }
    };
    Reply {
        status,
        headers,
        body,
    }
}

fn set(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    if let Some(entry) = headers
        .iter_mut()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
    {
        entry.1 = value.to_string();
    } else {
        headers.push((name.to_string(), value.to_string()));
    }
}

/// `_is_range_request_processable`: a `Range` header, and no `If-Range` or one the resource
/// still satisfies.
fn range_processable(req: &Req, etag: &str, last_modified: &str) -> bool {
    req.header("range").is_some()
        && (req.header("if-range").is_none() || !resource_modified(req, etag, last_modified, true))
}

/// `parse_range_header(value).range_for_length(length)`: one satisfiable `bytes` range as
/// `(start, end)` (end exclusive).
fn byte_range(value: &str, length: u64) -> Option<(u64, u64)> {
    let (units, ranges) = value.split_once('=')?;
    let units = units.trim_matches([' ', '\t']).to_ascii_lowercase();
    let mut parsed: Vec<(i128, Option<i128>)> = Vec::new();
    let mut last_end: i128 = 0;
    for item in ranges.split(',') {
        let item = item.trim_matches([' ', '\t']);
        if !item.contains('-') {
            return None;
        }
        if item.starts_with('-') {
            if last_end < 0 {
                return None;
            }
            let begin = plain_int(item)?;
            if begin == 0 {
                return None;
            }
            parsed.push((begin, None));
            last_end = -1;
        } else {
            let (begin, end) = item.split_once('-')?;
            let begin = plain_int(begin.trim_matches([' ', '\t']))?;
            if begin < last_end || last_end < 0 {
                return None;
            }
            let end_text = end.trim_matches([' ', '\t']);
            let end = if end_text.is_empty() {
                None
            } else {
                let end = plain_int(end_text)? + 1;
                if begin >= end {
                    return None;
                }
                Some(end)
            };
            last_end = end.unwrap_or(-1);
            parsed.push((begin, end));
        }
    }
    if units != "bytes" || parsed.len() != 1 {
        return None;
    }
    let length = i128::from(length);
    let (mut start, end) = parsed[0];
    let end = if let Some(end) = end {
        end
    } else {
        // A suffix range: `start` is negative, counted from the end.
        if start < 0 {
            start += length;
        }
        length
    };
    if start >= end || !(0..length).contains(&start) {
        return None;
    }
    let end = end.min(length);
    Some((u64::try_from(start).ok()?, u64::try_from(end).ok()?))
}

/// `_plain_int`: `-?[0-9]+` after stripping spaces and tabs.
fn plain_int(value: &str) -> Option<i128> {
    let value = value.trim_matches([' ', '\t']);
    let digits = value.strip_prefix('-').unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(value.parse().unwrap_or(if value.starts_with('-') {
        i128::MIN
    } else {
        i128::MAX
    }))
}

/// A parsed `If-None-Match` / `If-Match` value.
struct ETags {
    star: bool,
    strong: Vec<String>,
    weak: Vec<String>,
}

impl ETags {
    fn any(&self) -> bool {
        self.star || !self.strong.is_empty() || !self.weak.is_empty()
    }
}

/// `parse_etags(value)`: `[ \t]*(W/)?("…"|token)[ \t]*(,|$)` repeatedly, an invalid item
/// skipped up to the next comma.
fn etags(value: Option<&str>) -> ETags {
    let mut tags = ETags {
        star: false,
        strong: Vec::new(),
        weak: Vec::new(),
    };
    let Some(value) = value.filter(|value| !value.is_empty()) else {
        return tags;
    };
    if value == "*" {
        tags.star = true;
        return tags;
    }
    let bytes = value.as_bytes();
    let mut pos = 0;
    while pos <= bytes.len() {
        match etag_item(value, pos) {
            Some((weak, tag, end)) => {
                if weak {
                    tags.weak.push(tag);
                } else {
                    tags.strong.push(tag);
                }
                pos = end;
                if pos >= bytes.len() {
                    break;
                }
            }
            None => match value[pos..].find(',') {
                Some(offset) => pos += offset + 1,
                None => break,
            },
        }
    }
    tags
}

/// One `_etag_re` match at `pos`: `(weak, tag, position after the comma)`.
fn etag_item(value: &str, mut pos: usize) -> Option<(bool, String, usize)> {
    let bytes = value.as_bytes();
    let skip = |pos: &mut usize| {
        while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t') {
            *pos += 1;
        }
    };
    skip(&mut pos);
    let weak = matches!(bytes.get(pos..pos + 2), Some(b"W/" | b"w/"));
    if weak {
        pos += 2;
    }
    let tag = if bytes.get(pos) == Some(&b'"') {
        let end = pos + 1 + value[pos + 1..].find('"')?;
        let tag = value[pos + 1..end].to_string();
        pos = end + 1;
        tag
    } else {
        let start = pos;
        while pos < bytes.len() && !matches!(bytes[pos], b'"' | b' ' | b'\t' | b',') {
            pos += 1;
        }
        if pos == start {
            return None;
        }
        value[start..pos].to_string()
    };
    skip(&mut pos);
    match bytes.get(pos) {
        None => Some((weak, tag, pos)),
        Some(b',') => Some((weak, tag, pos + 1)),
        Some(_) => None,
    }
}

/// `unquote_etag(value)[0]`.
fn unquote_etag(value: &str) -> &str {
    let rest = value
        .strip_prefix("W/")
        .or_else(|| value.strip_prefix("w/"))
        .unwrap_or(value);
    if rest.starts_with('"') && rest.ends_with('"') {
        if rest.len() >= 2 {
            &rest[1..rest.len() - 1]
        } else {
            ""
        }
    } else {
        rest
    }
}

/// `is_resource_modified(environ, etag, last_modified=…, ignore_if_range=…)`.
fn resource_modified(req: &Req, etag: &str, last_modified: &str, use_if_range: bool) -> bool {
    let last_modified = pyutil::parse_http_date(last_modified);
    let mut unmodified = false;
    let if_range = if use_if_range && req.header("range").is_some() {
        req.header("if-range")
    } else {
        None
    };
    let if_range_date = if_range.and_then(pyutil::parse_http_date);
    let modified_since = match if_range {
        Some(_) if if_range_date.is_some() => if_range_date,
        _ => req
            .header("if-modified-since")
            .and_then(pyutil::parse_http_date),
    };
    if let (Some(since), Some(modified)) = (modified_since, last_modified)
        && modified <= since
    {
        unmodified = true;
    }
    match if_range {
        // `parse_if_range_header("")` has neither a date nor an etag.
        Some(value) if !value.is_empty() && if_range_date.is_none() => {
            unmodified = unquote_etag(value) == etag;
        }
        _ => {
            let none_match = etags(req.header("if-none-match"));
            if none_match.any() {
                unmodified = none_match.star
                    || none_match.strong.iter().any(|tag| tag == etag)
                    || none_match.weak.iter().any(|tag| tag == etag);
            }
            let if_match = etags(req.header("if-match"));
            if if_match.any() {
                unmodified = !if_match.strong.iter().any(|tag| tag == etag);
            }
        }
    }
    !unmodified
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_follow_werkzeug() {
        assert_eq!(byte_range("bytes=2-5", 16), Some((2, 6)));
        assert_eq!(byte_range("bytes=-3", 16), Some((13, 16)));
        assert_eq!(
            byte_range("bytes=-20", 16),
            None,
            "a suffix longer than the file is 416"
        );
        assert_eq!(byte_range("bytes=20-30", 16), None);
        assert_eq!(byte_range("bytes=0-1,4-5", 16), None, "one range only");
        assert_eq!(byte_range("bytes=10-", 16), Some((10, 16)));
        assert_eq!(byte_range("bytes=0-100", 16), Some((0, 16)));
        assert_eq!(byte_range("items=0-1", 16), None);
        assert_eq!(byte_range("bytes=-0", 16), None);
    }

    #[test]
    fn content_types_and_dispositions() {
        assert_eq!(
            content_type("application/dash+xml"),
            "application/dash+xml; charset=utf-8"
        );
        assert_eq!(content_type("video/mp4"), "video/mp4");
        assert_eq!(content_type("text/vtt"), "text/vtt; charset=utf-8");
        assert_eq!(content_disposition("init.mp4"), "inline; filename=init.mp4");
        assert_eq!(
            content_disposition("a b.mp4"),
            "inline; filename=\"a b.mp4\""
        );
    }
}
