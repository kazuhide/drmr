//! The part of `http.client` the fetcher relies on (`CPython` 3.13): the request it writes
//! (`GET` over HTTP/1.1, `Host` first — with the implementation unless it is 80, as `HTTPConnection`
//! writes it for every scheme — then the caller's headers in order), the response it parses
//! (status line, interim `100` responses, headers as the `email` parser's compat32 policy reads
//! them), the body framing (`Content-Length`, `chunked`, or until the connection closes), the
//! keep-alive decision (`will_close`) and `read1`'s at-most-one-read semantics over a
//! `BufferedReader` of 8 KiB. Each response reads through its own buffer, as each
//! `HTTPResponse` reads through its own `makefile`.

use super::platform::Stream;
use super::pytext::{is_space, py_int};
use std::io;
use std::time::Duration;

const MAX_LINE: usize = 65_536;
const MAX_HEADERS: usize = 100;
const MAX_INTERIM_RESPONSES: usize = 100;
const BUFFER_SIZE: usize = 8192;

/// Why a request or a response failed, by the Python exception class the fetcher tells apart.
#[derive(Debug)]
pub enum HttpError {
    /// An `OSError`: a socket error or a timeout.
    Io(io::Error),
    /// `RemoteDisconnected`: the connection closed before a status line.
    RemoteDisconnected,
    /// Another `HTTPException` (`BadStatusLine`, `LineTooLong`, `IncompleteRead`,
    /// `UnknownProtocol`, too many headers, interim responses or trailers).
    Protocol,
    /// A `ValueError` from building the request (a control character or a non-ASCII character
    /// in the target, a header value that is not Latin-1 or holds a bare line break).
    Invalid,
}

impl From<io::Error> for HttpError {
    fn from(error: io::Error) -> Self {
        HttpError::Io(error)
    }
}

impl HttpError {
    /// Whether this is how a kept-alive connection the server had already closed shows before
    /// any response (`RemoteDisconnected`, `ConnectionResetError`, `BrokenPipeError`): only
    /// then is a retry free.
    pub fn is_dropped(&self) -> bool {
        match self {
            HttpError::RemoteDisconnected => true,
            HttpError::Io(error) => matches!(
                error.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
            ),
            HttpError::Protocol | HttpError::Invalid => false,
        }
    }
}

/// The `Host` header `HTTPConnection(host, port)` writes: brackets around an IPv6 address
/// (its zone removed), the implementation unless it is 80 (`default_port`, whatever the scheme).
pub fn host_header(host: &str, port: u16) -> String {
    let mut encoded = host.to_string();
    if encoded.contains(':') && !encoded.starts_with('[') {
        encoded = format!("[{encoded}]");
    }
    if host.contains(':')
        && let Some((address, _zone)) = encoded.split_once('%')
    {
        encoded = format!("{address}]");
    }
    if port == 80 {
        encoded
    } else {
        format!("{encoded}:{port}")
    }
}

/// Write `GET target HTTP/1.1`, `Host`, the headers in order and the blank line, in one write.
pub fn send_request(
    stream: &mut dyn Stream,
    host: &str,
    target: &str,
    headers: &[(String, String)],
) -> Result<(), HttpError> {
    let target = if target.is_empty() { "/" } else { target };
    if target
        .chars()
        .any(|c| c <= ' ' || c == '\u{7f}' || !c.is_ascii())
    {
        return Err(HttpError::Invalid);
    }
    let mut message: Vec<u8> = format!("GET {target} HTTP/1.1\r\nHost: {host}\r\n").into_bytes();
    for (name, value) in headers {
        let legal_name = !name.is_empty()
            && name.is_ascii()
            && !name.starts_with(|c: char| c == ':' || c == '\u{b}' || c.is_ascii_whitespace())
            && !name.contains([':', '\r', '\n']);
        if !legal_name {
            return Err(HttpError::Invalid);
        }
        let mut encoded = Vec::with_capacity(value.len());
        for c in value.chars() {
            encoded.push(u8::try_from(u32::from(c)).map_err(|_| HttpError::Invalid)?);
        }
        if illegal_header_value(&encoded) {
            return Err(HttpError::Invalid);
        }
        message.extend_from_slice(name.as_bytes());
        message.extend_from_slice(b": ");
        message.extend_from_slice(&encoded);
        message.extend_from_slice(b"\r\n");
    }
    message.extend_from_slice(b"\r\n");
    stream.write_all(&message)?;
    stream.flush()?;
    Ok(())
}

/// `\n(?![ \t])|\r(?![ \t\n])`.
fn illegal_header_value(value: &[u8]) -> bool {
    value.iter().enumerate().any(|(index, byte)| {
        let next = value.get(index + 1);
        match byte {
            b'\n' => !matches!(next, Some(b' ' | b'\t')),
            b'\r' => !matches!(next, Some(b' ' | b'\t' | b'\n')),
            _ => false,
        }
    })
}

/// `CPython`'s `BufferedReader` over the socket, as far as the response reads it.
struct Buffered<'s> {
    stream: &'s mut dyn Stream,
    buffer: Vec<u8>,
    position: usize,
}

impl Buffered<'_> {
    fn buffered(&self) -> &[u8] {
        &self.buffer[self.position..]
    }

    /// One raw read into an empty buffer; `false` at end of stream.
    fn fill(&mut self) -> io::Result<bool> {
        self.buffer.resize(BUFFER_SIZE, 0);
        let count = read_retrying(self.stream, &mut self.buffer)?;
        self.buffer.truncate(count);
        self.position = 0;
        Ok(count > 0)
    }

    /// `readline(limit)`: up to and including `\n`, at most `limit` bytes, shorter at EOF.
    fn readline(&mut self, limit: usize) -> io::Result<Vec<u8>> {
        let mut line = Vec::new();
        loop {
            if self.buffered().is_empty() && !self.fill()? {
                return Ok(line);
            }
            let room = limit - line.len();
            let available = self.buffered();
            let window = &available[..available.len().min(room)];
            if let Some(newline) = window.iter().position(|byte| *byte == b'\n') {
                line.extend_from_slice(&window[..=newline]);
                self.position += newline + 1;
                return Ok(line);
            }
            let taken = window.len();
            line.extend_from_slice(window);
            self.position += taken;
            if line.len() >= limit {
                return Ok(line);
            }
        }
    }

    /// `read1(n)`: buffered bytes when there are any, else at most one raw read of `n` bytes.
    fn read1(&mut self, n: usize) -> io::Result<Vec<u8>> {
        if n == 0 {
            return Ok(Vec::new());
        }
        if !self.buffered().is_empty() {
            let take = n.min(self.buffered().len());
            let out = self.buffered()[..take].to_vec();
            self.position += take;
            return Ok(out);
        }
        let mut out = vec![0; n];
        let count = read_retrying(self.stream, &mut out)?;
        out.truncate(count);
        Ok(out)
    }

    /// `read(n)`: `n` bytes, fewer only at EOF.
    fn read(&mut self, n: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            if self.buffered().is_empty() && !self.fill()? {
                break;
            }
            let take = (n - out.len()).min(self.buffered().len());
            out.extend_from_slice(&self.buffered()[..take]);
            self.position += take;
        }
        Ok(out)
    }
}

fn read_retrying(stream: &mut dyn Stream, buffer: &mut [u8]) -> io::Result<usize> {
    loop {
        match stream.read(buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

/// A response being read (`HTTPResponse` after `begin`).
pub struct Response<'s> {
    reader: Buffered<'s>,
    /// The status code.
    pub status: u16,
    headers: Vec<(String, String)>,
    chunked: bool,
    chunk_left: Option<i128>,
    length: Option<u128>,
    /// `will_close`: the connection cannot serve another request.
    pub will_close: bool,
    closed: bool,
}

impl<'s> Response<'s> {
    /// `getresponse`: the status line (skipping interim `100` responses) and the headers.
    pub fn begin(stream: &'s mut dyn Stream) -> Result<Self, HttpError> {
        let mut reader = Buffered {
            stream,
            buffer: Vec::new(),
            position: 0,
        };
        let mut status_line = None;
        for _ in 0..MAX_INTERIM_RESPONSES {
            let (version, status) = read_status(&mut reader)?;
            if status != 100 {
                status_line = Some((version, status));
                break;
            }
            read_header_lines(&mut reader)?;
        }
        let (version, status) = status_line.ok_or(HttpError::Protocol)?;
        let version_11 = if version == "HTTP/1.0" || version == "HTTP/0.9" {
            false
        } else if version.starts_with("HTTP/1.") {
            true
        } else {
            return Err(HttpError::Protocol);
        };
        let lines = read_header_lines(&mut reader)?;
        let text: String = lines
            .concat()
            .iter()
            .map(|byte| char::from(*byte))
            .collect();
        let headers = parse_header_block(&text);
        let mut response = Response {
            reader,
            status,
            headers,
            chunked: false,
            chunk_left: None,
            length: None,
            will_close: false,
            closed: false,
        };
        response.chunked = response
            .first("transfer-encoding")
            .is_some_and(|value| !value.is_empty() && value.to_lowercase() == "chunked");
        response.will_close = response.check_close(version_11);
        if let Some(length) = response.first("content-length")
            && !length.is_empty()
            && !response.chunked
        {
            response.length = py_int(length).and_then(|value| u128::try_from(value).ok());
        }
        if status == 204 || status == 304 || (100..200).contains(&status) {
            response.length = Some(0);
        }
        if !response.will_close && !response.chunked && response.length.is_none() {
            response.will_close = true;
        }
        Ok(response)
    }

    fn first(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.to_lowercase() == name)
            .map(|(_, value)| value.as_str())
    }

    /// `getheader(name, default)`: every value of `name` joined with `", "`, else `default`.
    pub fn header(&self, name: &str, default: Option<&str>) -> Option<String> {
        let name = name.to_lowercase();
        let values: Vec<&str> = self
            .headers
            .iter()
            .filter(|(key, _)| key.to_lowercase() == name)
            .map(|(_, value)| value.as_str())
            .collect();
        if values.is_empty() {
            default.map(str::to_string)
        } else {
            Some(values.join(", "))
        }
    }

    fn check_close(&self, version_11: bool) -> bool {
        let connection = self.first("connection");
        if version_11 {
            return connection.is_some_and(|value| value.to_lowercase().contains("close"));
        }
        if self
            .first("keep-alive")
            .is_some_and(|value| !value.is_empty())
        {
            return false;
        }
        if connection.is_some_and(|value| value.to_lowercase().contains("keep-alive")) {
            return false;
        }
        if self
            .first("proxy-connection")
            .is_some_and(|value| value.to_lowercase().contains("keep-alive"))
        {
            return false;
        }
        true
    }

    /// `length` as `begin` left it: the declared body length (`None` for `chunked`, a
    /// missing or invalid `Content-Length`).
    pub fn declared_length(&self) -> Option<u128> {
        self.length
    }

    /// `isclosed`: the body was read to its end.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// `sock.settimeout(bound)` on the connection under this response.
    pub fn set_read_timeout(&self, bound: Duration) -> io::Result<()> {
        self.reader.stream.set_read_timeout(Some(bound))
    }

    /// `read1(n)`: at most `n` bytes with at most one read beyond the buffer; empty at the end.
    pub fn read1(&mut self, n: usize) -> Result<Vec<u8>, HttpError> {
        if self.closed {
            return Ok(Vec::new());
        }
        if self.chunked {
            return self.read1_chunked(n);
        }
        let n = match self.length {
            Some(length) => usize::try_from(length).map_or(n, |length| n.min(length)),
            None => n,
        };
        let result = self.reader.read1(n)?;
        if result.is_empty() && n > 0 {
            self.closed = true;
        } else if let Some(length) = self.length.as_mut() {
            *length -= result.len() as u128;
            if *length == 0 {
                self.closed = true;
            }
        }
        Ok(result)
    }

    fn read1_chunked(&mut self, n: usize) -> Result<Vec<u8>, HttpError> {
        let Some(chunk_left) = self.chunk_left_or_next()? else {
            return Ok(Vec::new());
        };
        if n == 0 {
            return Ok(Vec::new());
        }
        // `if not (0 <= n <= chunk_left): n = chunk_left`. A negative size (which `int(line,
        // 16)` accepts) makes it `fp.read1(negative)`: up to the buffer's size, and the chunk
        // never ends, so the rest of the stream is body until EOF (`IncompleteRead`).
        let n = if chunk_left < 0 {
            BUFFER_SIZE
        } else {
            usize::try_from(chunk_left).map_or(n, |left| n.min(left))
        };
        let read = self.reader.read1(n)?;
        self.chunk_left = Some(chunk_left.saturating_sub(read.len() as i128));
        if read.is_empty() {
            return Err(HttpError::Protocol);
        }
        Ok(read)
    }

    /// `_get_chunk_left`: the bytes left in the current chunk, reading the next chunk's size
    /// when the current one is done; `None` after the last chunk (the trailer is read and the
    /// body is closed).
    fn chunk_left_or_next(&mut self) -> Result<Option<i128>, HttpError> {
        if let Some(left) = self.chunk_left
            && left != 0
        {
            return Ok(Some(left));
        }
        if self.closed {
            return Ok(None);
        }
        if self.chunk_left == Some(0) {
            let crlf = self.reader.read(2)?;
            if crlf.len() < 2 {
                return Err(HttpError::Protocol);
            }
        }
        let line = self.reader.readline(MAX_LINE + 1)?;
        if line.len() > MAX_LINE {
            return Err(HttpError::Protocol);
        }
        let size = chunk_size(&line).ok_or_else(|| {
            self.closed = true;
            HttpError::Protocol
        })?;
        if size == 0 {
            self.discard_trailer()?;
            self.closed = true;
            self.chunk_left = None;
            return Ok(None);
        }
        self.chunk_left = Some(size);
        Ok(Some(size))
    }

    fn discard_trailer(&mut self) -> Result<(), HttpError> {
        let mut trailers = 0;
        loop {
            let line = self.reader.readline(MAX_LINE + 1)?;
            if line.len() > MAX_LINE {
                return Err(HttpError::Protocol);
            }
            if line.is_empty() || line == b"\r\n" || line == b"\n" {
                return Ok(());
            }
            trailers += 1;
            if trailers > MAX_HEADERS {
                return Err(HttpError::Protocol);
            }
        }
    }
}

/// `int(line[:line.find(b";")], 16)`: ASCII whitespace around, an optional sign, an optional
/// `0x`, hex digits with single underscores between them.
fn chunk_size(line: &[u8]) -> Option<i128> {
    let end = line
        .iter()
        .position(|byte| *byte == b';')
        .unwrap_or(line.len());
    let text = std::str::from_utf8(&line[..end]).ok()?;
    let text = text.trim_matches(|c: char| c.is_ascii_whitespace() || c == '\u{b}');
    let (negative, text) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let text = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .map_or(text, |rest| rest.strip_prefix('_').unwrap_or(rest));
    if text.is_empty() || text.starts_with('_') || text.ends_with('_') || text.contains("__") {
        return None;
    }
    let mut value: i128 = 0;
    for c in text.chars() {
        if c == '_' {
            continue;
        }
        let digit = i128::from(c.to_digit(16)?);
        value = value.saturating_mul(16).saturating_add(digit);
    }
    Some(if negative { -value } else { value })
}

/// `_read_status`: the version and the status code of the next status line.
fn read_status(reader: &mut Buffered<'_>) -> Result<(String, u16), HttpError> {
    let raw = reader.readline(MAX_LINE + 1)?;
    if raw.len() > MAX_LINE {
        return Err(HttpError::Protocol);
    }
    if raw.is_empty() {
        return Err(HttpError::RemoteDisconnected);
    }
    let line: String = raw.iter().map(|byte| char::from(*byte)).collect();
    let words: Vec<&str> = line
        .split(is_space)
        .filter(|word| !word.is_empty())
        .collect();
    let (version, status) = match words.as_slice() {
        [version, status, ..] => (*version, *status),
        _ => return Err(HttpError::Protocol),
    };
    if !version.starts_with("HTTP/") {
        return Err(HttpError::Protocol);
    }
    let status = py_int(status)
        .filter(|code| (100..=999).contains(code))
        .ok_or(HttpError::Protocol)?;
    Ok((
        version.to_string(),
        u16::try_from(status).expect("100..=999"),
    ))
}

/// `_read_headers`: the header lines up to and including the blank line (or EOF); at most
/// 100 lines counting the blank one.
fn read_header_lines(reader: &mut Buffered<'_>) -> Result<Vec<Vec<u8>>, HttpError> {
    let mut lines = Vec::new();
    loop {
        let line = reader.readline(MAX_LINE + 1)?;
        if line.len() > MAX_LINE {
            return Err(HttpError::Protocol);
        }
        let end = line.is_empty() || line == b"\r\n" || line == b"\n";
        lines.push(line);
        if lines.len() > MAX_HEADERS {
            return Err(HttpError::Protocol);
        }
        if end {
            return Ok(lines);
        }
    }
}

/// The headers the `email` parser (compat32) reads from a Latin-1 header block: lines split at
/// `\r\n`, `\r` or `\n`; the block ends at the first line that is neither `Name:` (printable,
/// no colon or space in the name), a continuation (space or tab) nor `From `; a leading
/// `From ` line is the envelope, a later one is skipped; a value is the text after the colon
/// with leading blanks removed, continuations appended, the final line break removed.
fn parse_header_block(text: &str) -> Vec<(String, String)> {
    let mut lines: Vec<&str> = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\r' => {
                let end = if bytes.get(index + 1) == Some(&b'\n') {
                    index + 2
                } else {
                    index + 1
                };
                lines.push(&text[start..end]);
                start = end;
                index = end;
            }
            b'\n' => {
                lines.push(&text[start..=index]);
                start = index + 1;
                index += 1;
            }
            _ => index += 1,
        }
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    let header_lines: Vec<&str> = lines
        .iter()
        .copied()
        .take_while(|line| is_header_line(line))
        .collect();
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    let last = header_lines.len().saturating_sub(1);
    for (number, line) in header_lines.iter().enumerate() {
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = current.as_mut() {
                value.push_str(line);
            }
            continue;
        }
        if let Some(done) = current.take() {
            headers.push(finish_header(done));
        }
        if line.starts_with("From ") {
            if number == last && number != 0 {
                break;
            }
            continue;
        }
        let colon = line.find(':').expect("a header line has a colon");
        if colon == 0 {
            continue;
        }
        let value = line[colon + 1..]
            .trim_start_matches([' ', '\t'])
            .to_string();
        current = Some((line[..colon].to_string(), value));
    }
    if let Some(done) = current.take() {
        headers.push(finish_header(done));
    }
    headers
}

fn finish_header((name, value): (String, String)) -> (String, String) {
    (name, value.trim_end_matches(['\r', '\n']).to_string())
}

/// `^(From |[\041-\071\073-\176]*:|[\t ])`.
fn is_header_line(line: &str) -> bool {
    if line.starts_with("From ") || line.starts_with([' ', '\t']) {
        return true;
    }
    let name_end = line
        .find(|c: char| !matches!(c, '\u{21}'..='\u{39}' | '\u{3b}'..='\u{7e}'))
        .unwrap_or(line.len());
    line[name_end..].starts_with(':')
}
