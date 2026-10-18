//! Clear HLS analysis, packaging jobs and playback sessions.

use crate::catalog;
use crate::config::{Clock, ServerConfig};
use crate::jobs::{ConversionJobManager, JobError, JobHandle};
use crate::pyutil::{self, ResolveError};
use base64::Engine as _;
use drmr_core::config::KeyMaterial;
use drmr_core::hls_cli;
use drmr_core::hls_pipeline::DrmSignaling;
use drmr_core::source::{Failure, SourceAnalysis, SourceTrack};
use indexmap::IndexMap;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// `HlsApiError(code, status=422, detail=None)`: the JSON error answer `{**detail, "error": code}`.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("{code}")]
pub struct HlsApiError {
    /// The error token.
    pub code: &'static str,
    /// The HTTP status.
    pub status: u16,
    /// Extra members of the answer (`asset_id` for the queue's conflicts).
    pub detail: Map<String, Value>,
}

impl HlsApiError {
    /// A 422 with this token.
    pub fn new(code: &'static str) -> Self {
        HlsApiError {
            code,
            status: 422,
            detail: Map::new(),
        }
    }

    /// This token with this status.
    pub fn status(code: &'static str, status: u16) -> Self {
        HlsApiError {
            code,
            status,
            detail: Map::new(),
        }
    }

    /// This token, status and detail.
    pub fn with_detail(code: &'static str, status: u16, detail: Map<String, Value>) -> Self {
        HlsApiError {
            code,
            status,
            detail,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// An `HlsApiError` answer.
    #[error(transparent)]
    Api(#[from] HlsApiError),
    /// An internal error (500).
    #[error("internal error")]
    Internal,
}

impl From<ResolveError> for ServiceError {
    fn from(_: ResolveError) -> Self {
        ServiceError::Internal
    }
}

/// `_component(value)`: `[A-Za-z0-9][A-Za-z0-9_.-]{0,127}` (the clear-HLS id rule).
pub fn is_component(value: &str) -> bool {
    crate::jobs::safe_job_id(value)
}

/// `_catalog_id(value)`: the catalog's rule (`[A-Za-z0-9_-][A-Za-z0-9._-]*`).
pub fn is_catalog_id(value: &str) -> bool {
    catalog::is_safe_asset_id(value)
}

fn is_hls_kind(kind: &str) -> bool {
    matches!(kind, "analysis" | "package")
}

/// `_asset_id_valid(value, kind)`.
fn asset_id_valid(value: &str, kind: &str) -> bool {
    if kind == "convert" {
        is_catalog_id(value)
    } else {
        is_component(value)
    }
}

/// One job record as the service holds it.
#[derive(Clone, Debug)]
struct Row {
    job_id: String,
    kind: String,
    state: String,
    created_at: String,
    asset_id: String,
    analysis_id: Option<String>,
    result_id: Option<String>,
    exit_code: Option<i64>,
    stop_requested: bool,
    hidden: bool,
    /// A terminal state whose write failed: retried on every later refresh.
    unsaved: bool,
    /// Request configs deleted once the job ends.
    private_files: Vec<PathBuf>,
}

impl Row {
    /// `{key: row.get(key) for key in _PUBLIC_FIELDS}`.
    fn public(&self) -> Map<String, Value> {
        let text = |value: &Option<String>| value.clone().map_or(Value::Null, Value::from);
        let mut map = Map::new();
        map.insert("job_id".into(), Value::from(self.job_id.clone()));
        map.insert("kind".into(), Value::from(self.kind.clone()));
        map.insert("state".into(), Value::from(self.state.clone()));
        if self.stop_requested {
            map.insert("stop_requested".into(), Value::Bool(true));
        }
        map.insert("created_at".into(), Value::from(self.created_at.clone()));
        map.insert("asset_id".into(), Value::from(self.asset_id.clone()));
        map.insert("analysis_id".into(), text(&self.analysis_id));
        map.insert("result_id".into(), text(&self.result_id));
        map.insert(
            "exit_code".into(),
            self.exit_code.map_or(Value::Null, Value::from),
        );
        map
    }

    /// The record file's body: the public fields, `hidden`, `schema_version`.
    fn persisted(&self, hidden: bool) -> Value {
        let mut map = self.public();
        map.insert("hidden".into(), Value::Bool(hidden));
        map.insert("schema_version".into(), Value::from("1"));
        Value::Object(map)
    }
}

/// `PreviewSession`: a playback session over the frozen source of a result. The KID and key
/// never reach `Debug`; the key is wiped from memory when the session is dropped.
#[derive(Clone)]
pub struct PreviewSession {
    /// The result this session plays.
    pub result_id: String,
    /// Its analysis.
    pub analysis_id: String,
    kid: String,
    key: Zeroizing<String>,
    expires: f64,
    /// `resource id → (kind, value)` in insertion order.
    resources: IndexMap<String, Resource>,
    /// The MIME type of each media resource.
    media_mimes: HashMap<String, String>,
}

impl fmt::Debug for PreviewSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PreviewSession")
            .field("result_id", &self.result_id)
            .field("analysis_id", &self.analysis_id)
            .field("expires", &self.expires)
            .finish_non_exhaustive()
    }
}

impl PreviewSession {
    /// The KID (hex) the license answers with.
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The content key (hex) the license answers with.
    pub fn key(&self) -> &str {
        &self.key
    }
}

#[derive(Clone, Debug)]
enum Resource {
    Master(Arc<SourceAnalysis>),
    Playlist(Box<SourceTrack>),
    Media(PathBuf),
}

/// What a source request answers: a media file to send, or a generated playlist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceAnswer {
    /// `send_file(path, mimetype=…, conditional=True)`.
    Media {
        /// The resolved file.
        path: PathBuf,
        /// Its MIME type.
        mime: String,
    },
    /// An HLS playlist (`application/vnd.apple.mpegurl`).
    Playlist(String),
}

/// A KID and a key held in memory (`pending_keys`, `contexts`); `Debug` prints neither, and
/// the key is wiped when the value is dropped.
#[derive(Clone)]
struct Credentials {
    kid: String,
    key: Zeroizing<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Credentials")
    }
}

#[derive(Debug, Default)]
struct State {
    records: IndexMap<String, Row>,
    published: HashSet<String>,
    pending_keys: HashMap<String, Credentials>,
    contexts: HashMap<String, (Credentials, f64)>,
    sessions: HashMap<String, PreviewSession>,
    expiry_scheduled: bool,
}

/// `HlsService`: the record machinery of every job kind, the clear-HLS analyses, packages,
/// results and playback sessions, and the platform facts.
pub struct HlsService {
    config: Arc<ServerConfig>,
    manager: Arc<ConversionJobManager>,
    output: PathBuf,
    state: Mutex<State>,
    clock: Clock,
    ttl: f64,
    monitor: AtomicBool,
    this: Weak<HlsService>,
    workers: Arc<crate::workers::Workers>,
    // Keep external tool reopen paths stable for the entire service lifetime.
    #[cfg(windows)]
    _output_pin: drmr_core::native::PrivateDirectoryGuard,
}

impl fmt::Debug for HlsService {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HlsService")
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The output root cannot be created.
    #[error("the output root cannot be created")]
    Output(#[source] std::io::Error),
    #[error("a job record is not a JSON object: {0}")]
    Record(PathBuf),
    /// A symlink loop.
    #[error("symlink loop under the output root")]
    Loop,
}

fn monotonic_clock() -> Clock {
    let origin = Instant::now();
    Arc::new(move || origin.elapsed().as_secs_f64())
}

impl HlsService {
    /// `HlsService(config, manager)`: the output root resolved and created, the history loaded.
    pub fn new(
        config: Arc<ServerConfig>,
        manager: Arc<ConversionJobManager>,
    ) -> Result<Arc<HlsService>, StartError> {
        let output = pyutil::resolve(&config.output_root).map_err(|_| StartError::Loop)?;
        #[cfg(not(windows))]
        drmr_core::native::create_private_dir_all(&output).map_err(StartError::Output)?;
        #[cfg(windows)]
        let output_pin =
            drmr_core::native::pin_private_directory(&output).map_err(StartError::Output)?;
        let clock = config
            .hooks
            .hls_clock
            .clone()
            .unwrap_or_else(monotonic_clock);
        let monitor = config.hooks.monitor;
        let service = Arc::new_cyclic(|this| HlsService {
            config,
            manager,
            output,
            state: Mutex::new(State::default()),
            clock,
            ttl: 1800.0,
            monitor: AtomicBool::new(monitor),
            this: this.clone(),
            workers: Arc::new(crate::workers::Workers::default()),
            #[cfg(windows)]
            _output_pin: output_pin,
        });
        {
            let mut state = service.lock();
            service.load_history(&mut state)?;
        }
        Ok(service)
    }

    /// The resolved output root.
    pub fn output(&self) -> &Path {
        &self.output
    }

    /// The job runner.
    pub fn manager(&self) -> &Arc<ConversionJobManager> {
        &self.manager
    }

    /// Turn the job monitor on or off (`service.monitor`).
    pub fn set_monitor(&self, on: bool) {
        self.monitor.store(on, Ordering::SeqCst);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn log(&self, record: &Value) {
        let _ = (self.config.hooks.log)(&pyutil::dumps(record));
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    // --------------------------------------------------------------------------------------
    // Paths and records
    // --------------------------------------------------------------------------------------

    /// `_asset(asset_id, kind)`: the resolved `output/<asset_id>`, directly under the output
    /// root.
    fn asset(&self, asset_id: &str, kind: &str) -> Result<PathBuf, ServiceError> {
        if !asset_id_valid(asset_id, kind) {
            return Err(HlsApiError::new("invalid-asset-id").into());
        }
        let path = pyutil::resolve(&self.output.join(asset_id))?;
        if path.parent() != Some(self.output.as_path()) {
            return Err(HlsApiError::new("unsafe-output-path").into());
        }
        Ok(path)
    }

    /// `_metadata_path(record)`.
    fn metadata_path(&self, row: &Row) -> Result<PathBuf, ServiceError> {
        let prefix = if is_hls_kind(&row.kind) {
            "hls-job-"
        } else {
            "job-"
        };
        Ok(self
            .asset(&row.asset_id, &row.kind)?
            .join("reports")
            .join(format!("{prefix}{}.json", row.job_id)))
    }

    /// `_persist(record)`: the public fields written atomically (temporary file, exclusive
    /// creation, rename); a failed write leaves no temporary file behind.
    fn persist(&self, row: &Row, hidden: bool) -> Result<(), PersistError> {
        let path = self.metadata_path(row).map_err(PersistError::Service)?;
        let asset = self
            .asset(&row.asset_id, &row.kind)
            .map_err(PersistError::Service)?;
        let resolved =
            pyutil::resolve(&path).map_err(|_| PersistError::Service(ServiceError::Internal))?;
        if !pyutil::is_relative_to(&resolved, &asset) {
            return Err(PersistError::Service(
                HlsApiError::new("unsafe-output-path").into(),
            ));
        }
        let parent = path.parent().unwrap_or(&self.output);
        drmr_core::native::create_private_dir_all(parent).map_err(|_| PersistError::Io)?;
        let file_name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let temporary = path.with_file_name(format!("{file_name}.{}.tmp", pyutil::token_hex(4)));
        let written = write_private(&temporary, &row.persisted(hidden))
            .and_then(|()| std::fs::rename(&temporary, &path));
        if written.is_err() {
            let _ = std::fs::remove_file(&temporary);
            return Err(PersistError::Io);
        }
        Ok(())
    }

    /// `_load_history()`: conversion records first, clear-HLS records second (on a duplicate
    /// job id the clear-HLS record wins).
    fn load_history(&self, state: &mut State) -> Result<(), StartError> {
        for prefix in ["job-", "hls-job-"] {
            for path in self.record_files(prefix) {
                if let Some(row) = self.load_record(&path)? {
                    state.records.insert(row.job_id.clone(), row);
                }
            }
        }
        Ok(())
    }

    /// `self.output.glob("*/reports/<prefix>*.json")`.
    #[allow(clippy::case_sensitive_file_extension_comparisons)] // reason: the glob is case-sensitive on POSIX
    fn record_files(&self, prefix: &str) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let Ok(assets) = std::fs::read_dir(&self.output) else {
            return files;
        };
        for asset in assets.flatten() {
            let reports = asset.path().join("reports");
            if !pyutil::is_dir(&asset.path()) || !pyutil::is_dir(&reports) {
                continue;
            }
            let Ok(entries) = std::fs::read_dir(&reports) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(prefix)
                    && name.ends_with(".json")
                    && name.len() >= prefix.len() + 5
                {
                    files.push(entry.path());
                }
            }
        }
        files
    }

    /// One record file of `_load_history`: `None` when it is skipped.
    fn load_record(&self, path: &Path) -> Result<Option<Row>, StartError> {
        let Ok(metadata) = std::fs::metadata(path) else {
            return Ok(None);
        };
        if metadata.len() > 8192 || pyutil::is_symlink(path) {
            return Ok(None);
        }
        let Some(text) = std::fs::read(path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
        else {
            return Ok(None);
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return Ok(None);
        };
        let Value::Object(record) = value else {
            return Err(StartError::Record(path.to_path_buf()));
        };
        let Some(mut row) = self.validate_record(&record, path)? else {
            return Ok(None);
        };
        if row.state == "running" {
            row.state = "interrupted".to_string();
            row.exit_code = None;
            if self.persist(&row, row.hidden).is_err() {
                // Kept for this run (it still protects its outputs and answers the status and
                // log routes); the rewrite is retried on later refreshes.
                row.unsaved = true;
            }
        }
        Ok(Some(row))
    }

    fn validate_record(
        &self,
        record: &Map<String, Value>,
        path: &Path,
    ) -> Result<Option<Row>, StartError> {
        let text = |key: &str| record.get(key).and_then(Value::as_str);
        let (Some(job_id), Some(kind), Some(asset_id), Some(state), Some(created_at)) = (
            text("job_id"),
            text("kind"),
            text("asset_id"),
            text("state"),
            text("created_at"),
        ) else {
            return Ok(None);
        };
        let hidden = match record.get("hidden") {
            None => false,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => return Ok(None),
        };
        let exit_code = match record.get("exit_code") {
            None | Some(Value::Null) => None,
            Some(Value::Number(number)) => match number.as_i64() {
                Some(code) if !number.is_f64() && i32::try_from(code).is_ok() => Some(code),
                _ => return Ok(None),
            },
            Some(_) => return Ok(None),
        };
        let valid = text("schema_version") == Some("1")
            && is_component(job_id)
            && ["analysis", "package", "convert"].contains(&kind)
            && asset_id_valid(asset_id, kind)
            && ["running", "succeeded", "failed", "interrupted"].contains(&state)
            && created_at.chars().count() <= 64;
        if !valid {
            return Ok(None);
        }
        let optional_id = |key: &str| match record.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(value)) if is_component(value) => Ok(Some(value.clone())),
            Some(_) => Err(()),
        };
        let mut row = Row {
            job_id: job_id.to_string(),
            kind: kind.to_string(),
            state: state.to_string(),
            created_at: created_at.to_string(),
            asset_id: asset_id.to_string(),
            analysis_id: None,
            result_id: None,
            exit_code,
            stop_requested: match record.get("stop_requested") {
                None => false,
                Some(Value::Bool(value)) => *value,
                Some(_) => return Ok(None),
            },
            hidden,
            unsaved: false,
            private_files: Vec::new(),
        };
        let expected = match self.metadata_path(&row) {
            Ok(expected) => expected,
            Err(ServiceError::Internal) => return Err(StartError::Loop),
            Err(ServiceError::Api(_)) => return Ok(None),
        };
        let found = pyutil::resolve(path).map_err(|_| StartError::Loop)?;
        if found != pyutil::resolve(&expected).map_err(|_| StartError::Loop)? {
            return Ok(None);
        }
        let (Ok(analysis_id), Ok(result_id)) =
            (optional_id("analysis_id"), optional_id("result_id"))
        else {
            return Ok(None);
        };
        if !pyutil::fromisoformat_valid(created_at) {
            return Ok(None);
        }
        row.analysis_id = analysis_id;
        row.result_id = result_id;
        Ok(Some(row))
    }

    /// `_record(handle, kind, asset_id, analysis_id)`: a new record, written; for a conversion
    /// a failed write keeps the in-memory row (marked unsaved), for clear HLS it is dropped.
    fn record(
        &self,
        state: &mut State,
        handle: &JobHandle,
        kind: &str,
        asset_id: &str,
        analysis_id: Option<&str>,
    ) -> Result<(), ServiceError> {
        let row = Row {
            job_id: handle.job_id.clone(),
            kind: kind.to_string(),
            state: handle.state.to_string(),
            created_at: drmr_core::events::now_iso(),
            asset_id: asset_id.to_string(),
            analysis_id: is_hls_kind(kind)
                .then(|| analysis_id.unwrap_or(&handle.job_id).to_string()),
            result_id: (kind == "package").then(|| handle.job_id.clone()),
            exit_code: handle.exit_code.map(i64::from),
            stop_requested: handle.stop_requested,
            hidden: false,
            unsaved: false,
            private_files: Vec::new(),
        };
        state.records.insert(handle.job_id.clone(), row.clone());
        match self.persist(&row, false) {
            Ok(()) => Ok(()),
            Err(error) => {
                if kind == "convert" {
                    if let Some(row) = state.records.get_mut(&handle.job_id) {
                        row.unsaved = true;
                    }
                } else {
                    state.records.shift_remove(&handle.job_id);
                }
                Err(error.into_service())
            }
        }
    }

    /// `asset_reserved(asset_id)`: clear-HLS work owns this id (a conversion never reserves).
    pub fn asset_reserved(&self, asset_id: &str) -> bool {
        let state = self.lock();
        state
            .records
            .values()
            .any(|row| row.asset_id == asset_id && is_hls_kind(&row.kind))
    }

    /// `owns(job_id)`.
    pub fn owns(&self, job_id: &str) -> bool {
        self.lock().records.contains_key(job_id)
    }

    /// `kind(job_id)`.
    pub fn kind(&self, job_id: &str) -> Option<String> {
        self.lock().records.get(job_id).map(|row| row.kind.clone())
    }

    /// `record_conversion(handle, asset_id)`: register a launched catalog conversion. `None`
    /// when no record could be kept (logged; the job runs either way).
    pub fn record_conversion(
        &self,
        handle: &JobHandle,
        asset_id: &str,
    ) -> Option<Map<String, Value>> {
        let mut state = self.lock();
        let mut failed = false;
        let mut kept = false;
        if is_catalog_id(asset_id) {
            if self
                .record(&mut state, handle, "convert", asset_id, None)
                .is_err()
            {
                failed = true;
            }
            kept = state.records.contains_key(&handle.job_id);
        }
        if !kept || failed {
            let mut event = Map::new();
            event.insert("event".into(), Value::from("conversion-record-failed"));
            event.insert("job_id".into(), Value::from(handle.job_id.clone()));
            event.insert("asset_id".into(), Value::from(asset_id));
            event.insert("retained".into(), Value::Bool(kept));
            self.log(&Value::Object(event));
        }
        if !kept {
            return None;
        }
        drop(state);
        self.watch(&handle.job_id);
        self.lock().records.get(&handle.job_id).map(Row::public)
    }

    /// `running_job()`: the public record of the job that is running, if any.
    pub fn running_job(&self) -> Option<Map<String, Value>> {
        let mut state = self.lock();
        let keys: Vec<String> = state.records.keys().cloned().collect();
        for key in keys {
            if let Ok(public) = self.refresh(&mut state, &key)
                && public.get("state").and_then(Value::as_str) == Some("running")
            {
                return Some(public);
            }
        }
        None
    }

    /// `_refresh(job_id)`: the record brought up to date with the job runner; a terminal state
    /// is written (retried while it fails), the request configs deleted, a successful package's
    /// key kept as a playback context.
    fn refresh(&self, state: &mut State, job_id: &str) -> Result<Map<String, Value>, ServiceError> {
        self.refresh_with_status(state, job_id, || self.manager.status(job_id))
    }

    fn refresh_with_status(
        &self,
        state: &mut State,
        job_id: &str,
        status: impl FnOnce() -> Result<JobHandle, JobError>,
    ) -> Result<Map<String, Value>, ServiceError> {
        let Some(row) = state.records.get(job_id).cloned() else {
            return Err(HlsApiError::status("job-not-found", 404).into());
        };
        let mut row = row;
        if row.unsaved && row.state != "running" && self.persist(&row, row.hidden).is_ok() {
            row.unsaved = false;
        }
        if row.state == "running" {
            match status() {
                Ok(handle) => {
                    row.state = handle.state.to_string();
                    row.exit_code = handle.exit_code.map(i64::from);
                    if handle.stop_requested && !row.stop_requested {
                        row.stop_requested = true;
                        row.unsaved = self.persist(&row, row.hidden).is_err();
                    }
                }
                Err(JobError::CleanupUnconfirmed) => {
                    // The live supervisor still owns this job and its request files.
                    return Err(HlsApiError::status("cleanup-unconfirmed", 503).into());
                }
                Err(_) => {
                    // A prior server run's job is no longer owned by this manager.
                    row.state = "interrupted".to_string();
                    row.exit_code = None;
                }
            }
            if row.state != "running" {
                let keys = state.pending_keys.remove(job_id);
                if let Some(keys) = keys
                    && row.state == "succeeded"
                {
                    state
                        .contexts
                        .insert(job_id.to_string(), (keys, self.now() + self.ttl));
                    self.schedule_expiry(state);
                }
                if self.persist(&row, row.hidden).is_ok() {
                    row.unsaved = false;
                } else {
                    // The outcome is still answered from memory for this run; the write is
                    // retried on every later refresh.
                    row.unsaved = true;
                    let mut event = Map::new();
                    event.insert("event".into(), Value::from("job-record-write-failed"));
                    event.insert("job_id".into(), Value::from(job_id));
                    event.insert("asset_id".into(), Value::from(row.asset_id.clone()));
                    self.log(&Value::Object(event));
                }
                // Request configs can carry signed URLs or license metadata: retain neither.
                row.private_files
                    .retain(|path| match std::fs::remove_file(path) {
                        Ok(()) => false,
                        Err(error) => error.kind() != std::io::ErrorKind::NotFound,
                    });
            }
        }
        if row.state != "running" {
            row.private_files
                .retain(|path| match std::fs::remove_file(path) {
                    Ok(()) => false,
                    Err(error) => error.kind() != std::io::ErrorKind::NotFound,
                });
        }
        let public = row.public();
        state.records.insert(job_id.to_string(), row);
        Ok(public)
    }

    /// `status(job_id)`.
    pub fn status(&self, job_id: &str) -> Result<Map<String, Value>, ServiceError> {
        let mut state = self.lock();
        self.expire(&mut state);
        self.refresh(&mut state, job_id)
    }

    /// `_watch(job_id)`: a monitor thread refreshes the record every 0.25 s until the job is no
    /// longer running (so its terminal state is written even when nobody asks).
    fn watch(&self, job_id: &str) {
        if !self.monitor.load(Ordering::SeqCst) {
            return;
        }
        let this = self.this.clone();
        let job_id = job_id.to_string();
        let spawned = self.workers.spawn("hls-job-monitor", move || {
            loop {
                let Some(service) = this.upgrade() else {
                    return;
                };
                match service.status(&job_id) {
                    Ok(public)
                        if public.get("state").and_then(Value::as_str) == Some("running") => {}
                    _ => return,
                }
                if !service.workers.wait(Duration::from_millis(250)) {
                    return;
                }
                drop(service);
            }
        });
        drop(spawned);
    }

    /// Confirm terminal records and release private requests and playback credentials.
    /// The caller must first close submissions and confirm the manager's child cleanup.
    pub fn shutdown(&self) -> Result<(), ServiceError> {
        self.monitor.store(false, Ordering::SeqCst);
        let workers_ok = self.workers.shutdown().is_ok();
        let mut state = self.lock();
        let keys: Vec<String> = state.records.keys().cloned().collect();
        let mut records_ok = true;
        for key in keys {
            records_ok &= self.refresh(&mut state, &key).is_ok();
        }
        records_ok &= state
            .records
            .values()
            .all(|row| row.state != "running" && !row.unsaved && row.private_files.is_empty());
        state.pending_keys.clear();
        state.contexts.clear();
        state.sessions.clear();
        state.expiry_scheduled = false;
        if workers_ok && records_ok {
            Ok(())
        } else {
            Err(HlsApiError::status("service-cleanup-unconfirmed", 503).into())
        }
    }

    // --------------------------------------------------------------------------------------
    // Expiry of playback state
    // --------------------------------------------------------------------------------------

    /// `_expire()`: sessions and contexts past their time are forgotten.
    fn expire(&self, state: &mut State) {
        let now = self.now();
        state.sessions.retain(|_, session| session.expires > now);
        state.contexts.retain(|_, (_, expires)| *expires > now);
    }

    /// `_schedule_expiry()`: a timer every 30 s while any session or context remains, so that
    /// key material leaves memory even when no request arrives.
    fn schedule_expiry(&self, state: &mut State) {
        if state.expiry_scheduled {
            return;
        }
        state.expiry_scheduled = true;
        let this = self.this.clone();
        let spawned = self.workers.spawn("hls-expiry", move || {
            if let Some(service) = this.upgrade() {
                if !service.workers.wait(Duration::from_secs(30)) {
                    return;
                }
                let mut state = service.lock();
                state.expiry_scheduled = false;
                service.expire(&mut state);
                if !state.contexts.is_empty() || !state.sessions.is_empty() {
                    service.schedule_expiry(&mut state);
                }
            }
        });
        if spawned.is_err() {
            state.expiry_scheduled = false;
        }
    }

    // --------------------------------------------------------------------------------------
    // Analyses
    // --------------------------------------------------------------------------------------

    /// `_policy(source_url, profile)`: the fetch policy for this source, from the operator's
    /// profile file (or, without one, the source's own HTTPS origin under `default`); local
    /// roots are always emptied. Any problem is `source-profile-invalid`.
    fn policy(&self, source_url: &str, profile: &Value) -> Result<Map<String, Value>, HlsApiError> {
        self.build_policy(source_url, profile)
            .ok_or_else(|| HlsApiError::new("source-profile-invalid"))
    }

    fn build_policy(&self, source_url: &str, profile: &Value) -> Option<Map<String, Value>> {
        let parsed = hls_cli::urlsplit(source_url).ok()?;
        if !matches!(parsed.scheme.as_str(), "https" | "http")
            || parsed.netloc.is_empty()
            || parsed.username().is_some_and(|name| !name.is_empty())
            || parsed.password().is_some_and(|word| !word.is_empty())
            || !parsed.fragment.is_empty()
        {
            return None;
        }
        let origin = format!("{}://{}", parsed.scheme, parsed.netloc);
        let mut data = if let Some(file) = &self.config.hls_profiles {
            let profiles = hls_cli::private_json(file).ok()?;
            let Value::Object(entry) = profiles.get(profile.as_str()?)? else {
                return None;
            };
            hls_cli::py_dict(entry.get("policy").unwrap_or(&Value::Object(Map::new())))?
        } else if profile.as_str() == Some("default") && parsed.scheme == "https" {
            let mut data = Map::new();
            data.insert(
                "allowed_origins".into(),
                Value::Array(vec![Value::from(origin.clone())]),
            );
            data
        } else {
            return None;
        };
        // Browser requests never admit file:// even if an operator's CLI policy does.
        data.insert("allowed_local_roots".into(), Value::Array(Vec::new()));
        let policy = hls_cli::fetch_policy(&Value::Object(data.clone())).ok()?;
        // `origin not in policy.allowed_origins` (a non-text item never equals an origin).
        if !policy.allowed_origins.contains(&origin)
            || (parsed.scheme == "http" && !policy.allow_http)
        {
            return None;
        }
        Some(data)
    }

    /// `_retryable_analysis(asset_id, asset)`: only a known unadmitted tree may be analysed
    /// again (every attempt a failed or interrupted analysis, nothing on disk but their
    /// records and request configs).
    #[allow(clippy::case_sensitive_file_extension_comparisons)] // reason: generated initialization names are case-sensitive
    fn retryable_analysis(
        &self,
        state: &mut State,
        asset_id: &str,
        asset: &Path,
    ) -> Result<bool, ServiceError> {
        let attempts: Vec<Row> = state
            .records
            .values()
            .filter(|row| row.asset_id == asset_id && is_hls_kind(&row.kind))
            .cloned()
            .collect();
        if attempts.is_empty() {
            return Ok(false);
        }
        for row in &attempts {
            if row.kind != "analysis" {
                return Ok(false);
            }
            let public = self.refresh(state, &row.job_id)?;
            if !matches!(
                public.get("state").and_then(Value::as_str),
                Some("failed" | "interrupted")
            ) {
                return Ok(false);
            }
        }
        let mut history = HashSet::new();
        for row in &attempts {
            history.insert(self.metadata_path(row)?);
        }
        let requests = asset.join("clear").join("_requests");
        let allowed = [asset.join("reports"), asset.join("clear"), requests.clone()];
        let mut stack = vec![asset.to_path_buf()];
        while let Some(directory) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if pyutil::is_symlink(&path) {
                    return Ok(false);
                }
                if pyutil::is_dir(&path) {
                    if !allowed.contains(&path) {
                        return Ok(false);
                    }
                    stack.push(path);
                } else if !history.contains(&path) {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let request_config = path.parent() == Some(requests.as_path())
                        && name.len() == 37
                        && name.ends_with(".json")
                        && name[..32]
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
                    if !request_config {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    /// `analyze(body)`: launch `analyze-hls` for a new (or retryable) clear-HLS asset; answers
    /// `{job_id, analysis_id}`.
    pub fn analyze(&self, body: &Map<String, Value>) -> Result<Value, ServiceError> {
        if body
            .keys()
            .any(|key| !["asset_id", "url", "access_profile"].contains(&key.as_str()))
        {
            return Err(HlsApiError::new("source-parameters-invalid").into());
        }
        let asset_id = match body.get("asset_id") {
            Some(Value::String(id)) if is_component(id) => id.clone(),
            _ => return Err(HlsApiError::new("source-parameters-invalid").into()),
        };
        let source_url = match body.get("url") {
            Some(Value::String(url)) if url.chars().count() <= 16_384 => url.clone(),
            _ => return Err(HlsApiError::new("source-parameters-invalid").into()),
        };
        let default_profile = Value::from("default");
        let policy = self.policy(
            &source_url,
            body.get("access_profile").unwrap_or(&default_profile),
        )?;
        let mut state = self.lock();
        let asset = self.asset(&asset_id, "analysis")?;
        let existing = pyutil::exists(&asset);
        let in_catalog = catalog::discover_catalog(&self.config.catalog_root, None)
            .map_err(|_| ServiceError::Internal)?
            .iter()
            .any(|entry| entry.asset_id == asset_id);
        if in_catalog || (existing && !self.retryable_analysis(&mut state, &asset_id, &asset)?) {
            return Err(HlsApiError::status("asset-already-exists", 409).into());
        }
        let requests = asset.join("clear").join("_requests");
        if existing && let Ok(entries) = std::fs::read_dir(&requests) {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().ends_with(".json") {
                    std::fs::remove_file(entry.path()).map_err(|_| ServiceError::Internal)?;
                }
            }
        }
        let path = requests.join(format!("{}.json", pyutil::token_hex(16)));
        let mut config = Map::new();
        config.insert("url".into(), Value::from(source_url));
        config.insert("policy".into(), Value::Object(policy));
        write_private(&path, &Value::Object(config)).map_err(|_| ServiceError::Internal)?;
        let arguments: Vec<OsString> = vec![
            "analyze-hls".into(),
            "--source-config".into(),
            path.clone().into_os_string(),
            "--asset-id".into(),
            asset_id.clone().into(),
            "--output".into(),
            self.output.clone().into_os_string(),
        ];
        let handle = match self
            .manager
            .submit_command(&arguments, &self.manager.fresh_env())
        {
            Ok(handle) => handle,
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                if !existing {
                    // No child ever owned this newly created tree.
                    let _ = std::fs::remove_dir_all(&asset);
                }
                return Err(match error {
                    JobError::AlreadyRunning => HlsApiError::status("job-busy", 409),
                    _ => HlsApiError::status("job-launch-failed", 503),
                }
                .into());
            }
        };
        self.record(&mut state, &handle, "analysis", &asset_id, None)?;
        if let Some(row) = state.records.get_mut(&handle.job_id) {
            row.private_files = vec![path];
        }
        let analysis_id = state
            .records
            .get(&handle.job_id)
            .and_then(|row| row.analysis_id.clone());
        drop(state);
        self.watch(&handle.job_id);
        let mut answer = Map::new();
        answer.insert("job_id".into(), Value::from(handle.job_id));
        answer.insert(
            "analysis_id".into(),
            analysis_id.map_or(Value::Null, Value::from),
        );
        Ok(Value::Object(answer))
    }

    /// `_analysis_record(analysis_id)`.
    fn analysis_record(state: &State, analysis_id: &Value) -> Result<Row, HlsApiError> {
        state
            .records
            .values()
            .find(|row| {
                row.kind == "analysis"
                    && row
                        .analysis_id
                        .as_deref()
                        .is_some_and(|id| Some(id) == analysis_id.as_str())
            })
            .cloned()
            .ok_or_else(|| HlsApiError::status("analysis-not-found", 404))
    }

    /// `_read_analysis(record)`: the frozen analysis of this record's asset, or
    /// `analysis-unavailable` (410).
    fn read_analysis(&self, row: &Row) -> Result<SourceAnalysis, ServiceError> {
        let asset = self.asset(&row.asset_id, "analysis")?;
        let path = asset.join("clear").join("source").join("analysis.json");
        let unavailable = || ServiceError::from(HlsApiError::status("analysis-unavailable", 410));
        let resolved = pyutil::resolve(&path)?;
        if !pyutil::is_relative_to(&resolved, &asset) {
            return Err(unavailable());
        }
        // `except (OSError, ValueError, KeyError, TypeError)`: a `SourceError` (a `ValueError`)
        // and an escaping exception of those classes are 410; any other class (an
        // `AttributeError` on a record that is not an object, an `OverflowError`) is a 500.
        match drmr_core::source::read_analysis(&path) {
            Ok(analysis) if analysis.asset_id == row.asset_id => Ok(analysis),
            Ok(_) | Err(Failure::Source(_)) => Err(unavailable()),
            Err(Failure::Escaped(escaped))
                if matches!(
                    escaped.class,
                    "OSError" | "ValueError" | "KeyError" | "TypeError"
                ) =>
            {
                Err(unavailable())
            }
            Err(Failure::Escaped(_) | Failure::Cancelled) => Err(ServiceError::Internal),
        }
    }

    /// `analysis(analysis_id)`: the public analysis with the audio-selection facts, or the
    /// state of an analysis that has not succeeded.
    pub fn analysis(&self, analysis_id: &str) -> Result<Value, ServiceError> {
        let mut state = self.lock();
        let row = Self::analysis_record(&state, &Value::from(analysis_id))?;
        self.refresh(&mut state, &row.job_id)?;
        let row = state.records.get(&row.job_id).cloned().unwrap_or(row);
        if row.state != "succeeded" {
            let mut finding = Map::new();
            finding.insert("severity".into(), Value::from("error"));
            finding.insert(
                "code".into(),
                Value::from(format!("analysis-{}", row.state)),
            );
            let mut answer = Map::new();
            answer.insert("analysis_id".into(), Value::from(analysis_id));
            answer.insert("asset_id".into(), Value::from(row.asset_id.clone()));
            answer.insert("state".into(), Value::from(row.state.clone()));
            answer.insert("admitted".into(), Value::Bool(false));
            answer.insert("tracks".into(), Value::Array(Vec::new()));
            answer.insert(
                "findings".into(),
                Value::Array(vec![Value::Object(finding)]),
            );
            return Ok(Value::Object(answer));
        }
        let analysis = self.read_analysis(&row)?;
        let mut result = analysis.to_public_dict();
        result.insert("analysis_id".into(), Value::from(analysis_id));
        result.insert("state".into(), Value::from("succeeded"));
        let candidates = audio_candidates(&analysis);
        let groups: HashSet<&str> = candidates.iter().map(|t| t.group_id.as_str()).collect();
        if multiple_group_selection(&candidates, &groups) {
            let mut finding = Map::new();
            finding.insert("severity".into(), Value::from("error"));
            finding.insert(
                "code".into(),
                Value::from("multiple-audio-group-selection-required"),
            );
            result.insert("admitted".into(), Value::Bool(false));
            result.insert(
                "findings".into(),
                Value::Array(vec![Value::Object(finding)]),
            );
        }
        result.insert(
            "requires_audio_selection".into(),
            Value::Bool(groups.len() == 1 && candidates.len() > 1),
        );
        let listed = if groups.len() == 1 {
            candidates
                .iter()
                .map(|track| {
                    let mut item = Map::new();
                    item.insert("id".into(), Value::from(track.id.clone()));
                    let label = if track.name.is_empty() {
                        &track.id
                    } else {
                        &track.name
                    };
                    item.insert("label".into(), Value::from(label.clone()));
                    Value::Object(item)
                })
                .collect()
        } else {
            Vec::new()
        };
        result.insert("audio_candidates".into(), Value::Array(listed));
        Ok(Value::Object(result))
    }

    // --------------------------------------------------------------------------------------
    // Packages
    // --------------------------------------------------------------------------------------

    /// `package(body)`: launch `package-hls` for a succeeded, admitted analysis with the
    /// operator's three DRM signalings; the key and IV reach the child through its environment
    /// only. Answers `{job_id}`.
    pub fn package(&self, body: &Map<String, Value>) -> Result<Value, ServiceError> {
        if body
            .keys()
            .any(|key| !["analysis_id", "selection", "drm"].contains(&key.as_str()))
        {
            return Err(HlsApiError::new("package-parameters-invalid").into());
        }
        let empty = Value::Object(Map::new());
        let (keys, signaling) = hls_cli::drm_parameters(body.get("drm").unwrap_or(&empty), None)
            .map_err(|_| HlsApiError::new("drm-parameters-invalid"))?;
        let mut state = self.lock();
        let row = Self::analysis_record(&state, body.get("analysis_id").unwrap_or(&Value::Null))?;
        let packages: Vec<String> = state
            .records
            .values()
            .filter(|record| record.kind == "package" && record.asset_id == row.asset_id)
            .map(|record| record.job_id.clone())
            .collect();
        for job_id in packages {
            let public = self.refresh(&mut state, &job_id)?;
            if public.get("state").and_then(Value::as_str) == Some("succeeded") {
                return Err(HlsApiError::status("result-already-exists", 409).into());
            }
        }
        self.refresh(&mut state, &row.job_id)?;
        let row = state.records.get(&row.job_id).cloned().unwrap_or(row);
        if row.state != "succeeded" {
            return Err(HlsApiError::new("analysis-not-ready").into());
        }
        let analysis = self.read_analysis(&row)?;
        let choice = check_selection(body.get("selection"), &row, &analysis)?;
        let request_dir = self
            .asset(&row.asset_id, "analysis")?
            .join("clear")
            .join("_requests");
        let (drm_path, selection_path) =
            write_package_configs(&request_dir, &signaling, &analysis, choice)?;
        let handle = self.launch_package(&keys, &analysis, &drm_path, &selection_path)?;
        self.record(
            &mut state,
            &handle,
            "package",
            &row.asset_id,
            row.analysis_id.as_deref(),
        )?;
        if let Some(new) = state.records.get_mut(&handle.job_id) {
            new.private_files = vec![drm_path, selection_path];
        }
        state.pending_keys.insert(
            handle.job_id.clone(),
            Credentials {
                kid: keys.key_id_hex().to_string(),
                key: Zeroizing::new(keys.key_hex().to_string()),
            },
        );
        drop(state);
        self.watch(&handle.job_id);
        let mut answer = Map::new();
        answer.insert("job_id".into(), Value::from(handle.job_id));
        Ok(Value::Object(answer))
    }

    /// `package-hls` under the lease with the key material in a fresh environment; the request
    /// configs are deleted when the launch fails (`job-busy` 409, `job-launch-failed` 503).
    fn launch_package(
        &self,
        keys: &KeyMaterial,
        analysis: &SourceAnalysis,
        drm_path: &Path,
        selection_path: &Path,
    ) -> Result<JobHandle, ServiceError> {
        let mut env = self.manager.fresh_env();
        env.push(("DRM_KID".into(), keys.key_id_hex().into()));
        env.push(("DRM_KEY".into(), keys.key_hex().into()));
        env.push(("DRM_IV".into(), keys.iv_hex().into()));
        let mut command: Vec<OsString> = vec![
            "package-hls".into(),
            "--analysis".into(),
            analysis.snapshot_path.clone().into_os_string(),
            "--drm-config".into(),
            drm_path.as_os_str().to_os_string(),
            "--selection".into(),
            selection_path.as_os_str().to_os_string(),
        ];
        if let Some(workers) = self.config.max_workers {
            command.push("--max-workers".into());
            command.push(workers.to_string().into());
        }
        self.manager
            .submit_command(&command, &env)
            .map_err(|error| {
                let _ = std::fs::remove_file(drm_path);
                let _ = std::fs::remove_file(selection_path);
                match error {
                    JobError::AlreadyRunning => HlsApiError::status("job-busy", 409),
                    _ => HlsApiError::status("job-launch-failed", 503),
                }
                .into()
            })
    }

    // --------------------------------------------------------------------------------------
    // History
    // --------------------------------------------------------------------------------------

    /// `jobs(cursor, limit)`: the visible records, newest first, paged by an opaque cursor.
    pub fn jobs(&self, cursor: Option<&str>, limit: i128) -> Result<Value, ServiceError> {
        if !(1..=50).contains(&limit) {
            return Err(HlsApiError::status("pagination-invalid", 400).into());
        }
        let limit = usize::try_from(limit).unwrap_or(50);
        let mut state = self.lock();
        self.expire(&mut state);
        let keys: Vec<String> = state
            .records
            .iter()
            .filter(|(_, row)| !row.hidden)
            .map(|(key, _)| key.clone())
            .collect();
        let mut rows = Vec::with_capacity(keys.len());
        for key in keys {
            rows.push(self.refresh(&mut state, &key)?);
        }
        let sort_key = |row: &Map<String, Value>| {
            (
                row.get("created_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                row.get("job_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        };
        rows.sort_by_key(|row| std::cmp::Reverse(sort_key(row)));
        if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
            let anchor = decode_cursor(cursor)
                .ok_or_else(|| HlsApiError::status("pagination-invalid", 400))?;
            rows.retain(|row| sort_key(row) < anchor);
        }
        let next_cursor = if rows.len() > limit {
            let last = sort_key(&rows[limit - 1]);
            let text = pyutil::dumps(&Value::Array(vec![
                Value::from(last.0),
                Value::from(last.1),
            ]));
            Value::from(
                base64::engine::general_purpose::URL_SAFE
                    .encode(text)
                    .trim_end_matches('=')
                    .to_string(),
            )
        } else {
            Value::Null
        };
        rows.truncate(limit);
        let mut answer = Map::new();
        answer.insert(
            "jobs".into(),
            Value::Array(rows.into_iter().map(Value::Object).collect()),
        );
        answer.insert("next_cursor".into(), next_cursor);
        Ok(Value::Object(answer))
    }

    /// `delete_job(job_id)`: hide a terminal record from the history (written first, so a
    /// failed write leaves it visible and retryable); outputs and logs are kept.
    pub fn delete_job(&self, job_id: &str) -> Result<(), ServiceError> {
        let mut state = self.lock();
        if !is_component(job_id) {
            return Err(HlsApiError::status("job-not-found", 404).into());
        }
        let public = self.refresh(&mut state, job_id)?;
        if public.get("state").and_then(Value::as_str) == Some("running") {
            return Err(HlsApiError::status("job-running-cannot-delete", 409).into());
        }
        let row = state
            .records
            .get(job_id)
            .cloned()
            .ok_or(ServiceError::Internal)?;
        if !row.hidden {
            self.persist(&row, true)
                .map_err(PersistError::into_service)?;
            if let Some(row) = state.records.get_mut(job_id) {
                row.hidden = true;
            }
        }
        Ok(())
    }

    // --------------------------------------------------------------------------------------
    // Results and playback
    // --------------------------------------------------------------------------------------

    /// `_result_record(result_id)`.
    fn result_record(&self, state: &mut State, result_id: &Value) -> Result<Row, ServiceError> {
        let result_id = match result_id {
            Value::String(id) if is_component(id) => id.clone(),
            _ => return Err(HlsApiError::new("result-parameters-invalid").into()),
        };
        match state.records.get(&result_id) {
            Some(row) if row.kind == "package" => {}
            _ => return Err(HlsApiError::status("result-not-found", 404).into()),
        }
        self.refresh(state, &result_id)?;
        let row = state
            .records
            .get(&result_id)
            .cloned()
            .ok_or(ServiceError::Internal)?;
        if row.state != "succeeded" {
            return Err(HlsApiError::status("result-unavailable", 410).into());
        }
        Ok(row)
    }

    /// The report page's check (`/reports/hls/<id>`): the result is known and succeeded.
    pub fn check_result_record(&self, result_id: &str) -> Result<(), ServiceError> {
        let mut state = self.lock();
        self.result_record(&mut state, &Value::from(result_id))
            .map(|_| ())
    }

    /// `result(result_id)`: the validated public result of a succeeded package.
    pub fn result(&self, result_id: &str) -> Result<Value, ServiceError> {
        let mut state = self.lock();
        self.result_locked(&mut state, result_id)
    }

    fn result_locked(&self, state: &mut State, result_id: &str) -> Result<Value, ServiceError> {
        let row = self.result_record(state, &Value::from(result_id))?;
        let asset = self.asset(&row.asset_id, "package")?;
        let (tracks, track_reports, checks) = match self.validated_result(state, &row, &asset) {
            Ok(parts) => parts,
            Err(ServiceError::Internal) => return Err(ServiceError::Internal),
            Err(ServiceError::Api(_)) => {
                return Err(HlsApiError::status("result-unavailable", 410).into());
            }
        };
        state.published.insert(row.asset_id.clone());
        let base = format!("/output/{}/manifests/clear-hls", row.asset_id);
        let mut answer = Map::new();
        answer.insert("result_id".into(), Value::from(result_id));
        answer.insert("asset_id".into(), Value::from(row.asset_id.clone()));
        answer.insert(
            "analysis_id".into(),
            row.analysis_id.clone().map_or(Value::Null, Value::from),
        );
        answer.insert("created_at".into(), Value::from(row.created_at.clone()));
        answer.insert("all_pass".into(), Value::Bool(true));
        answer.insert("tracks".into(), Value::Array(tracks));
        answer.insert("track_reports".into(), Value::Array(track_reports));
        answer.insert("checks".into(), Value::Array(checks));
        answer.insert(
            "dash_url".into(),
            Value::from(format!("{base}/manifest.mpd")),
        );
        answer.insert("hls_url".into(), Value::from(format!("{base}/master.m3u8")));
        answer.insert("production_verified".into(), Value::Bool(false));
        Ok(Value::Object(answer))
    }

    /// The `try` block of `result()`: any `Api` error here becomes `result-unavailable`.
    fn validated_result(
        &self,
        state: &State,
        row: &Row,
        asset: &Path,
    ) -> Result<ResultParts, ServiceError> {
        let invalid = || ServiceError::from(HlsApiError::new("result-invalid"));
        let report_path = asset.join("reports").join("package-hls-summary.json");
        if !pyutil::is_relative_to(&pyutil::resolve(&report_path)?, asset) {
            return Err(invalid());
        }
        let text = std::fs::read(&report_path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or_else(invalid)?;
        let Ok(Value::Object(report)) = serde_json::from_str::<Value>(&text) else {
            return Err(invalid());
        };
        let analysis_row = Self::analysis_record(
            state,
            &row.analysis_id.clone().map_or(Value::Null, Value::from),
        )
        .map_err(ServiceError::from)?;
        let analysis = self.read_analysis(&analysis_row)?;
        if report.get("success") != Some(&Value::Bool(true))
            || report.get("analysis_id").and_then(Value::as_str)
                != Some(analysis.analysis_id.as_str())
        {
            return Err(invalid());
        }
        let folder = asset.join("manifests").join("clear-hls");
        if !["manifest.mpd", "master.m3u8"]
            .iter()
            .all(|name| pyutil::is_file(&folder.join(name)))
        {
            return Err(invalid());
        }
        let Some(Value::Array(report_tracks)) = report.get("tracks") else {
            return Err(invalid());
        };
        let mut disposition: HashMap<String, &Map<String, Value>> = HashMap::new();
        for track in report_tracks {
            let Value::Object(track) = track else {
                return Err(invalid());
            };
            match track.get("source_track_id") {
                Some(Value::String(id)) => {
                    disposition.insert(id.clone(), track);
                }
                Some(Value::Array(_) | Value::Object(_)) | None => return Err(invalid()),
                Some(_) => {}
            }
        }
        let mut tracks = Vec::new();
        let mut checks = Vec::new();
        let mut track_reports = Vec::new();
        for track in &analysis.tracks {
            let terminal = disposition.get(&track.id).ok_or_else(invalid)?;
            let kind = match terminal.get("disposition") {
                Some(Value::String(kind))
                    if kind == "packaged" || kind == "duplicate-not-selected" =>
                {
                    kind.clone()
                }
                _ => return Err(invalid()),
            };
            let track_checks = track_checks(terminal.get("checks")).ok_or_else(invalid)?;
            for check in &track_checks {
                checks.push(check_value(&format!("{}:{check}", track.id)));
            }
            let segments = public_segments(terminal.get("segments")).ok_or_else(invalid)?;
            let mut report_row = Map::new();
            report_row.insert("source_track_id".into(), Value::from(track.id.clone()));
            report_row.insert("disposition".into(), Value::from(kind.clone()));
            report_row.insert(
                "checks".into(),
                Value::Array(
                    track_checks
                        .iter()
                        .map(|check| check_value(check))
                        .collect(),
                ),
            );
            report_row.insert("segments".into(), Value::Array(segments));
            track_reports.push(Value::Object(report_row));
            if kind == "packaged" {
                let mut public = track.to_public_dict();
                public.insert("rep_id".into(), Value::from(track.id.clone()));
                if matches!(track.content_type.as_str(), "video" | "audio") {
                    let init = asset.join("cbcs").join(&track.id).join("init.mp4");
                    // `OSError` reading it or `HlsMediaError`: both are `result-unavailable`.
                    let codec = drmr_core::media::codec_from_init(&init, &track.codec)
                        .map_err(|_| invalid())?;
                    public.insert("codec".into(), Value::from(codec));
                }
                tracks.push(Value::Object(public));
            }
        }
        Ok((tracks, track_reports, checks))
    }

    /// `_kid(row)`: the one `cenc:default_KID` of the generated MPD, as 32 lower-case hex
    /// digits.
    fn result_kid(&self, row: &Row) -> Result<String, ServiceError> {
        let unavailable = || ServiceError::from(HlsApiError::status("result-unavailable", 410));
        let path = self
            .asset(&row.asset_id, "package")
            .map_err(|_| unavailable())?
            .join("manifests")
            .join("clear-hls")
            .join("manifest.mpd");
        let text = std::fs::read(&path)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or_else(unavailable)?;
        let options = roxmltree::ParsingOptions {
            allow_dtd: true,
            ..roxmltree::ParsingOptions::default()
        };
        let document =
            roxmltree::Document::parse_with_options(&text, options).map_err(|_| unavailable())?;
        let values: HashSet<String> = document
            .descendants()
            .filter_map(|node| node.attribute(("urn:mpeg:cenc:2013", "default_KID")))
            .map(|value| value.replace('-', "").to_lowercase())
            .collect();
        let mut values = values.into_iter();
        match (values.next(), values.next()) {
            (Some(kid), None)
                if kid.len() == 32
                    && kid
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
            {
                Ok(kid)
            }
            _ => Err(unavailable()),
        }
    }

    /// `create_session(body)`: a playback session over the frozen source of a result, keyed
    /// by the context of a fresh package or by the KID and key the operator re-enters.
    pub fn create_session(&self, body: &Map<String, Value>) -> Result<Value, ServiceError> {
        if body
            .keys()
            .any(|key| !["result_id", "kid", "key"].contains(&key.as_str()))
        {
            return Err(HlsApiError::new("playback-parameters-invalid").into());
        }
        let mut state = self.lock();
        self.expire(&mut state);
        let result_value = body.get("result_id").cloned().unwrap_or(Value::Null);
        let row = self.result_record(&mut state, &result_value)?;
        let result_id = row.job_id.clone();
        self.result_locked(&mut state, &result_id)?;
        let expected = self.result_kid(&row)?;
        let context = state.contexts.get(&result_id).cloned();
        let credentials = if body.contains_key("kid") || body.contains_key("key") {
            let invalid = || ServiceError::from(HlsApiError::new("playback-credentials-invalid"));
            let (Some(Value::String(kid)), Some(Value::String(key))) =
                (body.get("kid"), body.get("key"))
            else {
                return Err(invalid());
            };
            let kid = kid.replace('-', "").to_lowercase();
            let key = key.to_lowercase();
            let hex32 = key.len() == 32
                && key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if kid != expected || !hex32 {
                return Err(invalid());
            }
            Credentials {
                kid,
                key: Zeroizing::new(key),
            }
        } else if let Some((credentials, _)) = context {
            credentials
        } else {
            return Err(HlsApiError::status("playback-credentials-required", 410).into());
        };
        let analysis_row = Self::analysis_record(
            &state,
            &row.analysis_id.clone().map_or(Value::Null, Value::from),
        )?;
        let analysis = Arc::new(self.read_analysis(&analysis_row)?);
        let session_id = pyutil::token_hex(24);
        let mut session = PreviewSession {
            result_id: result_id.clone(),
            analysis_id: row.analysis_id.clone().unwrap_or_default(),
            kid: credentials.kid.clone(),
            key: credentials.key.clone(),
            expires: self.now() + self.ttl,
            resources: IndexMap::new(),
            media_mimes: HashMap::new(),
        };
        source_resources(&mut session, &analysis)?;
        state.sessions.insert(session_id.clone(), session);
        self.schedule_expiry(&mut state);
        state
            .contexts
            .insert(result_id.clone(), (credentials, self.now() + self.ttl));
        let prefix = format!("/api/hls/playback/{session_id}");
        let new_url = self
            .result_locked(&mut state, &result_id)?
            .get("dash_url")
            .cloned()
            .unwrap_or(Value::Null);
        let renditions: Vec<Value> = analysis
            .tracks
            .iter()
            .map(|track| {
                let mut public = track.to_public_dict();
                public.insert("rep_id".into(), Value::from(track.id.clone()));
                Value::Object(public)
            })
            .collect();
        let mut answer = Map::new();
        answer.insert("session_id".into(), Value::from(session_id));
        answer.insert(
            "old_url".into(),
            Value::from(format!("{prefix}/source/master")),
        );
        answer.insert("new_url".into(), new_url);
        answer.insert(
            "license_url".into(),
            Value::from(format!("{prefix}/license")),
        );
        answer.insert("source_renditions".into(), Value::Array(renditions));
        Ok(Value::Object(answer))
    }

    /// `session(session_id)`: a live session, its lifetime (and its result's context)
    /// extended; `playback-session-expired` (410) otherwise.
    pub fn session(&self, session_id: &str) -> Result<PreviewSession, HlsApiError> {
        let mut state = self.lock();
        self.expire(&mut state);
        let expires = self.now() + self.ttl;
        let Some(session) = state.sessions.get_mut(session_id) else {
            return Err(HlsApiError::status("playback-session-expired", 410));
        };
        session.expires = expires;
        let session = session.clone();
        state.contexts.insert(
            session.result_id.clone(),
            (
                Credentials {
                    kid: session.kid.clone(),
                    key: session.key.clone(),
                },
                expires,
            ),
        );
        Ok(session)
    }

    /// `revoke(session_id)`: the session ends; the result's context goes with its last session.
    pub fn revoke(&self, session_id: &str) {
        let mut state = self.lock();
        if let Some(session) = state.sessions.remove(session_id)
            && !state
                .sessions
                .values()
                .any(|other| other.result_id == session.result_id)
        {
            state.contexts.remove(&session.result_id);
        }
    }

    /// `source(session_id, resource_id)`: a media file of the frozen source, or a playlist
    /// generated over it that names only opaque resource ids.
    pub fn source(
        &self,
        session_id: &str,
        resource_id: &str,
    ) -> Result<SourceAnswer, ServiceError> {
        let session = self.session(session_id)?;
        let resource = session
            .resources
            .get(resource_id)
            .ok_or_else(|| HlsApiError::status("source-resource-not-found", 404))?;
        let prefix = format!("/api/hls/playback/{session_id}/source/");
        match resource {
            Resource::Media(path) => Ok(SourceAnswer::Media {
                path: path.clone(),
                mime: session
                    .media_mimes
                    .get(resource_id)
                    .cloned()
                    .unwrap_or_default(),
            }),
            Resource::Playlist(track) => {
                media_playlist(&session, track, &prefix).map(SourceAnswer::Playlist)
            }
            Resource::Master(analysis) => {
                Ok(SourceAnswer::Playlist(master_playlist(analysis, &prefix)))
            }
        }
    }

    /// `output_published(asset_id)`: a package of this asset has a valid result.
    pub fn output_published(&self, asset_id: &str) -> Result<bool, ServiceError> {
        let mut state = self.lock();
        if state.published.contains(asset_id) {
            return Ok(true);
        }
        let packages: Vec<String> = state
            .records
            .values()
            .filter(|row| row.asset_id == asset_id && row.kind == "package")
            .map(|row| row.job_id.clone())
            .collect();
        for job_id in packages {
            match self.result_locked(&mut state, &job_id) {
                Ok(_) => return Ok(true),
                Err(ServiceError::Api(_)) => {}
                Err(ServiceError::Internal) => return Err(ServiceError::Internal),
            }
        }
        Ok(false)
    }

    /// `platform()`: the instance's facts for the page (roots, profiles without credentials,
    /// limits).
    pub fn platform(&self) -> Result<Value, ServiceError> {
        let profiles = self.config.hls_profiles.as_ref().map(|file| {
            let Ok(data) = hls_cli::private_json(file) else {
                return Vec::new();
            };
            let mut names: Vec<&String> = data.keys().collect();
            names.sort();
            names
                .into_iter()
                .filter(|name| is_component(name))
                .filter_map(|name| {
                    let Value::Object(entry) = &data[name.as_str()] else {
                        return None;
                    };
                    let empty = Map::new();
                    let policy = match entry.get("policy") {
                        Some(Value::Object(policy)) => policy,
                        _ => &empty,
                    };
                    let origins: Vec<Value> = match policy.get("allowed_origins") {
                        Some(Value::Array(items)) => items
                            .iter()
                            .filter_map(Value::as_str)
                            .filter(|origin| platform_origin(origin))
                            .map(Value::from)
                            .collect(),
                        _ => Vec::new(),
                    };
                    let mut profile = Map::new();
                    profile.insert("name".into(), Value::from(name.clone()));
                    profile.insert("allowed_origins".into(), Value::Array(origins));
                    profile.insert(
                        "allow_http".into(),
                        Value::Bool(policy.get("allow_http") == Some(&Value::Bool(true))),
                    );
                    Some(Value::Object(profile))
                })
                .collect()
        });
        let catalog_root = pyutil::resolve(&self.config.catalog_root)?;
        let mut answer = Map::new();
        answer.insert(
            "catalog_root".into(),
            Value::from(pyutil::path_text(&catalog_root)),
        );
        answer.insert(
            "output_root".into(),
            Value::from(pyutil::path_text(&self.output)),
        );
        answer.insert(
            "jobs_dir".into(),
            Value::from(pyutil::path_text(&self.output.join("_jobs"))),
        );
        answer.insert("concurrency".into(), Value::from(1));
        answer.insert(
            "batch_available".into(),
            Value::Bool(self.config.batch_available),
        );
        answer.insert(
            "profiles".into(),
            profiles.map_or(Value::Null, Value::Array),
        );
        answer.insert("conventions_version".into(), Value::from("1"));
        Ok(Value::Object(answer))
    }
}

/// A validated result's public parts: `tracks`, `track_reports`, `checks`.
type ResultParts = (Vec<Value>, Vec<Value>, Vec<Value>);

/// A failed `_persist`: an API error from the path checks, or the write itself (`OSError`).
#[derive(Debug)]
enum PersistError {
    Service(ServiceError),
    Io,
}

impl PersistError {
    fn into_service(self) -> ServiceError {
        match self {
            PersistError::Service(error) => error,
            PersistError::Io => ServiceError::Internal,
        }
    }
}

/// `_write_private(path, body)`: parent directories created, the file created exclusively with
/// mode 0600 (never following an existing file or symlink), `json.dump` of the body.
pub fn write_private(path: &Path, body: &Value) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = path.parent() {
        drmr_core::native::create_private_dir_all(parent)?;
    }
    let mut file = drmr_core::native::create_private_new(path)?;
    file.write_all(pyutil::dumps(body).as_bytes())
}

fn write_package_configs(
    request_dir: &Path,
    signaling: &DrmSignaling,
    analysis: &SourceAnalysis,
    choice: Value,
) -> Result<(PathBuf, PathBuf), ServiceError> {
    let drm_path = request_dir.join(format!("{}.json", pyutil::token_hex(16)));
    let selection_path = request_dir.join(format!("{}.json", pyutil::token_hex(16)));
    let mut drm = Map::new();
    drm.insert(
        "widevine_pssh".into(),
        Value::from(base64::engine::general_purpose::STANDARD.encode(&signaling.widevine_pssh)),
    );
    drm.insert(
        "playready_la_url".into(),
        Value::from(signaling.playready_la_url.clone()),
    );
    drm.insert(
        "fairplay_uri".into(),
        Value::from(signaling.fairplay_uri.clone()),
    );
    write_private(&drm_path, &Value::Object(drm)).map_err(|_| ServiceError::Internal)?;
    let mut selection = Map::new();
    selection.insert(
        "analysis_id".into(),
        Value::from(analysis.analysis_id.clone()),
    );
    selection.insert(
        "snapshot_digest".into(),
        Value::from(analysis.snapshot_digest.clone()),
    );
    selection.insert("audio_source_id".into(), choice);
    write_private(&selection_path, &Value::Object(selection))
        .map_err(|_| ServiceError::Internal)?;
    Ok((drm_path, selection_path))
}

/// `{"id": id, "status": "PASS"}`.
fn check_value(id: &str) -> Value {
    let mut check = Map::new();
    check.insert("id".into(), Value::from(id));
    check.insert("status".into(), Value::from("PASS"));
    Value::Object(check)
}

/// `_TRACK_CHECKS` among a terminal's checks (strings only); `None` when they are not
/// iterable (`TypeError`).
fn track_checks(value: Option<&Value>) -> Option<Vec<String>> {
    const TRACK_CHECKS: [&str; 7] = [
        "selection-recorded",
        "source-correspondence-pass",
        "timing-pass",
        "decoded-identity-pass",
        "cbcs-signaling-pass",
        "text-payload-pass",
        "text-timing-map-pass",
    ];
    let items: Vec<Value> = match value? {
        Value::Array(items) => items.clone(),
        Value::String(text) => text.chars().map(|c| Value::from(c.to_string())).collect(),
        Value::Object(map) => map.keys().cloned().map(Value::from).collect(),
        _ => return None,
    };
    Some(
        items
            .iter()
            .filter_map(Value::as_str)
            .filter(|check| TRACK_CHECKS.contains(check))
            .map(str::to_string)
            .collect(),
    )
}

/// The public segment rows of a terminal (`_SEGMENT_FIELDS`, non-negative integers but the
/// presentation start); `None` for anything else.
fn public_segments(value: Option<&Value>) -> Option<Vec<Value>> {
    const FIELDS: [&str; 6] = [
        "source_index",
        "output_index",
        "timescale",
        "presentation_start",
        "duration",
        "sample_count",
    ];
    let items: Vec<Value> = match value {
        None => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(Value::String(text)) => text.chars().map(|c| Value::from(c.to_string())).collect(),
        Some(Value::Object(map)) => map.keys().cloned().map(Value::from).collect(),
        Some(_) => return None,
    };
    let mut segments = Vec::with_capacity(items.len());
    for item in items {
        let Value::Object(segment) = item else {
            return None;
        };
        let mut public = Map::new();
        for name in FIELDS {
            let Some(Value::Number(number)) = segment.get(name) else {
                return None;
            };
            if number.is_f64() {
                return None;
            }
            if name != "presentation_start" && number.as_i64().is_some_and(|n| n < 0) {
                return None;
            }
            public.insert(name.into(), Value::Number(number.clone()));
        }
        segments.push(Value::Object(public));
    }
    Some(segments)
}

/// The audio tracks a muxed variant carries (`content_type == "audio" and muxed`).
fn audio_candidates(analysis: &SourceAnalysis) -> Vec<&SourceTrack> {
    analysis
        .tracks
        .iter()
        .filter(|track| track.content_type == "audio" && track.muxed)
        .collect()
}

/// Several groups, one of them with more than one candidate: the pipeline cannot choose.
fn multiple_group_selection(candidates: &[&SourceTrack], groups: &HashSet<&str>) -> bool {
    groups.len() > 1
        && groups.iter().any(|group| {
            candidates
                .iter()
                .filter(|track| track.group_id == *group)
                .count()
                > 1
        })
}

/// The `selection` checks of `package()`; the audio choice to record.
fn check_selection(
    selection: Option<&Value>,
    row: &Row,
    analysis: &SourceAnalysis,
) -> Result<Value, HlsApiError> {
    let stale = || HlsApiError::new("stale-packaging-selection");
    let Some(Value::Object(selection)) = selection else {
        return Err(stale());
    };
    if selection
        .keys()
        .any(|key| !["analysis_id", "snapshot_digest", "audio_source_id"].contains(&key.as_str()))
        || selection.get("analysis_id").and_then(Value::as_str) != row.analysis_id.as_deref()
        || selection
            .get("analysis_id")
            .is_none_or(|id| !id.is_string())
        || selection.get("snapshot_digest").and_then(Value::as_str)
            != Some(analysis.snapshot_digest.as_str())
    {
        return Err(stale());
    }
    let choice = selection
        .get("audio_source_id")
        .cloned()
        .unwrap_or(Value::Null);
    let candidates = audio_candidates(analysis);
    let groups: HashSet<&str> = candidates.iter().map(|t| t.group_id.as_str()).collect();
    if multiple_group_selection(&candidates, &groups) {
        return Err(HlsApiError::new("multiple-audio-group-selection-required"));
    }
    let listed = choice
        .as_str()
        .is_some_and(|id| candidates.iter().any(|track| track.id == id));
    if (!choice.is_null() && !listed)
        || (groups.len() == 1 && candidates.len() > 1 && !hls_cli::truthy(&choice))
        || (groups.len() > 1 && !choice.is_null())
    {
        return Err(HlsApiError::new("audio-selection-required"));
    }
    Ok(choice)
}

/// The first 24 hex digits of the SHA-256 of `text`.
fn short_hash(text: &str, length: usize) -> String {
    let digest = hex::encode(Sha256::digest(text.as_bytes()));
    digest[..length].to_string()
}

/// `_source_resources(session, analysis)`: only the original resources of the frozen graph
/// (never a generated or decrypted artifact), each behind an opaque id.
fn source_resources(
    session: &mut PreviewSession,
    analysis: &Arc<SourceAnalysis>,
) -> Result<(), ServiceError> {
    let root = pyutil::resolve(analysis.snapshot_path.parent().unwrap_or(Path::new("/")))?;
    session
        .resources
        .insert("master".into(), Resource::Master(analysis.clone()));
    for track in &analysis.tracks {
        session.resources.insert(
            format!("track-{}", short_hash(&track.id, 24)),
            Resource::Playlist(Box::new(track.clone())),
        );
        let paths = track
            .init_path
            .iter()
            .chain(track.segments.iter().map(|segment| &segment.path));
        for path in paths {
            let resolved = pyutil::resolve(path)?;
            let suffix = suffix(&resolved);
            if !pyutil::is_relative_to(&resolved, &root)
                || !pyutil::is_file(&resolved)
                || suffix == ".json"
                || suffix == ".m3u8"
            {
                return Err(HlsApiError::status("source-preview-unavailable", 410).into());
            }
            let relative = resolved.strip_prefix(&root).unwrap_or(&resolved);
            let resource_id = format!("media-{}", short_hash(&pyutil::path_text(relative), 32));
            session
                .resources
                .insert(resource_id.clone(), Resource::Media(resolved.clone()));
            let mime = match track.container.as_str() {
                "ts" => "video/mp2t",
                "webvtt" => "text/vtt",
                _ => match track.content_type.as_str() {
                    "video" => "video/mp4",
                    "audio" => "audio/mp4",
                    "text" => "application/mp4",
                    _ => return Err(ServiceError::Internal),
                },
            };
            // A multiplexed resource may appear in logical video and audio tracks: its video
            // use wins; independent audio keeps audio/mp4.
            if session.media_mimes.get(&resource_id).map(String::as_str) != Some("video/mp4") {
                session.media_mimes.insert(resource_id, mime.to_string());
            }
        }
    }
    Ok(())
}

/// `PurePath.suffix`: the last `.xxx` of the name, empty for a dot file or a trailing dot.
fn suffix(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match name.rfind('.') {
        Some(index) if index > 0 && index < name.len() - 1 => name[index..].to_string(),
        _ => String::new(),
    }
}

/// `_hls_attribute(value)`: quotes, line breaks and backslashes removed.
fn hls_attribute(value: &str) -> String {
    value
        .chars()
        .filter(|c| !matches!(c, '"' | '\r' | '\n' | '\\'))
        .collect()
}

/// The media playlist of one track over the session's opaque resource ids.
fn media_playlist(
    session: &PreviewSession,
    track: &SourceTrack,
    prefix: &str,
) -> Result<String, ServiceError> {
    let longest = track
        .segments
        .iter()
        .map(|segment| segment.duration)
        .reduce(f64::max)
        .ok_or(ServiceError::Internal)?;
    let media_url = |path: &Path| -> Result<String, ServiceError> {
        let resolved = pyutil::resolve(path)?;
        session
            .resources
            .iter()
            .find(|(_, resource)| matches!(resource, Resource::Media(media) if *media == resolved))
            .map(|(key, _)| format!("{prefix}{key}"))
            .ok_or(ServiceError::Internal)
    };
    let mut lines = vec![
        "#EXTM3U".to_string(),
        "#EXT-X-VERSION:7".to_string(),
        "#EXT-X-PLAYLIST-TYPE:VOD".to_string(),
        "#EXT-X-MEDIA-SEQUENCE:0".to_string(),
        format!("#EXT-X-TARGETDURATION:{}", ceil_text(longest)),
    ];
    if let Some(init) = &track.init_path {
        lines.push(format!("#EXT-X-MAP:URI=\"{}\"", media_url(init)?));
    }
    for segment in &track.segments {
        lines.push(format!("#EXTINF:{:.9},", segment.duration));
        lines.push(media_url(&segment.path)?);
    }
    lines.push("#EXT-X-ENDLIST".to_string());
    Ok(lines.join("\n") + "\n")
}

/// `str(math.ceil(value))` of a finite float.
#[allow(clippy::cast_possible_truncation)] // reason: a segment duration is far below i64::MAX
fn ceil_text(value: f64) -> String {
    (value.ceil() as i64).to_string()
}

/// The master playlist of the frozen source over the session's opaque playlist ids.
fn master_playlist(analysis: &SourceAnalysis, prefix: &str) -> String {
    let playlist_url = |track: &SourceTrack| format!("{prefix}track-{}", short_hash(&track.id, 24));
    let group_id = |value: &str, kind: &str| format!("{kind}-{}", short_hash(value, 12));
    let yes_no = |flag: bool| if flag { "YES" } else { "NO" };
    let mut lines = vec!["#EXTM3U".to_string(), "#EXT-X-VERSION:7".to_string()];
    // A TS variant already contains its muxed audio: listing extracted logical audio copies as
    // alternatives would play the same TS as an audio-only source.
    let audios: Vec<&SourceTrack> = analysis
        .tracks
        .iter()
        .filter(|track| track.content_type == "audio" && !track.muxed)
        .collect();
    let audio_groups: HashSet<&str> = audios.iter().map(|track| track.group_id.as_str()).collect();
    for track in &audios {
        lines.push(format!(
            "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"{}\",NAME=\"{}\",LANGUAGE=\"{}\",AUTOSELECT={},DEFAULT={},URI=\"{}\"",
            group_id(&track.group_id, "audio"),
            hls_attribute(&track.id),
            hls_attribute(&track.language),
            yes_no(track.autoselect),
            yes_no(track.is_default),
            playlist_url(track),
        ));
    }
    let texts: Vec<&SourceTrack> = analysis
        .tracks
        .iter()
        .filter(|track| track.content_type == "text")
        .collect();
    let text_groups: HashSet<&str> = texts.iter().map(|track| track.group_id.as_str()).collect();
    for track in &texts {
        lines.push(format!(
            "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"{}\",NAME=\"{}\",LANGUAGE=\"{}\",DEFAULT={},AUTOSELECT={},URI=\"{}\"",
            group_id(&track.group_id, "text"),
            hls_attribute(&track.id),
            hls_attribute(&track.language),
            yes_no(track.is_default),
            yes_no(track.autoselect),
            playlist_url(track),
        ));
    }
    let videos: Vec<&SourceTrack> = analysis
        .tracks
        .iter()
        .filter(|track| track.content_type == "video")
        .collect();
    let bandwidth = |track: &SourceTrack| track.bandwidth.filter(|b| *b != 0).unwrap_or(1).max(1);
    for track in &videos {
        let mut attrs = vec![format!("BANDWIDTH={}", bandwidth(track))];
        if let (Some(width), Some(height)) = (
            track.width.filter(|w| *w != 0),
            track.height.filter(|h| *h != 0),
        ) {
            attrs.push(format!("RESOLUTION={width}x{height}"));
        }
        if !track.codecs.is_empty() {
            attrs.push(format!("CODECS=\"{}\"", hls_attribute(&track.codecs)));
        }
        if audio_groups.contains(track.audio_group.as_str()) {
            attrs.push(format!(
                "AUDIO=\"{}\"",
                group_id(&track.audio_group, "audio")
            ));
        }
        if text_groups.contains(track.subtitle_group.as_str()) {
            attrs.push(format!(
                "SUBTITLES=\"{}\"",
                group_id(&track.subtitle_group, "text")
            ));
        }
        lines.push(format!("#EXT-X-STREAM-INF:{}", attrs.join(",")));
        lines.push(playlist_url(track));
    }
    if videos.is_empty() {
        for track in &audios {
            lines.push(format!("#EXT-X-STREAM-INF:BANDWIDTH={}", bandwidth(track)));
            lines.push(playlist_url(track));
        }
    }
    lines.join("\n") + "\n"
}

/// `https?://[A-Za-z0-9.-]+(?::\d{1,5})?` (the platform response admits plain origins only).
fn platform_origin(origin: &str) -> bool {
    let rest = origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"));
    let Some(rest) = rest else {
        return false;
    };
    let (host, port) = match rest.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (rest, None),
    };
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && port.is_none_or(|port| {
            (1..=5).contains(&port.len()) && port.bytes().all(|b| b.is_ascii_digit())
        })
}

/// The cursor of `jobs()`: unpadded URL-safe base64 (decoded leniently, as
/// `urlsafe_b64decode` does) of `json.dumps([created_at, job_id])`.
fn decode_cursor(cursor: &str) -> Option<(String, String)> {
    if !cursor.is_ascii() {
        return None;
    }
    let padding = "=".repeat((4 - cursor.len() % 4) % 4);
    let translated: String = format!("{cursor}{padding}")
        .chars()
        .map(|c| match c {
            '-' => '+',
            '_' => '/',
            other => other,
        })
        .collect();
    let bytes = b64decode_lenient(&translated)?;
    let text = String::from_utf8(bytes).ok()?;
    match serde_json::from_str::<Value>(&text).ok()? {
        Value::Array(items) if items.len() == 2 => match (&items[0], &items[1]) {
            (Value::String(a), Value::String(b)) => Some((a.clone(), b.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// `binascii.a2b_base64(data)` without strict mode: characters outside the alphabet skipped,
/// decoding stopped at a complete pad, an incomplete quantum an error.
fn b64decode_lenient(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut quad_pos, mut left, mut pads) = (0u8, 0u8, 0u8);
    for byte in text.bytes() {
        if byte == b'=' {
            if quad_pos >= 2 {
                pads += 1;
                if quad_pos + pads >= 4 {
                    return Some(out);
                }
            }
            continue;
        }
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => continue,
        };
        pads = 0;
        match quad_pos {
            0 => {
                quad_pos = 1;
                left = value;
            }
            1 => {
                quad_pos = 2;
                out.push(left << 2 | value >> 4);
                left = value & 0x0f;
            }
            2 => {
                quad_pos = 3;
                out.push(left << 4 | value >> 2);
                left = value & 0x03;
            }
            _ => {
                quad_pos = 0;
                out.push(left << 6 | value);
                left = 0;
            }
        }
    }
    (quad_pos == 0).then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfirmed_cleanup_keeps_history_keys_and_request_files_owned() {
        use std::io::Write as _;

        let root = tempfile::tempdir().unwrap();
        let manager = Arc::new(
            ConversionJobManager::new(&root.path().join("jobs"), Vec::new(), Arc::new(Vec::new))
                .unwrap(),
        );
        let mut config = ServerConfig::new(root.path().join("catalog"), root.path().join("output"));
        config.hooks.monitor = false;
        let service = HlsService::new(Arc::new(config), manager).unwrap();
        let request = service.output.join("request.json");
        drmr_core::native::create_private_new(&request)
            .unwrap()
            .write_all(b"private request")
            .unwrap();
        let row = Row {
            job_id: "job".into(),
            kind: "package".into(),
            state: "running".into(),
            created_at: "2026-10-18T00:00:00+00:00".into(),
            asset_id: "asset".into(),
            analysis_id: Some("analysis".into()),
            result_id: Some("job".into()),
            exit_code: None,
            stop_requested: false,
            hidden: false,
            unsaved: false,
            private_files: vec![request.clone()],
        };
        service.persist(&row, false).unwrap();
        let metadata = service.metadata_path(&row).unwrap();
        let before = std::fs::read(&metadata).unwrap();
        let mut state = service.lock();
        state.records.insert("job".into(), row);
        state.pending_keys.insert(
            "job".into(),
            Credentials {
                kid: "00".repeat(16),
                key: Zeroizing::new("11".repeat(16)),
            },
        );
        let result =
            service.refresh_with_status(&mut state, "job", || Err(JobError::CleanupUnconfirmed));
        assert!(matches!(result, Err(ServiceError::Api(ref error))
            if error.code == "cleanup-unconfirmed" && error.status == 503));
        assert_eq!(state.records["job"].state, "running");
        assert_eq!(state.records["job"].exit_code, None);
        assert_eq!(
            state.records["job"].private_files,
            std::slice::from_ref(&request)
        );
        assert!(state.pending_keys.contains_key("job"));
        assert!(!state.contexts.contains_key("job"));
        assert_eq!(std::fs::read(&request).unwrap(), b"private request");
        assert_eq!(std::fs::read(&metadata).unwrap(), before);
    }

    #[cfg(windows)]
    #[test]
    fn the_service_pins_its_output_path_until_the_last_owner_drops() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("output-parent");
        std::fs::create_dir(&parent).unwrap();
        let output = parent.join("output");
        let manager = Arc::new(
            ConversionJobManager::new(&root.path().join("jobs"), Vec::new(), Arc::new(Vec::new))
                .unwrap(),
        );
        let service = HlsService::new(
            Arc::new(ServerConfig::new(root.path().join("catalog"), &output)),
            manager,
        )
        .unwrap();
        let other_owner = service.clone();
        assert!(std::fs::rename(&output, parent.join("redirected-output")).is_err());
        assert!(std::fs::rename(&parent, root.path().join("redirected-parent")).is_err());
        drop(service);
        assert!(std::fs::rename(&output, parent.join("redirected-output")).is_err());
        drop(other_owner);
        std::fs::rename(&parent, root.path().join("released-parent")).unwrap();
    }

    #[test]
    fn cursors_round_trip_and_are_decoded_leniently() {
        let text = pyutil::dumps(&serde_json::json!(["2026-10-16T00:00:00+00:00", "abc"]));
        let cursor = base64::engine::general_purpose::URL_SAFE.encode(text);
        let cursor = cursor.trim_end_matches('=');
        assert_eq!(
            decode_cursor(cursor),
            Some(("2026-10-16T00:00:00+00:00".to_string(), "abc".to_string()))
        );
        assert_eq!(decode_cursor("%%invalid"), None);
        assert_eq!(decode_cursor("é"), None);
    }

    #[test]
    fn platform_origins_are_plain() {
        assert!(
            platform_origin("https://ok.example:8443") && platform_origin("http://127.0.0.1:8098")
        );
        for bad in [
            "https://user:pw@media.example",
            "https://media.example/path",
            "*",
            "ftp://x",
        ] {
            assert!(!platform_origin(bad), "{bad}");
        }
    }

    #[test]
    fn suffix_is_pathlibs() {
        assert_eq!(suffix(Path::new("/a/r1.ts")), ".ts");
        assert_eq!(suffix(Path::new("/a/.hidden")), "");
        assert_eq!(suffix(Path::new("/a/x.")), "");
    }
}
