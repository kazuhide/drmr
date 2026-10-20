//! A local S3 HTTP fixture supporting bucket location and object operations.

use drmr_core::storage::S3Access;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One request as the fake saw it.
#[derive(Clone, Debug)]
pub struct Logged {
    pub method: String,
    pub bucket: String,
    pub key: String,
    pub query: Vec<(String, String)>,
}

impl Logged {
    pub fn query(&self, name: &str) -> Option<&str> {
        self.query
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

/// An injected fault.
#[derive(Clone, Debug)]
pub enum Fault {
    /// An error response with this status and S3 error code (no body for `HEAD`).
    Status(u16, &'static str),
    /// Close the connection without a response.
    Close,
    /// Send part of a status line and headers, then close.
    PartialHeaders,
    /// Declare `declared` bytes, send `body`, close.
    Truncate { declared: usize, body: Vec<u8> },
    /// Wait, then answer normally.
    Delay(Duration),
    /// Answer with this listing page instead (raw XML).
    Xml(String),
    /// Write an exact HTTP response, including its status and headers.
    Raw(Vec<u8>),
    /// Send one byte at a time, pausing before every byte.
    SlowBody { pause: Duration, body: Vec<u8> },
}

type Hook = Box<dyn Fn(&Logged) -> Option<Fault> + Send>;

#[derive(Default)]
struct State {
    buckets: HashMap<String, (String, BTreeMap<String, Vec<u8>>)>,
    requests: Vec<Logged>,
    hook: Option<Hook>,
}

/// The running fake.
pub struct FakeS3 {
    pub endpoint: String,
    state: Arc<Mutex<State>>,
}

impl FakeS3 {
    pub fn start() -> FakeS3 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let state = Arc::clone(&shared);
                std::thread::spawn(move || serve(stream, &state));
            }
        });
        FakeS3 { endpoint, state }
    }

    /// Synthetic credentials for this endpoint.
    pub fn access(&self) -> S3Access {
        S3Access {
            endpoint: self.endpoint.clone(),
            access_key: "AKIDSYNTHETICTEST".to_string(),
            secret_key: "synthetic-secret-not-real".to_string(),
            session_token: None,
        }
    }

    pub fn bucket(&self, name: &str, region: &str) {
        self.state
            .lock()
            .unwrap()
            .buckets
            .insert(name.to_string(), (region.to_string(), BTreeMap::new()));
    }

    pub fn put(&self, bucket: &str, key: &str, body: &[u8]) {
        let mut state = self.state.lock().unwrap();
        state
            .buckets
            .get_mut(bucket)
            .unwrap()
            .1
            .insert(key.to_string(), body.to_vec());
    }

    pub fn object(&self, bucket: &str, key: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .unwrap()
            .buckets
            .get(bucket)?
            .1
            .get(key)
            .cloned()
    }

    pub fn keys(&self, bucket: &str) -> Vec<String> {
        let state = self.state.lock().unwrap();
        state
            .buckets
            .get(bucket)
            .map_or_else(Vec::new, |b| b.1.keys().cloned().collect())
    }

    pub fn on(&self, hook: impl Fn(&Logged) -> Option<Fault> + Send + 'static) {
        self.state.lock().unwrap().hook = Some(Box::new(hook));
    }

    pub fn requests(&self) -> Vec<Logged> {
        self.state.lock().unwrap().requests.clone()
    }

    /// Requests with this method (bucket-level ones included).
    pub fn count(&self, method: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.method == method)
            .count()
    }

    /// Object requests with this method (not `GetBucketLocation`, listings or deletes).
    pub fn count_object(&self, method: &str) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.method == method && !r.key.is_empty())
            .count()
    }
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let escaped = (bytes[index] == b'%')
            .then(|| text.get(index + 1..index + 3))
            .flatten()
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        if let Some(byte) = escaped {
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn encode_key(key: &str) -> String {
    let mut out = String::new();
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~/".contains(&byte) {
            out.push(char::from(byte));
        } else {
            write!(out, "%{byte:02X}").unwrap();
        }
    }
    out
}

fn read_request(stream: &mut TcpStream) -> Option<(String, String, Vec<u8>)> {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).ok()? == 0 {
            return None;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_string();
    let target = request_line.next()?.to_string();
    let length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim().eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).ok()?;
    Some((method, target, body))
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    headers: &[(&str, String)],
    body: &[u8],
    head_only: bool,
) {
    let mut out = format!("HTTP/1.1 {status} X\r\nConnection: close\r\n");
    for (name, value) in headers {
        out.push_str(name);
        out.push_str(": ");
        out.push_str(value);
        out.push_str("\r\n");
    }
    if !headers.iter().any(|(name, _)| *name == "Content-Length") {
        write!(out, "Content-Length: {}", body.len()).unwrap();
        out.push_str("\r\n");
    }
    out.push_str("\r\n");
    let _ = stream.write_all(out.as_bytes());
    if !head_only {
        let _ = stream.write_all(body);
    }
    let _ = stream.flush();
}

fn error_xml(code: &str) -> Vec<u8> {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code><Message>private-provider-message</Message></Error>")
        .into_bytes()
}

fn serve(mut stream: TcpStream, state: &Mutex<State>) {
    let Some((method, target, body)) = read_request(&mut stream) else {
        return;
    };
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    let path = path.trim_start_matches('/');
    let (bucket, key) = path.split_once('/').unwrap_or((path, ""));
    let logged = Logged {
        method: method.clone(),
        bucket: decode(bucket),
        key: decode(key),
        query: query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                (decode(name), decode(value))
            })
            .collect(),
    };
    let fault = {
        let mut state = state.lock().unwrap();
        state.requests.push(logged.clone());
        state.hook.as_ref().and_then(|hook| hook(&logged))
    };
    let head_only = method == "HEAD";
    match fault {
        Some(Fault::Status(status, code)) => {
            respond(&mut stream, status, &[], &error_xml(code), head_only);
            return;
        }
        Some(Fault::Close) => return,
        Some(Fault::PartialHeaders) => {
            let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Le");
            return;
        }
        Some(Fault::Truncate { declared, body }) => {
            let header = format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\n\r\n");
            let _ = stream.write_all(header.as_bytes());
            let _ = stream.write_all(&body);
            return;
        }
        Some(Fault::Delay(pause)) => std::thread::sleep(pause),
        Some(Fault::Xml(xml)) => {
            respond(&mut stream, 200, &[], xml.as_bytes(), head_only);
            return;
        }
        Some(Fault::Raw(response)) => {
            let _ = stream.write_all(&response);
            return;
        }
        Some(Fault::SlowBody { pause, body }) => {
            let header = format!(
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(header.as_bytes());
            for byte in body {
                std::thread::sleep(pause);
                if stream.write_all(&[byte]).is_err() {
                    break;
                }
                let _ = stream.flush();
            }
            return;
        }
        None => {}
    }
    let (status, headers, reply) = answer(state, &logged, body);
    respond(&mut stream, status, &headers, &reply, head_only);
}

fn answer(
    state: &Mutex<State>,
    request: &Logged,
    body: Vec<u8>,
) -> (u16, Vec<(&'static str, String)>, Vec<u8>) {
    let mut state = state.lock().unwrap();
    let Some((region, objects)) = state.buckets.get_mut(&request.bucket) else {
        return (404, vec![], error_xml("NoSuchBucket"));
    };
    match (request.method.as_str(), request.key.is_empty()) {
        ("GET", true) if request.query("location").is_some() => (
            200,
            vec![],
            format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<LocationConstraint xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">{region}</LocationConstraint>").into_bytes(),
        ),
        ("GET", true) if request.query("list-type") == Some("2") => (200, vec![], list(objects, request).into_bytes()),
        ("POST", true) if request.query("delete").is_some() => {
            let text = String::from_utf8_lossy(&body).into_owned();
            for part in text.split("<Key>").skip(1) {
                let key = part.split("</Key>").next().unwrap_or_default();
                objects.remove(&key.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">"));
            }
            (200, vec![], b"<?xml version=\"1.0\" encoding=\"UTF-8\"?><DeleteResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"></DeleteResult>".to_vec())
        }
        ("HEAD", false) => match objects.get(&request.key) {
            Some(data) => (200, vec![("Content-Length", data.len().to_string())], Vec::new()),
            None => (404, vec![], Vec::new()),
        },
        ("GET", false) => match objects.get(&request.key) {
            Some(data) => (200, vec![], data.clone()),
            None => (404, vec![], error_xml("NoSuchKey")),
        },
        ("PUT", false) => {
            objects.insert(request.key.clone(), body);
            (200, vec![], Vec::new())
        }
        _ => (400, vec![], error_xml("InvalidRequest")),
    }
}

fn list(objects: &BTreeMap<String, Vec<u8>>, request: &Logged) -> String {
    let prefix = request.query("prefix").unwrap_or_default();
    let max_keys: usize = request
        .query("max-keys")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1000);
    let start: usize = request
        .query("continuation-token")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let matching: Vec<&String> = objects
        .keys()
        .filter(|key| key.starts_with(prefix))
        .collect();
    let page: Vec<&&String> = matching.iter().skip(start).take(max_keys).collect();
    let truncated = start + page.len() < matching.len();
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">",
    );
    write!(xml, "<IsTruncated>{truncated}</IsTruncated>").unwrap();
    for key in &page {
        write!(
            xml,
            "<Contents><Key>{}</Key><Size>{}</Size></Contents>",
            encode_key(key),
            objects[**key].len()
        )
        .unwrap();
    }
    if truncated {
        write!(
            xml,
            "<NextContinuationToken>{}</NextContinuationToken>",
            start + page.len()
        )
        .unwrap();
    }
    xml.push_str("<EncodingType>url</EncodingType></ListBucketResult>");
    xml
}
