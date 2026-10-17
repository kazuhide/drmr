//! HTTP request handlers for the catalog, jobs and playback.

use crate::App;
use crate::catalog::{self, CatalogEntry};
use crate::hls::{HlsApiError, ServiceError, SourceAnswer, is_catalog_id, is_component};
use crate::http::{self, Body, FileFacts, Reply, Req};
use crate::jobs::{JobError, TailError};
use crate::pyutil;
use crate::router::Endpoint;
use crate::safelog;
use crate::static_files;
use drmr_core::config::{KeyMaterial, Rendition};
use drmr_core::manifest_dash::{self, DashError, DashRepresentationSummary};
use drmr_core::manifest_support::py::PyErr;
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug)]
pub enum RouteError {
    /// `HlsApiError`: `{**detail, "error": code}`.
    Api(HlsApiError),
    /// `PathConfinementError` / `FileNotFoundError`: `404 {"error": "not found"}`.
    NotFound,
    Http(u16),
    /// Anything else: `500`.
    Internal,
}

impl From<HlsApiError> for RouteError {
    fn from(error: HlsApiError) -> Self {
        RouteError::Api(error)
    }
}

impl From<ServiceError> for RouteError {
    fn from(error: ServiceError) -> Self {
        match error {
            ServiceError::Api(error) => RouteError::Api(error),
            ServiceError::Internal => RouteError::Internal,
        }
    }
}

impl From<catalog::CatalogScanError> for RouteError {
    fn from(_: catalog::CatalogScanError) -> Self {
        RouteError::Internal
    }
}

impl RouteError {
    pub fn reply(self) -> Reply {
        match self {
            RouteError::Api(error) => {
                let mut body = error.detail.clone();
                body.insert("error".into(), Value::from(error.code));
                Reply::json(error.status, &Value::Object(body))
            }
            RouteError::NotFound => Reply::json_error(404, "not found"),
            RouteError::Http(status) => http::html_error(status),
            RouteError::Internal => http::html_error(500),
        }
    }
}

type Answer = Result<Reply, RouteError>;

pub fn reads_body(endpoint: Endpoint, method: &str) -> bool {
    matches!(
        endpoint,
        Endpoint::Convert
            | Endpoint::HlsAnalyze
            | Endpoint::HlsPackage
            | Endpoint::PlaybackSessions
            | Endpoint::QueueAdd
            | Endpoint::PlaybackLicense
    ) || (endpoint == Endpoint::ClearkeyLicense && method == "POST")
}

/// Run one endpoint.
pub fn dispatch(app: &Arc<App>, endpoint: Endpoint, values: &[String], req: &Req) -> Reply {
    let value = |index: usize| values.get(index).map_or("", String::as_str);
    let answer = match endpoint {
        Endpoint::Index => page(app, req, "index.html", false),
        Endpoint::Static => static_file(app, req, value(0)),
        Endpoint::Catalog => api_catalog(app),
        Endpoint::Convert => api_convert(app, req),
        Endpoint::JobStatus => api_job_status(app, value(0)),
        Endpoint::JobLog => Ok(api_job_log(app, value(0))),
        Endpoint::JobLogDownload => Ok(api_job_log_download(app, value(0))),
        Endpoint::Assets => api_assets(app, value(0)),
        Endpoint::CatalogReport => catalog_report(app, value(0)).map(|body| {
            let mut reply = Reply::json(200, &body);
            reply.set_header("Cache-Control", "no-store");
            reply
        }),
        Endpoint::CatalogReportPage => catalog_report_page(app, req, value(0)),
        Endpoint::Source => source(app, req, value(0), value(1)),
        Endpoint::DashOld => dash_old(app, req),
        Endpoint::Output => output(app, req, value(0), value(1)),
        Endpoint::ClearkeyLicense => clearkey_license(app, req),
        _ => hls_dispatch(app, endpoint, values, req),
    };
    answer.unwrap_or_else(RouteError::reply)
}

fn hls_dispatch(app: &Arc<App>, endpoint: Endpoint, values: &[String], req: &Req) -> Answer {
    let value = |index: usize| values.get(index).map_or("", String::as_str);
    let service = &app.service;
    match endpoint {
        Endpoint::HlsAnalyze => Ok(Reply::json(202, &service.analyze(&json_body(req)?)?)),
        Endpoint::HlsAnalysis => Ok(Reply::json(200, &service.analysis(value(0))?)),
        Endpoint::HlsPackage => Ok(Reply::json(202, &service.package(&json_body(req)?)?)),
        Endpoint::HlsJobs => {
            let invalid = || RouteError::from(HlsApiError::status("pagination-invalid", 400));
            let limit =
                pyutil::python_int_text(req.arg("limit").unwrap_or("20")).ok_or_else(invalid)?;
            let cursor = req.arg("cursor");
            if cursor.is_some_and(|cursor| cursor.chars().count() > 1024) {
                return Err(invalid());
            }
            Ok(Reply::json(200, &service.jobs(cursor, limit)?))
        }
        Endpoint::HlsJobDelete => {
            service.delete_job(value(0))?;
            Ok(Reply::no_content())
        }
        Endpoint::HlsResult => Ok(Reply::json(200, &service.result(value(0))?)),
        Endpoint::Platform => Ok(Reply::json(200, &service.platform()?)),
        Endpoint::QueueState => Ok(Reply::json(200, &app.queue.snapshot())),
        Endpoint::QueueAdd => {
            let body = json_body(req)?;
            Ok(Reply::json(202, &app.queue.enqueue(body.get("asset_ids"))?))
        }
        Endpoint::QueueRemove => {
            if !is_catalog_id(value(0)) {
                return Err(HlsApiError::status("queue-item-not-found", 404).into());
            }
            app.queue.remove(value(0))?;
            Ok(Reply::no_content())
        }
        Endpoint::HlsReportPage => {
            if !is_component(value(0)) {
                return Err(HlsApiError::status("result-not-found", 404).into());
            }
            // The shell carries no report data; the API validates the snapshot once.
            service.check_result_record(value(0))?;
            page(app, req, "report.html", true)
        }
        Endpoint::PlaybackSessions => {
            Ok(Reply::json(201, &service.create_session(&json_body(req)?)?))
        }
        Endpoint::PlaybackRevoke => {
            service.revoke(value(0));
            Ok(Reply::no_content())
        }
        Endpoint::PlaybackLicense => {
            let session = service.session(value(0))?;
            let mut jwk = Map::new();
            jwk.insert("kty".into(), Value::from("oct"));
            jwk.insert("kid".into(), Value::from(b64url_hex(session.kid())));
            jwk.insert("k".into(), Value::from(b64url_hex(session.key())));
            let mut body = Map::new();
            body.insert("keys".into(), Value::Array(vec![Value::Object(jwk)]));
            body.insert("type".into(), Value::from("temporary"));
            Ok(Reply::json(200, &Value::Object(body)))
        }
        Endpoint::PlaybackSource => match service.source(value(0), value(1))? {
            SourceAnswer::Media { path, mime } => send_disk_file(req, &path, &mime),
            SourceAnswer::Playlist(text) => Ok(Reply::bytes(
                200,
                "application/vnd.apple.mpegurl",
                text.into_bytes(),
            )),
        },
        _ => Err(RouteError::Internal),
    }
}

/// `body()` of `register_hls`: the JSON object of the request, else `json-invalid` (400).
fn json_body(req: &Req) -> Result<Map<String, Value>, RouteError> {
    if !req.is_json() {
        return Err(HlsApiError::status("json-invalid", 400).into());
    }
    match pyutil::loads_bytes(&req.body) {
        Some(Value::Object(map)) => Ok(map),
        _ => Err(HlsApiError::status("json-invalid", 400).into()),
    }
}

/// hex → unpadded base64url (the W3C `ClearKey` encoding of `kid` and `k`).
fn b64url_hex(value: &str) -> String {
    use base64::Engine as _;
    let bytes = drmr_core::hls_cli::bytes_fromhex(value).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ------------------------------------------------------------------------------------------
// Files
// ------------------------------------------------------------------------------------------

/// The explicit MIME map of the media routes (never the OS guess).
fn mime_for(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
        .unwrap_or_default();
    match extension.as_str() {
        ".mpd" => "application/dash+xml",
        ".m3u8" => "application/vnd.apple.mpegurl",
        ".ts" => "video/mp2t",
        ".vtt" => "text/vtt",
        ".m4s" => "video/iso.segment",
        ".mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

/// `send_file(path, mimetype=…, conditional=True)` of a file on disk; a vanished file is the
/// `FileNotFoundError` handler's 404.
fn send_disk_file(req: &Req, path: &Path, mimetype: &str) -> Answer {
    let facts = FileFacts::stat(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => RouteError::NotFound,
        _ => RouteError::Internal,
    })?;
    Ok(http::send_file(req, &facts, mimetype))
}

/// `send_file(static_dir / name)` (`index.html`, `report.html`), `Cache-Control: no-store` for
/// the report pages.
fn page(app: &Arc<App>, req: &Req, name: &str, no_store: bool) -> Answer {
    let facts = static_files::page(&app.static_root, name).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => RouteError::NotFound,
        _ => RouteError::Internal,
    })?;
    let mimetype = static_files::guess_type(name).unwrap_or("application/octet-stream");
    let mut reply = http::send_file(req, &facts, mimetype);
    if no_store {
        reply.set_header("Cache-Control", "no-store");
    }
    Ok(reply)
}

fn static_file(app: &Arc<App>, req: &Req, filename: &str) -> Answer {
    let facts = static_files::lookup(&app.static_root, filename).ok_or(RouteError::Http(404))?;
    let name = facts
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mimetype = static_files::guess_type(&name).unwrap_or("application/octet-stream");
    Ok(http::send_file(req, &facts, mimetype))
}

/// `_resolve_within_root(rel, root)`: a non-empty relative path, resolved inside the resolved
/// root, naming a file; anything else is the indistinguishable 404.
fn resolve_within_root(rel: &str, root: &Path) -> Result<PathBuf, RouteError> {
    if rel.is_empty() || Path::new(rel).is_absolute() {
        return Err(RouteError::NotFound);
    }
    let root = pyutil::resolve(root).map_err(|_| RouteError::Internal)?;
    let candidate = pyutil::resolve(&root.join(rel)).map_err(|_| RouteError::Internal)?;
    if !pyutil::strictly_under(&candidate, &root) || !pyutil::is_file(&candidate) {
        return Err(RouteError::NotFound);
    }
    Ok(candidate)
}

/// `_local_manifest_rel(output_root, asset_id)`: the first (sorted) `.mpd` file under
/// `output/<id>/manifests/local/`, relative to the resolved output root.
fn local_manifest_rel(output_root: &Path, asset_id: &str) -> Result<Option<String>, RouteError> {
    let root = pyutil::resolve(output_root).map_err(|_| RouteError::Internal)?;
    let local = root.join(asset_id).join("manifests").join("local");
    if !pyutil::is_dir(&local) {
        return Ok(None);
    }
    let mut matches = Vec::new();
    let mut stack = vec![local];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_symlink = pyutil::is_symlink(&path);
            if !is_symlink && pyutil::is_dir(&path) {
                stack.push(path.clone());
            }
            if entry.file_name().to_string_lossy().ends_with(".mpd") && pyutil::is_file(&path) {
                matches.push(path);
            }
        }
    }
    matches.sort();
    Ok(matches
        .first()
        .and_then(|first| first.strip_prefix(&root).ok().map(pyutil::path_text)))
}

// ------------------------------------------------------------------------------------------
// Catalog, conversion, jobs
// ------------------------------------------------------------------------------------------

fn api_catalog(app: &Arc<App>) -> Answer {
    let log = app.config.hooks.log.clone();
    let mut on_skip = |entry_dir: &Path, reason: &str| {
        let mut record = Map::new();
        record.insert("event".into(), Value::from("catalog-entry-skipped"));
        record.insert(
            "entry_dir".into(),
            Value::from(pyutil::path_text(entry_dir)),
        );
        record.insert("reason".into(), Value::from(reason));
        let _ = log(&pyutil::dumps(&Value::Object(record)));
    };
    let entries = catalog::discover_catalog(&app.config.catalog_root, Some(&mut on_skip))?;
    let mut rows = Vec::with_capacity(entries.len());
    for entry in &entries {
        let converted = local_manifest_rel(&app.config.output_root, &entry.asset_id)?.is_some();
        rows.push(catalog::catalog_entry_public_dict(entry, converted));
    }
    Ok(Reply::json(200, &Value::Array(rows)))
}

fn api_convert(app: &Arc<App>, req: &Req) -> Answer {
    let body = pyutil::loads_bytes(&req.body)
        .filter(drmr_core::hls_cli::truthy)
        .unwrap_or_else(|| Value::Object(Map::new()));
    let Value::Object(body) = body else {
        return Err(RouteError::Internal); // `body.get` of a non-dict: AttributeError
    };
    let asset_id = body
        .get("asset_id")
        .filter(|value| drmr_core::hls_cli::truthy(value))
        .and_then(Value::as_str)
        .map(str::to_string);
    let entry = match &asset_id {
        Some(asset_id) => catalog::get_catalog_entry(&app.config.catalog_root, asset_id)?,
        None => None,
    };
    let (Some(entry), Some(asset_id)) = (entry, asset_id) else {
        return Ok(Reply::json_error(404, "unknown asset_id"));
    };
    if app.service.asset_reserved(&asset_id) {
        return Ok(Reply::json_error(409, "asset-already-exists"));
    }
    let handle = match app.manager.submit(&entry, &app.config.output_root) {
        Ok(handle) => handle,
        Err(error @ JobError::AlreadyRunning) => {
            return Ok(Reply::json_error(409, &error.to_string()));
        }
        Err(_) => return Err(RouteError::Internal),
    };
    // The job is running: a record that cannot be written is logged, never an error answer.
    app.service.record_conversion(&handle, &asset_id);
    let mut answer = Map::new();
    answer.insert("job_id".into(), Value::from(handle.job_id));
    Ok(Reply::json(202, &Value::Object(answer)))
}

fn state_payload(state: &Value, exit_code: &Value) -> Value {
    let mut payload = Map::new();
    payload.insert("state".into(), state.clone());
    payload.insert("exit_code".into(), exit_code.clone());
    Value::Object(payload)
}

fn api_job_status(app: &Arc<App>, job_id: &str) -> Answer {
    if app.service.owns(job_id) {
        let status = app.service.status(job_id)?;
        if status.get("kind").and_then(Value::as_str) == Some("convert") {
            let payload = state_payload(&status["state"], &status["exit_code"]);
            return Ok(Reply::json(200, &payload));
        }
        return Ok(Reply::json(200, &Value::Object(status)));
    }
    match app.manager.status(job_id) {
        Ok(handle) => Ok(Reply::json(
            200,
            &state_payload(
                &Value::from(handle.state),
                &handle.exit_code.map_or(Value::Null, Value::from),
            ),
        )),
        Err(_) => Ok(Reply::json_error(404, "no such job")),
    }
}

/// A line sanitiser: the clear-HLS allowlist, or the conversion log verbatim.
fn sanitizer(app: &Arc<App>, job_id: &str) -> bool {
    app.service.kind(job_id).as_deref() != Some("convert")
}

type Sender = tokio::sync::mpsc::Sender<Result<Vec<u8>, std::io::Error>>;

/// Run `produce` on its own thread, its chunks streamed as the response body.
fn stream(content_type: &str, produce: impl FnOnce(&Sender) + Send + 'static) -> Reply {
    let (sender, receiver) = tokio::sync::mpsc::channel(16);
    let spawned = std::thread::Builder::new()
        .name("response-stream".into())
        .spawn(move || produce(&sender));
    drop(spawned);
    Reply::stream(content_type, receiver)
}

fn send(sender: &Sender, chunk: String) -> bool {
    sender.blocking_send(Ok(chunk.into_bytes())).is_ok()
}

fn abort(sender: &Sender) {
    let _ = sender.blocking_send(Err(std::io::Error::other("stream aborted")));
}

fn api_job_log(app: &Arc<App>, job_id: &str) -> Reply {
    let owned = app.service.owns(job_id);
    let running_here = app.manager.status(job_id).is_ok();
    if !owned && !running_here {
        return Reply::json_error(404, "no such job");
    }
    let app = app.clone();
    let job_id = job_id.to_string();
    let hls = owned && sanitizer(&app, &job_id);
    stream("text/event-stream; charset=utf-8", move |sender| {
        let poll = app.config.hooks.tail_poll;
        let lines: Box<dyn Iterator<Item = Result<String, ()>>> = if running_here {
            match app.manager.tail_log(&job_id, poll) {
                Ok(tail) => Box::new(tail.map(|line| line.map_err(|_: TailError| ()))),
                Err(_) => Box::new(app.manager.read_log(&job_id).map(Ok)),
            }
        } else {
            Box::new(app.manager.read_log(&job_id).map(Ok))
        };
        for line in lines {
            let Ok(line) = line else {
                return abort(sender);
            };
            let line = if hls {
                match safelog::safe_log(&line) {
                    Ok(line) => line,
                    Err(_) => return abort(sender),
                }
            } else {
                line
            };
            if !send(sender, format!("data: {line}\n\n")) {
                return;
            }
        }
        let payload = if owned {
            match app.service.status(&job_id) {
                Ok(status) => Some(state_payload(&status["state"], &status["exit_code"])),
                Err(_) => return abort(sender),
            }
        } else {
            match app.manager.status(&job_id) {
                Ok(handle) if handle.exit_code.is_some() => Some(state_payload(
                    &Value::from(handle.state),
                    &handle.exit_code.map_or(Value::Null, Value::from),
                )),
                Ok(_) => None,
                Err(_) => return abort(sender),
            }
        };
        if let Some(payload) = payload {
            send(
                sender,
                format!("event: done\ndata: {}\n\n", pyutil::dumps(&payload)),
            );
        }
    })
}

fn api_job_log_download(app: &Arc<App>, job_id: &str) -> Reply {
    let hls = if app.service.owns(job_id) {
        sanitizer(app, job_id)
    } else if app.manager.status(job_id).is_ok() {
        false
    } else {
        return Reply::json_error(404, "no such job");
    };
    let shared = app.clone();
    let id = job_id.to_string();
    let mut reply = stream("text/plain; charset=utf-8; charset=utf-8", move |sender| {
        for line in shared.manager.read_log(&id) {
            let line = if hls {
                match safelog::safe_log(&line) {
                    Ok(line) => line,
                    Err(_) => return abort(sender),
                }
            } else {
                line
            };
            if !send(sender, format!("{line}\n")) {
                return;
            }
        }
    });
    reply.set_header(
        "Content-Disposition",
        &format!("attachment; filename=\"job-{job_id}.jsonl\""),
    );
    reply.set_header("Cache-Control", "no-store");
    reply
}

// ------------------------------------------------------------------------------------------
// Assets, reports, media
// ------------------------------------------------------------------------------------------

fn summary_value(rows: Option<&[DashRepresentationSummary]>) -> Value {
    let Some(rows) = rows.filter(|rows| !rows.is_empty()) else {
        return Value::Null;
    };
    let representative = rows
        .iter()
        .find(|row| row.content_type == "video" && row.in_scope)
        .unwrap_or(&rows[0]);
    let text = |value: &Option<String>| value.clone().map_or(Value::Null, Value::from);
    let mut map = Map::new();
    map.insert(
        "protection_scheme".into(),
        text(&representative.protection_scheme),
    );
    map.insert("default_kid".into(), text(&representative.default_kid));
    map.insert(
        "drm_systems".into(),
        Value::Array(
            representative
                .drm_systems
                .iter()
                .cloned()
                .map(Value::from)
                .collect(),
        ),
    );
    map.insert(
        "playready_version".into(),
        text(&representative.playready_version),
    );
    map.insert(
        "playready_algid".into(),
        text(&representative.playready_algid),
    );
    Value::Object(map)
}

fn renditions_value(
    rows: Option<&[DashRepresentationSummary]>,
    shapes: &HashMap<String, Rendition>,
) -> Value {
    let Some(rows) = rows else {
        return Value::Array(Vec::new());
    };
    let mut items = Vec::new();
    for row in rows.iter().filter(|row| row.in_scope) {
        let mut item = Map::new();
        item.insert("rep_id".into(), Value::from(row.rep_id.clone()));
        item.insert("content_type".into(), Value::from(row.content_type.clone()));
        item.insert("codec".into(), Value::from(row.codec.clone()));
        item.insert("container".into(), Value::from(row.container.clone()));
        let bandwidth = match &row.bandwidth {
            None => Value::Null,
            Some(value) => i64::try_from(value)
                .map(Value::from)
                .or_else(|_| u64::try_from(value).map(Value::from))
                .unwrap_or_else(|_| pyutil::big_int(&value.to_string())),
        };
        item.insert("bandwidth".into(), bandwidth);
        // `row.update({name: value for name, value in shape.items() if value is not None})`.
        if let Some(shape) = shapes.get(&row.rep_id) {
            if let Some(width) = shape.width {
                item.insert("width".into(), Value::from(width));
            }
            if let Some(height) = shape.height {
                item.insert("height".into(), Value::from(height));
            }
            if let Some(rate) = &shape.frame_rate {
                item.insert("frame_rate".into(), Value::from(rate.clone()));
            }
            if let Some(language) = &shape.lang {
                item.insert("language".into(), Value::from(language.clone()));
            }
            item.insert(
                "segment_count".into(),
                Value::from(shape.segment_paths.len()),
            );
        }
        items.push(Value::Object(item));
    }
    Value::Array(items)
}

fn analyze_manifest(path: &Path) -> Result<Option<Vec<DashRepresentationSummary>>, RouteError> {
    match manifest_dash::analyze_dash_manifest(path) {
        Ok(summary) => Ok(Some(summary.representations)),
        Err(DashError::Rewrite(_) | DashError::Py(PyErr::Os(_) | PyErr::UnicodeDecode(_))) => {
            Ok(None)
        }
        Err(DashError::PlayReady(_) | DashError::Py(_)) => Err(RouteError::Internal),
    }
}

fn api_assets(app: &Arc<App>, asset_id: &str) -> Answer {
    let Some(entry) = catalog::get_catalog_entry(&app.config.catalog_root, asset_id)? else {
        return Ok(Reply::json_error(404, "unknown asset_id"));
    };
    let new_rel = local_manifest_rel(&app.config.output_root, asset_id)?;
    let new_mpd = match &new_rel {
        Some(rel) => Some(
            pyutil::resolve(&app.config.output_root)
                .map_err(|_| RouteError::Internal)?
                .join(rel),
        ),
        None => None,
    };
    let old = analyze_manifest(&entry.mpd_path)?;
    let new = match &new_mpd {
        Some(path) => analyze_manifest(path)?,
        None => None,
    };
    let shapes = rendition_shapes(&entry);
    let mut renditions = Map::new();
    renditions.insert("old".into(), renditions_value(old.as_deref(), &shapes));
    renditions.insert(
        "new".into(),
        if new_mpd.is_some() {
            renditions_value(new.as_deref(), &shapes)
        } else {
            Value::Null
        },
    );
    let mut answer = Map::new();
    answer.insert(
        "old_url".into(),
        Value::from(format!("/dash/old?asset_id={asset_id}")),
    );
    answer.insert(
        "new_url".into(),
        new_rel.map_or(Value::Null, |rel| Value::from(format!("/output/{rel}"))),
    );
    answer.insert("old_summary".into(), summary_value(old.as_deref()));
    answer.insert("new_summary".into(), summary_value(new.as_deref()));
    answer.insert("renditions".into(), Value::Object(renditions));
    Ok(Reply::json(200, &Value::Object(answer)))
}

fn rendition_shapes(entry: &CatalogEntry) -> HashMap<String, Rendition> {
    let zeros = "0".repeat(32);
    let placeholder = KeyMaterial::new(zeros.as_str(), zeros.as_str(), zeros.as_str())
        .expect("32 zero digits are 16 bytes of hex");
    drmr_core::config::load_asset(
        &entry.mpd_path,
        placeholder,
        Path::new("."),
        Some(&entry.asset_id),
    )
    .map(|asset| {
        asset
            .renditions
            .into_iter()
            .map(|rendition| (rendition.rep_id.clone(), rendition))
            .collect()
    })
    .unwrap_or_default()
}

/// `_REPORT_CHECKS`: the catalog checks a report shows.
const REPORT_CHECKS: [&str; 20] = [
    "6-1.segment-duration",
    "6-1.number-origin",
    "6-1.boundary-agreement",
    "6-1.iv-agreement",
    "6-1.pssh-agreement",
    "6-4.framemd5",
    "6-2.audio-pcm-identity",
    "6-1.hls-content-gate",
    "6-1.hls-extinf",
    "6-6.dash-conformance",
    "6-1.dash-scheme-cbcs",
    "6-1.dash-playready-sync",
    "6-1.dash-widevine-unchanged",
    "6-1.dash-segmenttemplate-unchanged",
    "6-5.dash-out-of-scope-untouched",
    "6-1.hls-iv-agreement",
    "6-1.hls-segment-count",
    "6-1.hls-version-unchanged",
    "6-2.audio-consolidation-structure",
    "6-5.hls-master-untouched",
];

/// The checks a catalog report may show (`_REPORT_CHECKS`).
pub fn report_checks() -> &'static [&'static str] {
    &REPORT_CHECKS
}

/// `_public_report_checks(checks)`: classifications only (a failure's explanation can carry a
/// host path).
fn public_report_checks(checks: Option<&Value>) -> Option<Vec<Value>> {
    let Some(Value::Object(checks)) = checks else {
        return None;
    };
    Some(
        checks
            .iter()
            .filter(|(check, _)| REPORT_CHECKS.contains(&check.as_str()))
            .map(|(check, value)| {
                let status = match value.as_str() {
                    Some("PASS") => "PASS",
                    Some(text) if text == "FAIL" || text.starts_with("FAIL:") => "FAIL",
                    Some(text) if text == "SKIP" || text.starts_with("SKIP:") => "SKIP",
                    _ => "UNKNOWN",
                };
                let mut item = Map::new();
                item.insert("id".into(), Value::from(check.clone()));
                item.insert("status".into(), Value::from(status));
                Value::Object(item)
            })
            .collect(),
    )
}

/// `_is_safe_rendition_id(value)`: text without a separator or NUL, not `.` / `..`.
fn safe_rendition_id(value: &str) -> bool {
    !value.is_empty() && value != "." && value != ".." && !value.contains(['/', '\\', '\0'])
}

fn catalog_report(app: &Arc<App>, asset_id: &str) -> Result<Value, RouteError> {
    if !catalog::is_safe_asset_id(asset_id)
        || catalog::get_catalog_entry(&app.config.catalog_root, asset_id)?.is_none()
    {
        return Err(HlsApiError::status("result-not-found", 404).into());
    }
    match catalog_report_body(app, asset_id) {
        Ok(body) => Ok(body),
        Err(RouteError::Internal) => Err(RouteError::Internal),
        Err(_) => Err(HlsApiError::status("result-unavailable", 410).into()),
    }
}

/// The `try` block of `catalog_report`: any non-internal failure is `result-unavailable`.
fn catalog_report_body(app: &Arc<App>, asset_id: &str) -> Result<Value, RouteError> {
    let invalid = || RouteError::NotFound;
    let root = pyutil::resolve(&app.config.output_root).map_err(|_| RouteError::Internal)?;
    let asset = pyutil::resolve(&root.join(asset_id)).map_err(|_| RouteError::Internal)?;
    if asset.parent() != Some(root.as_path()) {
        return Err(invalid());
    }
    let path = resolve_within_root("reports/phase1-summary.json", &asset)?;
    let text = std::fs::read(&path)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .ok_or_else(invalid)?;
    let Ok(Value::Object(report)) = serde_json::from_str::<Value>(&text) else {
        return Err(invalid());
    };
    if report.get("schema_version").and_then(Value::as_str) != Some("1")
        || report.get("asset_id").and_then(Value::as_str) != Some(asset_id)
    {
        return Err(invalid());
    }
    let flags = ["all_pass", "full_enumeration", "round_trip_verified"];
    if flags
        .iter()
        .any(|flag| !matches!(report.get(*flag), Some(Value::Bool(_))))
    {
        return Err(invalid());
    }
    let tracks = catalog_tracks(report.get("renditions")).ok_or_else(invalid)?;
    let manifest_checks = match report.get("manifests") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Object(manifests)) => {
            public_report_checks(manifests.get("checks")).ok_or_else(invalid)?
        }
        Some(_) => return Err(invalid()),
    };
    let new_rel = local_manifest_rel(&app.config.output_root, asset_id)?;
    if let Some(rel) = &new_rel {
        resolve_within_root(rel, &root)?;
    }
    let mut answer = Map::new();
    answer.insert("asset_id".into(), Value::from(asset_id));
    answer.insert("report_kind".into(), Value::from("catalog"));
    for flag in flags {
        answer.insert(flag.into(), report[flag].clone());
    }
    answer.insert("production_verified".into(), Value::Bool(false));
    answer.insert("tracks".into(), Value::Array(tracks));
    answer.insert("checks".into(), Value::Array(manifest_checks));
    answer.insert(
        "dash_url".into(),
        new_rel
            .filter(|rel| !rel.is_empty())
            .map_or(Value::Null, |rel| Value::from(format!("/output/{rel}"))),
    );
    answer.insert("hls_url".into(), Value::Null);
    Ok(Value::Object(answer))
}

fn catalog_tracks(renditions: Option<&Value>) -> Option<Vec<Value>> {
    let items: Vec<Value> = match renditions? {
        Value::Array(items) => items.clone(),
        Value::String(text) => text.chars().map(|c| Value::from(c.to_string())).collect(),
        Value::Object(map) => map.keys().cloned().map(Value::from).collect(),
        _ => return None,
    };
    let mut tracks = Vec::with_capacity(items.len());
    for track in items {
        let Value::Object(track) = track else {
            return None;
        };
        let rendition = track
            .get("rendition_id")
            .and_then(Value::as_str)
            .filter(|id| safe_rendition_id(id))?;
        let content_type = track
            .get("content_type")
            .and_then(Value::as_str)
            .filter(|kind| ["video", "audio", "text"].contains(kind))?;
        let mut public = Map::new();
        public.insert("rep_id".into(), Value::from(rendition));
        public.insert("content_type".into(), Value::from(content_type));
        public.insert(
            "checks".into(),
            Value::Array(public_report_checks(track.get("checks"))?),
        );
        for name in [
            "source_segment_count",
            "output_segment_count",
            "requested_segment_duration_s",
        ] {
            let Some(Value::Number(number)) = track.get(name) else {
                return None;
            };
            let integer = number.is_i64() || number.is_u64();
            let valid = if name == "requested_segment_duration_s" {
                true
            } else {
                integer
            };
            let finite = number
                .as_f64()
                .is_some_and(|value| value.is_finite() && value >= 0.0);
            if !valid || !finite {
                return None;
            }
            public.insert(name.into(), Value::Number(number.clone()));
        }
        tracks.push(Value::Object(public));
    }
    Some(tracks)
}

fn catalog_report_page(app: &Arc<App>, req: &Req, asset_id: &str) -> Answer {
    if !catalog::is_safe_asset_id(asset_id)
        || catalog::get_catalog_entry(&app.config.catalog_root, asset_id)?.is_none()
    {
        return Err(HlsApiError::status("result-not-found", 404).into());
    }
    match resolve_within_root(
        &format!("{asset_id}/reports/phase1-summary.json"),
        &app.config.output_root,
    ) {
        Ok(_) => {}
        Err(RouteError::NotFound) => {
            return Err(HlsApiError::status("result-unavailable", 410).into());
        }
        Err(error) => return Err(error),
    }
    page(app, req, "report.html", true)
}

fn source(app: &Arc<App>, req: &Req, asset_id: &str, rel: &str) -> Answer {
    let entry = catalog::get_catalog_entry(&app.config.catalog_root, asset_id)?
        .ok_or(RouteError::NotFound)?;
    // Rooted at the entry's discovered directory, never `catalog_root / asset_id`.
    let full = resolve_within_root(rel, &entry.entry_dir.join("download"))?;
    send_disk_file(req, &full, mime_for(&full))
}

fn dash_old(app: &Arc<App>, req: &Req) -> Answer {
    let asset_id = req.arg("asset_id").unwrap_or_default();
    let entry = catalog::get_catalog_entry(&app.config.catalog_root, asset_id)?
        .ok_or(RouteError::NotFound)?;
    let bytes = std::fs::read(&entry.mpd_path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => RouteError::NotFound,
        _ => RouteError::Internal,
    })?;
    // `read_text(encoding="utf-8")`: strict, universal newlines.
    let text = String::from_utf8(bytes)
        .map_err(|_| RouteError::Internal)?
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    // `_source_route_for_url(asset_id, old)`: the `ValueError` `urlparse` can raise escapes the
    // localization (a 500); the callback cannot fail, so the error is carried out.
    let mut unparseable = false;
    let localized = manifest_dash::localize_dash_manifest_http(&text, |old| {
        let Ok(parsed) = pyutil::urlparse(old) else {
            unparseable = true;
            return String::new();
        };
        let path = if parsed.path.is_empty() {
            "/"
        } else {
            parsed.path.as_str()
        };
        format!("/source/{asset_id}/{}{path}", parsed.netloc)
    });
    match localized {
        Ok(text) if !unparseable => Ok(Reply::bytes(
            200,
            &http::content_type("application/dash+xml"),
            text.into_bytes(),
        )),
        _ => Err(RouteError::Internal),
    }
}

fn output(app: &Arc<App>, req: &Req, asset_id: &str, rel: &str) -> Answer {
    if !catalog::is_safe_asset_id(asset_id) {
        return Err(RouteError::NotFound);
    }
    if app.service.asset_reserved(asset_id) && !app.service.output_published(asset_id)? {
        return Err(RouteError::NotFound);
    }
    let root = pyutil::resolve(&app.config.output_root).map_err(|_| RouteError::Internal)?;
    let asset_root = pyutil::resolve(&root.join(asset_id)).map_err(|_| RouteError::Internal)?;
    let full = resolve_within_root(rel, &asset_root)?;
    // The subtree is checked on the resolved path: neither `cbcs/../clear/…` nor a symlink
    // from `cbcs/` into `clear/` reaches the decrypted copies.
    let subtree = full
        .strip_prefix(&asset_root)
        .ok()
        .and_then(|relative| relative.components().next())
        .map(|component| component.as_os_str().to_string_lossy().into_owned());
    if !matches!(subtree.as_deref(), Some("cbcs" | "manifests")) {
        return Err(RouteError::NotFound);
    }
    send_disk_file(req, &full, mime_for(&full))
}

fn clearkey_license(app: &Arc<App>, req: &Req) -> Answer {
    // A POSTed challenge has been drained (never inspected): the asset comes from the query.
    let asset_id = req.arg("asset_id").unwrap_or_default();
    let Some(entry) = catalog::get_catalog_entry(&app.config.catalog_root, asset_id)? else {
        return Ok(Reply::json_error(404, "unknown asset_id"));
    };
    let mut jwk = Map::new();
    jwk.insert("kty".into(), Value::from("oct"));
    jwk.insert(
        "k".into(),
        Value::from(b64url_hex(entry.key_material.key_hex())),
    );
    jwk.insert(
        "kid".into(),
        Value::from(b64url_hex(entry.key_material.key_id_hex())),
    );
    let mut body = Map::new();
    body.insert("keys".into(), Value::Array(vec![Value::Object(jwk)]));
    body.insert("type".into(), Value::from("temporary"));
    let mut reply = Reply::bytes(
        200,
        "application/json",
        pyutil::dumps(&Value::Object(body)).into_bytes(),
    );
    reply.set_header("Cache-Control", "no-store");
    reply.set_header("Pragma", "no-cache");
    Ok(reply)
}

/// Stream a slice of a file in chunks (the body of a `send_file` answer).
pub fn file_body(path: PathBuf, start: u64, length: u64) -> Body {
    use std::io::{Read, Seek, SeekFrom};
    let (sender, receiver) = tokio::sync::mpsc::channel(8);
    let spawned = std::thread::Builder::new()
        .name("file-body".into())
        .spawn(move || {
            let mut file = match std::fs::File::open(&path) {
                Ok(file) => file,
                Err(error) => {
                    let _ = sender.blocking_send(Err(error));
                    return;
                }
            };
            if let Err(error) = file.seek(SeekFrom::Start(start)) {
                let _ = sender.blocking_send(Err(error));
                return;
            }
            let mut remaining = length;
            let mut buffer = vec![0u8; 64 * 1024];
            while remaining > 0 {
                let want = usize::try_from(remaining.min(64 * 1024)).unwrap_or(64 * 1024);
                match file.read(&mut buffer[..want]) {
                    Ok(0) => break,
                    Ok(read) => {
                        remaining -= u64::try_from(read).unwrap_or(remaining);
                        if sender.blocking_send(Ok(buffer[..read].to_vec())).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = sender.blocking_send(Err(error));
                        return;
                    }
                }
            }
        });
    drop(spawned);
    Body::Stream(receiver)
}
