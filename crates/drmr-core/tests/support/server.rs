//! Local HTTP fixture servers for response framing, connection reuse, and failures.

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

/// The protocol version the handler speaks (`protocol_version`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    /// `HTTP/1.0`: one response per connection.
    Http10,
    /// `HTTP/1.1`: keep-alive.
    Http11,
}

/// One request as the handler sees it.
#[derive(Clone, Debug)]
pub struct Request {
    /// The request target (`self.path`).
    pub path: String,
    /// The headers as sent.
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// The last value of `name` (`dict(self.headers)[name]`), case-insensitive.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .rev()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// The response side of one request (`BaseHTTPRequestHandler`'s `send_response`,
/// `send_header`, `end_headers`, `wfile`).
pub struct Exchange<'a> {
    stream: &'a mut TcpStream,
    protocol: Protocol,
    head: Vec<u8>,
    /// The request.
    pub request: Request,
    /// Close the connection after this response (`close_connection`).
    pub close_connection: bool,
}

impl Exchange<'_> {
    /// The request target.
    pub fn path(&self) -> &str {
        &self.request.path
    }

    /// `send_response(code)`: the status line, `Server` and `Date`.
    pub fn send_response(&mut self, code: u16) {
        let version = match self.protocol {
            Protocol::Http10 => "HTTP/1.0",
            Protocol::Http11 => "HTTP/1.1",
        };
        let reason = match code {
            200 => "OK",
            206 => "Partial Content",
            301 => "Moved Permanently",
            302 => "Found",
            404 => "Not Found",
            503 => "Service Unavailable",
            _ => "Status",
        };
        self.head
            .extend_from_slice(format!("{version} {code} {reason}\r\n").as_bytes());
        self.send_header("Server", "BaseHTTP/0.6 drmr-test");
        self.send_header("Date", "Sat, 17 Oct 2026 00:00:00 GMT");
    }

    /// `send_header(name, value)` (`Connection: close` closes the connection afterwards).
    pub fn send_header(&mut self, name: &str, value: &str) {
        if name.eq_ignore_ascii_case("connection") {
            if value.eq_ignore_ascii_case("close") {
                self.close_connection = true;
            } else if value.eq_ignore_ascii_case("keep-alive") {
                self.close_connection = false;
            }
        }
        self.head
            .extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }

    /// `end_headers`: the blank line, everything sent.
    pub fn end_headers(&mut self) {
        self.head.extend_from_slice(b"\r\n");
        let head = std::mem::take(&mut self.head);
        let _ = self.stream.write_all(&head);
        let _ = self.stream.flush();
    }

    /// `wfile.write(data)`; the error is the handler's to ignore (the client may have gone).
    pub fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.stream.write_all(data)?;
        self.stream.flush()
    }

    /// `_respond`: 200, `Content-Length`, the body.
    pub fn respond(&mut self, body: &[u8]) {
        self.send_response(200);
        self.send_header("Content-Length", &body.len().to_string());
        self.end_headers();
        let _ = self.write(body);
    }
}

type Handler = dyn Fn(&mut Exchange<'_>) + Send + Sync;

/// A route of [`TestServer::routes`]: path, status, headers, body.
pub type Route<'a> = (&'a str, u16, Vec<(&'a str, &'a str)>, Vec<u8>);

struct OwnedRoute {
    path: String,
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

/// A running fixture server; dropping it stops accepting connections.
pub struct TestServer {
    /// `http://127.0.0.1:<port>`.
    pub origin: String,
    address: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
    connections: Arc<Mutex<Vec<SocketAddr>>>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl TestServer {
    /// A server whose `handler` writes every response.
    pub fn start(
        protocol: Protocol,
        handler: impl Fn(&mut Exchange<'_>) + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let address = listener.local_addr().expect("local address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let handler: Arc<Handler> = Arc::new(handler);
        let accept = {
            let requests = Arc::clone(&requests);
            let connections = Arc::clone(&connections);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                    let Ok(stream) = stream else { continue };
                    if let Ok(peer) = stream.peer_addr() {
                        connections.lock().unwrap().push(peer);
                    }
                    let requests = Arc::clone(&requests);
                    let handler = Arc::clone(&handler);
                    std::thread::spawn(move || serve(stream, protocol, &requests, &*handler));
                }
            })
        };
        TestServer {
            origin: format!("http://{address}"),
            address,
            requests,
            connections,
            stop,
            accept: Some(accept),
        }
    }

    /// The `server(routes)` fixture: HTTP/1.0, each route's status, headers and body as
    /// given (no `Content-Length` unless a route sets one), 404 for any other path.
    pub fn routes(routes: Vec<Route<'_>>) -> Self {
        let routes: Vec<OwnedRoute> = routes
            .into_iter()
            .map(|(path, status, headers, body)| OwnedRoute {
                path: path.to_string(),
                status,
                headers: headers
                    .into_iter()
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .collect(),
                body,
            })
            .collect();
        TestServer::start(Protocol::Http10, move |exchange| {
            let route = routes.iter().find(|route| route.path == exchange.path());
            let (status, headers, body) = route.map_or((404, Vec::new(), Vec::new()), |route| {
                (route.status, route.headers.clone(), route.body.clone())
            });
            exchange.send_response(status);
            for (name, value) in &headers {
                exchange.send_header(name, value);
            }
            exchange.end_headers();
            let _ = exchange.write(&body);
        })
    }

    /// The `_server11(routes)` fixture: HTTP/1.1 keep-alive, `Content-Length` on every
    /// response, 404 for any other path.
    pub fn routes11(routes: Vec<(String, u16, Vec<u8>)>) -> Self {
        TestServer::start(Protocol::Http11, move |exchange| {
            let route = routes.iter().find(|route| route.0 == exchange.path());
            let (status, body) =
                route.map_or((404, Vec::new()), |route| (route.1, route.2.clone()));
            exchange.send_response(status);
            exchange.send_header("Content-Length", &body.len().to_string());
            exchange.end_headers();
            let _ = exchange.write(&body);
        })
    }

    /// Every request so far.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().unwrap().clone()
    }

    /// The path of every request so far, in arrival order.
    pub fn paths(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|request| request.path)
            .collect()
    }

    /// The number of TCP connections accepted so far.
    pub fn connection_count(&self) -> usize {
        self.connections.lock().unwrap().len()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address); // wake the accept loop
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

/// One connection: requests until the client closes or the handler closes the connection.
fn serve(stream: TcpStream, protocol: Protocol, requests: &Mutex<Vec<Request>>, handler: &Handler) {
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader);
    let mut stream = stream;
    loop {
        let Some(request) = read_request(&mut reader) else {
            return;
        };
        requests.lock().unwrap().push(request.clone());
        let mut exchange = Exchange {
            stream: &mut stream,
            protocol,
            head: Vec::new(),
            request,
            close_connection: protocol == Protocol::Http10,
        };
        handler(&mut exchange);
        if exchange.close_connection {
            let _ = stream.shutdown(Shutdown::Write);
            return;
        }
    }
}

fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut line = String::new();
    if reader.read_line(&mut line).ok()? == 0 {
        return None;
    }
    let mut words = line.split_whitespace();
    let _method = words.next()?;
    let path = words.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.to_string(), value.trim().to_string()));
        }
    }
    Some(Request { path, headers })
}
