//! HTTP API and embedded browser console for drmr.

pub mod catalog;
pub mod config;
pub mod hls;
pub mod http;
pub mod jobs;
pub mod pyutil;
pub mod queue;
mod readiness;
pub mod router;
pub mod routes;
pub mod safelog;
pub mod static_files;
mod workers;

pub use config::{JobCli, ServeArgs, ServerConfig, config_from_env};

use crate::http::{Body, Reply, Req};
use crate::router::Match;
use axum::body::Bytes;
use serde_json::{Map, Value};
use std::fmt::Write as _;
use std::io::Write as _;
use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

/// The request body limit (`MAX_CONTENT_LENGTH = 256 * 1024`).
pub const MAX_CONTENT_LENGTH: u64 = 256 * 1024;

/// The application: `create_app(config)`'s objects.
#[derive(Debug)]
pub struct App {
    /// The configuration.
    pub config: Arc<ServerConfig>,
    /// The job runner (`<output_root>/_jobs`).
    pub manager: Arc<jobs::ConversionJobManager>,
    /// The record machinery and the clear-HLS service.
    pub service: Arc<hls::HlsService>,
    /// The conversion queue.
    pub queue: Arc<queue::ConversionQueue>,
    /// Where the UI comes from.
    pub static_root: static_files::StaticRoot,
    streams: Arc<workers::Workers>,
    requests: Arc<workers::Requests>,
    shutdown_lock: std::sync::Mutex<()>,
    closing: std::sync::atomic::AtomicBool,
}

/// Why the application could not be built.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The job log directory cannot be created.
    #[error("the job log directory cannot be created: {0}")]
    Jobs(#[source] std::io::Error),
    /// The service refused to start.
    #[error(transparent)]
    Service(#[from] hls::StartError),
}

impl App {
    /// `create_app(config)`: the job runner over `<output_root>/_jobs`, the service (output root
    /// resolved, history loaded), the queue.
    pub fn new(config: ServerConfig) -> Result<Arc<App>, AppError> {
        let config = Arc::new(config);
        let manager = Arc::new(
            jobs::ConversionJobManager::new(
                &config.output_root.join("_jobs"),
                config.cli_argv.clone(),
                config.hooks.base_env.clone(),
            )
            .map_err(AppError::Jobs)?,
        );
        let service = hls::HlsService::new(config.clone(), manager.clone())?;
        let queue = queue::ConversionQueue::new(config.clone(), manager.clone(), service.clone());
        let static_root = match &config.static_dir {
            Some(directory) => static_files::StaticRoot::Dir(directory.clone()),
            None => static_files::StaticRoot::Embedded,
        };
        Ok(Arc::new(App {
            config,
            manager,
            service,
            queue,
            static_root,
            streams: Arc::new(workers::Workers::default()),
            requests: Arc::new(workers::Requests::default()),
            shutdown_lock: std::sync::Mutex::new(()),
            closing: std::sync::atomic::AtomicBool::new(false),
        }))
    }

    /// Close submissions and drain owned jobs, queue, streams, records and credentials.
    /// External `Arc<App>` owners cannot keep launching work after this gate closes.
    pub fn shutdown(&self) -> Result<(), ShutdownError> {
        let _shutdown = self
            .shutdown_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.manager.begin_shutdown();
        self.closing
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let queue_ok = self.queue.shutdown().is_ok();
        self.manager
            .shutdown()
            .map_err(|_| ShutdownError::CleanupUnconfirmed)?;
        // Observations remain available throughout native cleanup. The final drain
        // closes their own gate before joining streams and blocking dispatches.
        self.requests.close();
        let streams_ok = self.streams.shutdown().is_ok();
        self.requests.drain();
        let service_ok = self.service.shutdown().is_ok();
        if !queue_ok || !streams_ok || !service_ok {
            // A failed embedding host still owns the server. Permit only its
            // diagnostic routes, with fresh owned streams for logs.
            self.streams.reopen_after_drain();
            self.requests.reopen();
            return Err(if !queue_ok || !streams_ok {
                ShutdownError::WorkerUnconfirmed
            } else {
                ShutdownError::RecordsUnconfirmed
            });
        }
        Ok(())
    }
}

pub fn build_router(app: Arc<App>) -> axum::Router {
    axum::Router::new().fallback(handle).with_state(app)
}

/// Reject browser requests addressed to an unrelated hostname before any route can read data.
fn host_guard(req: &Req, config: &ServerConfig) -> Option<Reply> {
    let mut hosts = req.headers.iter().filter(|(name, _)| name == "host");
    let Some((_, value)) = hosts.next() else {
        return Some(Reply::json_error(400, "invalid-host"));
    };
    if hosts.next().is_some() || value.contains('@') || value.ends_with(':') {
        return Some(Reply::json_error(400, "invalid-host"));
    }
    let Ok(authority) = value.parse::<axum::http::uri::Authority>() else {
        return Some(Reply::json_error(400, "invalid-host"));
    };
    if authority.as_str().len() != authority.host().len()
        && authority.port_u16().is_none_or(|port| port == 0)
    {
        return Some(Reply::json_error(400, "invalid-host"));
    }
    let host = authority.host().trim_matches(['[', ']']);
    let configured = config.bind_host.trim_matches(['[', ']']);
    let loopback = host.eq_ignore_ascii_case("localhost") || matches!(host, "127.0.0.1" | "::1");
    let explicit_bind =
        !matches!(configured, "0.0.0.0" | "::") && host.eq_ignore_ascii_case(configured);
    if loopback || explicit_bind {
        None
    } else {
        Some(Reply::json_error(403, "untrusted-host"))
    }
}

/// `hls_request_guard`: for `/api/hls/`, `/api/jobs/`, `/api/queue` and `/api/convert`, a declared body over
/// 256 KiB is `413`; a `POST` / `DELETE` from another origin (`Origin` not this host, or
/// `Sec-Fetch-Site` other than `same-origin` / `none`) is `403`; a `POST` without JSON (but the
/// license route) is `415`.
fn hls_guard(req: &Req) -> Option<Reply> {
    if !["/api/hls/", "/api/jobs/", "/api/queue", "/api/convert"]
        .iter()
        .any(|prefix| req.path.starts_with(prefix))
    {
        return None;
    }
    let refuse = |code: &'static str, status: u16| {
        Some(routes::RouteError::Api(hls::HlsApiError::status(code, status)).reply())
    };
    if req
        .content_length()
        .is_some_and(|length| length > MAX_CONTENT_LENGTH)
    {
        return refuse("request-too-large", 413);
    }
    if req.method == "POST" || req.method == "DELETE" {
        let host_url = host_url(req);
        let foreign_origin = req
            .header("origin")
            .is_some_and(|origin| origin.trim_end_matches('/') != host_url.trim_end_matches('/'));
        let site = req.header("sec-fetch-site");
        if foreign_origin || site.is_some_and(|site| site != "same-origin" && site != "none") {
            return refuse("same-origin-required", 403);
        }
        if req.method == "POST" && !req.path.ends_with("/license") && !req.is_json() {
            return refuse("json-required", 415);
        }
    }
    None
}

/// `request.host_url`: `http://` + the validated `Host` (port 80 dropped, host lower-cased) +
/// `/`; an invalid or missing `Host` is the empty host.
fn host_url(req: &Req) -> String {
    let host = req.header("host").unwrap_or_default();
    let host = host.strip_suffix(":80").unwrap_or(host);
    let (name, port) = match host.rfind(':') {
        Some(index) if !host[index..].contains(']') => (&host[..index], Some(&host[index + 1..])),
        _ => (host, None),
    };
    let name_ok = if name.starts_with('[') {
        name.ends_with(']')
            && name.len() > 2
            && name[1..name.len() - 1].contains(':')
            && name[1..name.len() - 1]
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b':' || b == b'.')
    } else {
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    };
    let port_ok = port.is_none_or(|port| {
        !port.is_empty()
            && port.len() <= 5
            && !port.starts_with('0')
            && port.bytes().all(|b| b.is_ascii_digit())
            && port.parse::<u32>().is_ok_and(|p| (1..=65_535).contains(&p))
    });
    if !name_ok || !port_ok {
        return "http:///".to_string();
    }
    let name = name.to_ascii_lowercase();
    match port {
        Some(port) => format!("http://{name}:{port}/"),
        None => format!("http://{name}/"),
    }
}

/// `hls_no_store`: no caching of the API, report and platform answers.
fn no_store(path: &str, reply: &mut Reply) {
    if [
        "/api/hls/",
        "/api/reports/",
        "/reports/",
        "/api/platform",
        "/api/queue",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
    {
        reply.set_header("Cache-Control", "no-store");
        reply.set_header("Pragma", "no-cache");
    }
}

/// `parse_qsl(query, keep_blank_values=True)`: `&`-separated pairs, `+` as a space,
/// percent-decoding with invalid UTF-8 left percent-encoded.
fn parse_query(query: &str) -> Vec<(String, String)> {
    let decode = |text: &str| decode_component(&text.replace('+', " "));
    query
        .split('&')
        .filter(|item| !item.is_empty())
        .map(|item| match item.split_once('=') {
            Some((name, value)) => (decode(name), decode(value)),
            None => (decode(item), String::new()),
        })
        .collect()
}

/// Percent-decoding to UTF-8, invalid bytes kept as `%XX` (`errors="werkzeug.url_quote"`).
fn decode_component(text: &str) -> String {
    let bytes = pyutil::percent_decode(text);
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => {
            let bytes = error.into_bytes();
            let mut out = String::new();
            let mut rest: &[u8] = &bytes;
            while !rest.is_empty() {
                match std::str::from_utf8(rest) {
                    Ok(text) => {
                        out.push_str(text);
                        break;
                    }
                    Err(error) => {
                        let valid = error.valid_up_to();
                        out.push_str(std::str::from_utf8(&rest[..valid]).unwrap_or_default());
                        let bad = error.error_len().unwrap_or(rest.len() - valid);
                        for byte in &rest[valid..valid + bad] {
                            let _ = write!(out, "%{byte:02X}");
                        }
                        rest = &rest[valid + bad..];
                    }
                }
            }
            out
        }
    }
}

fn request_path(raw: &str) -> String {
    let decoded = String::from_utf8_lossy(&pyutil::percent_decode(raw)).into_owned();
    format!("/{}", decoded.trim_start_matches('/'))
}

async fn read_body(body: axum::body::Body) -> Result<Vec<u8>, u16> {
    use http_body::Body as _;
    let mut body = body;
    let mut collected = Vec::new();
    loop {
        let frame = std::future::poll_fn(|cx| std::pin::Pin::new(&mut body).poll_frame(cx)).await;
        match frame {
            None => return Ok(collected),
            Some(Err(_)) => return Err(400),
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    collected.extend_from_slice(&data);
                    if u64::try_from(collected.len()).unwrap_or(u64::MAX) > MAX_CONTENT_LENGTH {
                        return Err(413);
                    }
                }
            }
        }
    }
}

async fn handle(
    axum::extract::State(app): axum::extract::State<Arc<App>>,
    request: axum::extract::Request,
) -> axum::response::Response {
    let started = Instant::now();
    let (parts, body) = request.into_parts();
    let mut req = Req {
        method: parts.method.as_str().to_string(),
        path: request_path(parts.uri.path()),
        args: parse_query(parts.uri.query().unwrap_or_default()),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_ascii_lowercase(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect(),
        body: Vec::new(),
    };
    let (mut reply, timed) =
        if let Some(reply) = host_guard(&req, &app.config).or_else(|| hls_guard(&req)) {
            // The guard runs before the request timer starts (`before_request` order).
            (reply, false)
        } else {
            (respond(&app, &mut req, body).await, true)
        };
    no_store(&req.path, &mut reply);
    let mut record = Map::new();
    record.insert("path".into(), Value::from(req.path.clone()));
    record.insert("method".into(), Value::from(req.method.clone()));
    record.insert("status".into(), Value::from(reply.status));
    record.insert(
        "duration_ms".into(),
        if timed {
            Value::from(started.elapsed().as_secs_f64() * 1000.0)
        } else {
            Value::Null
        },
    );
    record.insert(
        "content_length".into(),
        reply.content_length().map_or(Value::Null, Value::from),
    );
    if (app.config.hooks.log)(&pyutil::dumps(&Value::Object(record))).is_err() {
        return into_response(http::html_error(500), &req.method, &app.streams);
    }
    into_response(reply, &req.method, &app.streams)
}

/// Route and run the request (`dispatch_request` and the error handlers).
async fn respond(app: &Arc<App>, req: &mut Req, body: axum::body::Body) -> Reply {
    match router::route(&req.method, &req.path) {
        Match::Found(endpoint, values) => {
            let observation = matches!(
                endpoint,
                router::Endpoint::Index
                    | router::Endpoint::Static
                    | router::Endpoint::JobStatus
                    | router::Endpoint::JobStop
                    | router::Endpoint::JobLog
                    | router::Endpoint::JobLogDownload
                    | router::Endpoint::HlsJobs
                    | router::Endpoint::QueueState
            );
            if app.closing.load(std::sync::atomic::Ordering::SeqCst) && !observation {
                return Reply::json_error(503, "server-shutting-down");
            }
            if routes::reads_body(endpoint, &req.method) {
                if req
                    .content_length()
                    .is_some_and(|length| length > MAX_CONTENT_LENGTH)
                {
                    return http::html_error(413);
                }
                match read_body(body).await {
                    Ok(bytes) => req.body = bytes,
                    Err(status) => return http::html_error(status),
                }
            }
            let Some(permit) = app.requests.enter() else {
                return Reply::json_error(503, "server-shutting-down");
            };
            let app = app.clone();
            let request = req.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                routes::dispatch(&app, endpoint, &values, &request)
            })
            .await
            .unwrap_or_else(|_| http::html_error(500))
        }
        Match::Options(allowed) => http::options(&allowed),
        Match::MethodNotAllowed(allowed) => http::method_not_allowed(&allowed),
        Match::Redirect(path) => {
            let host = req.header("host").unwrap_or_default();
            let query = req
                .args
                .iter()
                .map(|(name, value)| {
                    format!("{}={}", pyutil::quote(name, ""), pyutil::quote(value, ""))
                })
                .collect::<Vec<_>>()
                .join("&");
            let path = pyutil::quote(&path, "!$&'()*+,/:;=@");
            let location = if query.is_empty() {
                format!("http://{host}{path}")
            } else {
                format!("http://{host}{path}?{query}")
            };
            http::redirect(&location)
        }
        Match::NotFound => http::html_error(404),
    }
}

/// A channel-backed response body.
struct ChannelBody(tokio::sync::mpsc::Receiver<Result<Vec<u8>, std::io::Error>>);

impl http_body::Body for ChannelBody {
    type Data = Bytes;
    type Error = std::io::Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        self.0.poll_recv(cx).map(|item| {
            item.map(|chunk| chunk.map(|bytes| http_body::Frame::data(Bytes::from(bytes))))
        })
    }
}

fn into_response(
    reply: Reply,
    method: &str,
    workers: &Arc<workers::Workers>,
) -> axum::response::Response {
    const ENTITY: [&str; 9] = [
        "allow",
        "content-encoding",
        "content-language",
        "content-length",
        "content-md5",
        "content-range",
        "content-type",
        "last-modified",
        "content-location",
    ];
    let mut builder = axum::http::Response::builder().status(reply.status);
    for (name, value) in &reply.headers {
        let lower = name.to_ascii_lowercase();
        if reply.status == 304 && ENTITY.contains(&lower.as_str()) && lower != "content-location" {
            continue;
        }
        builder = builder.header(name.as_str(), value.as_str());
    }
    let body = if method == "HEAD" || reply.status == 304 {
        axum::body::Body::empty()
    } else {
        match reply.body {
            Body::Bytes(bytes) => axum::body::Body::from(bytes),
            Body::File {
                path,
                start,
                length,
            } => match routes::file_body(workers, path, start, length) {
                Body::Stream(receiver) => axum::body::Body::new(ChannelBody(receiver)),
                _ => axum::body::Body::empty(),
            },
            Body::Stream(receiver) => axum::body::Body::new(ChannelBody(receiver)),
        }
    };
    builder
        .body(body)
        .unwrap_or_else(|_| axum::http::Response::new(axum::body::Body::empty()))
}

/// A shutdown failure preserves the embedding host's application and process ownership.
#[derive(Debug, thiserror::Error)]
pub enum ShutdownError {
    /// Native process-tree cleanup could not be confirmed.
    #[error("cleanup-unconfirmed")]
    CleanupUnconfirmed,
    /// An owned background worker failed to join successfully.
    #[error("worker-cleanup-unconfirmed")]
    WorkerUnconfirmed,
    /// Terminal history or private request cleanup could not be confirmed.
    #[error("record-cleanup-unconfirmed")]
    RecordsUnconfirmed,
}

/// Why `serve` stopped.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The application could not be built.
    #[error(transparent)]
    App(#[from] AppError),
    /// The address cannot be bound or the server failed.
    #[error("server: {0}")]
    Io(#[from] std::io::Error),
    /// Owned work did not shut down cleanly.
    #[error(transparent)]
    Shutdown(#[from] ShutdownError),
}

pub fn serve(config: ServerConfig) -> Result<(), ServeError> {
    serve_with_ready(config, |address| {
        let _ = writeln!(std::io::stderr(), " * Running on http://{address}");
    })
}

fn serve_with_ready(
    config: ServerConfig,
    ready: impl FnOnce(SocketAddr),
) -> Result<(), ServeError> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let host = config.bind_host.clone();
    let port = config.bind_port;
    let app = App::new(config)?;
    let result = runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
        let address = listener.local_addr()?;
        ready(address);
        let shutdown_app = app.clone();
        tokio::select! {
            result = axum::serve(listener, build_router(app.clone())) => result,
            () = async move {
                loop {
                    shutdown_signal().await;
                    let owner = shutdown_app.clone();
                    match tokio::task::spawn_blocking(move || owner.shutdown()).await {
                        Ok(Ok(())) => break,
                        Ok(Err(error)) => {
                            let _ = writeln!(std::io::stderr(),
                                "drmr serve: shutdown incomplete ({error}); diagnostic access retained; fix the cause and signal again to retry");
                        }
                        Err(_) => {
                            let _ = writeln!(std::io::stderr(),
                                "drmr serve: shutdown incomplete (worker-cleanup-unconfirmed); diagnostic access retained; signal again to retry");
                        }
                    }
                }
            } => Ok(()),
        }
    });
    let cleanup = app.shutdown();
    drop(runtime);
    cleanup?;
    result.map_err(ServeError::Io)
}

pub fn run_server(args: &config::ServeArgs, jobs: config::JobCli, program: &str) -> ExitCode {
    let config = match config::configure(|name: &str| std::env::var_os(name), args, jobs) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("{}", error.message);
            return ExitCode::from(error.exit_code);
        }
    };
    match serve(config) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{program}: {error}");
            ExitCode::from(1)
        }
    }
}

async fn shutdown_signal() {
    let interrupt = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            let _ = interrupt.await;
            return;
        };
        tokio::select! {
            _ = interrupt => {}
            _ = terminate.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = interrupt.await;
    }
}

/// A server running on a background runtime (tests and embedding).
pub struct RunningServer {
    /// Where it listens.
    pub addr: SocketAddr,
    /// The application (its service, queue and job runner).
    pub app: Arc<App>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    shutdown_failed: bool,
}

impl std::fmt::Debug for RunningServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningServer")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl RunningServer {
    /// `http://<addr>`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Stop serving and confirm owned cleanup. Prefer `shutdown` when ownership
    /// must remain available to display or recover from an error.
    pub fn stop(mut self) -> Result<(), ShutdownError> {
        self.shutdown()
    }

    /// Shut down without consuming the owner. Failure keeps the listener/runtime
    /// and application available to the host; no successful cleanup is implied.
    pub fn shutdown(&mut self) -> Result<(), ShutdownError> {
        if self.shutdown_failed {
            return Err(ShutdownError::WorkerUnconfirmed);
        }
        self.app.shutdown()?;
        if let Some(sender) = self.shutdown.take() {
            let _ = sender.send(());
        }
        if let Some(thread) = self.thread.take()
            && !matches!(thread.join(), Ok(Ok(())))
        {
            self.shutdown_failed = true;
            return Err(ShutdownError::WorkerUnconfirmed);
        }
        Ok(())
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        // Best effort only. Embedding hosts use explicit shutdown and retain self on error.
        let _ = self.shutdown();
    }
}

/// Build the application and serve it on `config.bind_host:config.bind_port` (port 0: any) on
/// a background runtime.
pub fn spawn(config: ServerConfig) -> Result<RunningServer, ServeError> {
    let app = App::new(config)?;
    let listener =
        std::net::TcpListener::bind((app.config.bind_host.as_str(), app.config.bind_port))?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let (sender, receiver) = tokio::sync::oneshot::channel::<()>();
    let router = build_router(app.clone());
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::Builder::new()
        .name("drmr-server".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(error.kind()));
                    return Err(error);
                }
            };
            let result = runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(listener) => listener,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.kind()));
                        return Err(error);
                    }
                };
                let _ = ready_tx.send(Ok(()));
                tokio::select! {
                    result = axum::serve(listener, router) => result,
                    _ = receiver => Ok(()),
                }
            });
            drop(runtime);
            result
        })?;
    match ready_rx.recv() {
        Ok(Ok(())) => {}
        response => {
            let _ = thread.join();
            let kind = response
                .ok()
                .and_then(Result::err)
                .unwrap_or(std::io::ErrorKind::Other);
            return Err(std::io::Error::new(kind, "server startup failed").into());
        }
    }

    Ok(RunningServer {
        addr,
        app,
        shutdown: Some(sender),
        thread: Some(thread),
        shutdown_failed: false,
    })
}

#[cfg(test)]
mod host_tests {
    use super::*;

    fn request(host: &str) -> Req {
        Req {
            headers: vec![("host".into(), host.into())],
            ..Req::default()
        }
    }

    #[test]
    fn explicit_bind_names_are_trusted_but_wildcards_are_not_host_allowlists() {
        let mut config = ServerConfig::new("catalog", "output");
        config.bind_host = "console.example".into();
        assert!(host_guard(&request("CONSOLE.EXAMPLE:8080"), &config).is_none());
        assert_eq!(
            host_guard(&request("other.example"), &config)
                .unwrap()
                .status,
            403
        );
        for wildcard in ["0.0.0.0", "::"] {
            config.bind_host = wildcard.into();
            assert_eq!(
                host_guard(&request("console.example:8080"), &config)
                    .unwrap()
                    .status,
                403
            );
            assert!(host_guard(&request("localhost:8080"), &config).is_none());
        }
    }
}

#[cfg(all(test, unix))]
mod signal_tests;
