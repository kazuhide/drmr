//! Source requests, fetch budgets, network rules, and configuration validation.

use super::SourceError;
use crate::concurrency::default_workers;
use serde_json::{Map, Value};
use std::fmt;
use std::path::PathBuf;

/// Resources fetched at once, at most.
pub const MAX_CONNECTIONS: usize = 64;
/// Segments inspected at once, at most, when given explicitly.
pub const MAX_PROBE_WORKERS: usize = 64;
const FORBIDDEN_HEADERS: [&str; 4] = ["host", "range", "connection", "content-length"];

fn invalid() -> SourceError {
    SourceError::new("invalid-fetch-policy")
}

/// Where the source is: an `https`/`http` URL, a `file:` URL, or a local path.
///
/// `Debug` never shows the URL (signed URLs carry credentials).
#[derive(Clone, PartialEq, Eq)]
pub struct SourceRequest {
    /// The master or media playlist.
    pub url: String,
}

impl SourceRequest {
    /// A request for `url`.
    pub fn new(url: impl Into<String>) -> Self {
        SourceRequest { url: url.into() }
    }
}

impl fmt::Debug for SourceRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SourceRequest")
    }
}

/// The bounds of one analysis (`source::FetchPolicy`), with the Python defaults.
///
/// `Debug` never shows `headers_by_origin` (credentials).
#[derive(Clone, PartialEq)]
pub struct FetchPolicy {
    /// Origins (`scheme://host[:port]`, lower case, default port omitted) a remote resource
    /// may come from, redirects included.
    pub allowed_origins: Vec<String>,
    /// Request headers per origin, in order (credentials; never sent to another origin).
    pub headers_by_origin: Vec<(String, Vec<(String, String)>)>,
    /// Directories a local (`file:` or path) resource may come from.
    pub allowed_local_roots: Vec<PathBuf>,
    /// Whether `http` is allowed besides `https`.
    pub allow_http: bool,
    /// Whether a host may resolve to an address that is not globally reachable.
    pub allow_private_network: bool,
    /// Bytes per playlist.
    pub max_playlist_bytes: u64,
    /// Playlists per analysis, the master included.
    pub max_playlists: u64,
    /// Segments per media playlist.
    pub max_segments_per_track: u64,
    /// Bytes per media resource.
    pub max_resource_bytes: u64,
    /// Bytes per analysis.
    pub max_total_bytes: u64,
    /// Free disk space that must remain.
    pub min_free_disk_bytes: u64,
    /// Seconds per request's headers and per body chunk; a body's whole deadline is one
    /// `timeout` per MiB of its declared length, one at least.
    pub timeout: f64,
    /// Attempts per resource (1–5).
    pub attempts: u32,
    /// Resources fetched at once across every playlist (1–64).
    pub connections: usize,
    /// Segments inspected at once (1–64); `None` is the CPUs the process may run on.
    pub probe_workers: Option<usize>,
    /// Resources per analysis, including playlists and initialization segments.
    /// `None` leaves only the playlist and per-track budgets in effect.
    /// This bound limits the download plan before fetching begins.
    pub max_total_resources: Option<u64>,
}

impl Default for FetchPolicy {
    fn default() -> Self {
        FetchPolicy {
            allowed_origins: Vec::new(),
            headers_by_origin: Vec::new(),
            allowed_local_roots: Vec::new(),
            allow_http: false,
            allow_private_network: false,
            max_playlist_bytes: 4 * 1024 * 1024,
            max_playlists: 512,
            max_segments_per_track: 100_000,
            max_resource_bytes: 256 * 1024 * 1024,
            max_total_bytes: 10 * 1024 * 1024 * 1024,
            min_free_disk_bytes: 0,
            timeout: 30.0,
            attempts: 3,
            connections: 16,
            probe_workers: None,
            max_total_resources: None,
        }
    }
}

impl fmt::Debug for FetchPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FetchPolicy")
            .field("allowed_origins", &self.allowed_origins)
            .field("allowed_local_roots", &self.allowed_local_roots)
            .field("allow_http", &self.allow_http)
            .field("allow_private_network", &self.allow_private_network)
            .field("max_playlist_bytes", &self.max_playlist_bytes)
            .field("max_playlists", &self.max_playlists)
            .field("max_segments_per_track", &self.max_segments_per_track)
            .field("max_resource_bytes", &self.max_resource_bytes)
            .field("max_total_bytes", &self.max_total_bytes)
            .field("min_free_disk_bytes", &self.min_free_disk_bytes)
            .field("timeout", &self.timeout)
            .field("attempts", &self.attempts)
            .field("connections", &self.connections)
            .field("probe_workers", &self.probe_workers)
            .field("max_total_resources", &self.max_total_resources)
            .finish_non_exhaustive()
    }
}

impl FetchPolicy {
    /// `__post_init__`: every limit a positive integer, `attempts` at most 5, `connections`
    /// at most 64, `timeout` finite and positive, `probe_workers` 1–64 when given, the
    /// resource budget positive when given, and every header for an allowed origin, named as
    /// an HTTP token other than `Host`, `Range`, `Connection` or `Content-Length`, its value
    /// without a line break.
    pub fn validate(&self) -> Result<(), SourceError> {
        let limits = [
            self.max_playlist_bytes,
            self.max_playlists,
            self.max_segments_per_track,
            self.max_resource_bytes,
            self.max_total_bytes,
            u64::from(self.attempts),
            self.connections as u64,
        ];
        if limits.contains(&0)
            || self.attempts > 5
            || self.connections > MAX_CONNECTIONS
            || !self.timeout.is_finite()
            || self.timeout <= 0.0
        {
            return Err(invalid());
        }
        if self
            .probe_workers
            .is_some_and(|workers| !(1..=MAX_PROBE_WORKERS).contains(&workers))
            || self.max_total_resources == Some(0)
        {
            return Err(invalid());
        }
        for (origin, headers) in &self.headers_by_origin {
            if !self.allowed_origins.contains(origin) {
                return Err(invalid());
            }
            for (name, value) in headers {
                let token = !name.is_empty()
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+.^_`|~-".contains(c));
                if !token
                    || FORBIDDEN_HEADERS.contains(&name.to_lowercase().as_str())
                    || value.contains(['\r', '\n'])
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }

    /// `effective_probe_workers`: the explicit value, else `concurrency::default_workers`.
    pub fn effective_probe_workers(&self) -> usize {
        self.probe_workers.unwrap_or_else(default_workers)
    }

    /// The headers configured for `origin`, in order.
    pub fn headers_for(&self, origin: &str) -> Vec<(String, String)> {
        self.headers_by_origin
            .iter()
            .find(|(name, _)| name == origin)
            .map(|(_, headers)| headers.clone())
            .unwrap_or_default()
    }

    /// The policy a source config's `policy` object describes (`hls_cli.fetch_policy` then
    /// `FetchPolicy(**policy)`): unknown keys, a limit that is not an integer (a boolean or a
    /// float included), a timeout that is not a number, a header map that is not a map of
    /// string maps — all `invalid-fetch-policy`. `allow_http` and `allow_private_network`
    /// take any value's truth; `allowed_origins` and `allowed_local_roots`
    /// take any iterable (a string is its characters, an object its keys, as `tuple` makes
    /// them).
    pub fn from_json(value: &Value) -> Result<FetchPolicy, SourceError> {
        let entries = policy_entries(value)?;
        let mut policy = FetchPolicy::default();
        for (key, item) in &entries {
            set_field(&mut policy, key, item)?;
        }
        policy.validate()?;
        Ok(policy)
    }
}

/// `dict(data)`: an object, or an array of `[key, value]` pairs.
fn policy_entries(value: &Value) -> Result<Vec<(String, Value)>, SourceError> {
    match value {
        Value::Object(map) => Ok(map
            .iter()
            .map(|(key, item)| (key.clone(), item.clone()))
            .collect()),
        Value::Array(items) => {
            let mut map = Map::new();
            for item in items {
                match item {
                    Value::Array(pair) if pair.len() == 2 => {
                        let Value::String(key) = &pair[0] else {
                            return Err(invalid());
                        };
                        map.insert(key.clone(), pair[1].clone());
                    }
                    _ => return Err(invalid()),
                }
            }
            Ok(map.into_iter().collect())
        }
        _ => Err(invalid()),
    }
}

fn set_field(policy: &mut FetchPolicy, key: &str, item: &Value) -> Result<(), SourceError> {
    match key {
        "allowed_origins" => policy.allowed_origins = strings_of(item, true)?,
        "allowed_local_roots" => {
            policy.allowed_local_roots = strings_of(item, false)?
                .into_iter()
                .map(PathBuf::from)
                .collect();
        }
        "headers_by_origin" => policy.headers_by_origin = headers_of(item)?,
        "allow_http" => policy.allow_http = truthy(item),
        "allow_private_network" => policy.allow_private_network = truthy(item),
        "max_playlist_bytes" => policy.max_playlist_bytes = positive(item)?,
        "max_playlists" => policy.max_playlists = positive(item)?,
        "max_segments_per_track" => policy.max_segments_per_track = positive(item)?,
        "max_resource_bytes" => policy.max_resource_bytes = positive(item)?,
        "max_total_bytes" => policy.max_total_bytes = positive(item)?,
        "min_free_disk_bytes" => policy.min_free_disk_bytes = integer(item)?,
        "timeout" => policy.timeout = number(item)?,
        "attempts" => {
            policy.attempts = u32::try_from(positive(item)?).map_err(|_| invalid())?;
        }
        "connections" => {
            policy.connections = usize::try_from(positive(item)?).map_err(|_| invalid())?;
        }
        "probe_workers" => {
            policy.probe_workers = match item {
                Value::Null => None,
                _ => Some(usize::try_from(positive(item)?).map_err(|_| invalid())?),
            };
        }
        "max_total_resources" => {
            policy.max_total_resources = match item {
                Value::Null => None,
                _ => Some(positive(item)?),
            };
        }
        _ => return Err(invalid()),
    }
    Ok(())
}

/// A JSON integer (never a boolean or a float), non-negative.
fn integer(item: &Value) -> Result<u64, SourceError> {
    match item {
        Value::Number(number) if !number.is_f64() => number.as_u64().ok_or_else(invalid),
        _ => Err(invalid()),
    }
}

fn positive(item: &Value) -> Result<u64, SourceError> {
    match integer(item)? {
        0 => Err(invalid()),
        value => Ok(value),
    }
}

/// An `int` or a `float` that is not a boolean.
fn number(item: &Value) -> Result<f64, SourceError> {
    match item {
        Value::Number(number) => number.as_f64().ok_or_else(invalid),
        _ => Err(invalid()),
    }
}

/// `tuple(value)` as strings: an array's string items, a string's characters, an object's
/// keys. Another item in an array can never equal an origin (`skip_others`: it is dropped)
/// and cannot name a root (`Path(p)` of it is a `TypeError`: refused).
fn strings_of(item: &Value, skip_others: bool) -> Result<Vec<String>, SourceError> {
    match item {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(Ok(text.clone())),
                _ if skip_others => None,
                _ => Some(Err(invalid())),
            })
            .collect(),
        Value::String(text) => Ok(text.chars().map(String::from).collect()),
        Value::Object(map) => Ok(map.keys().cloned().collect()),
        _ => Err(invalid()),
    }
}

/// Request headers per origin, each in order.
type HeadersByOrigin = Vec<(String, Vec<(String, String)>)>;

fn headers_of(item: &Value) -> Result<HeadersByOrigin, SourceError> {
    let Value::Object(origins) = item else {
        return Err(invalid());
    };
    origins
        .iter()
        .map(|(origin, headers)| {
            let Value::Object(headers) = headers else {
                return Err(invalid());
            };
            let headers = headers
                .iter()
                .map(|(name, value)| match value {
                    Value::String(text) => Ok((name.clone(), text.clone())),
                    _ => Err(invalid()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((origin.clone(), headers))
        })
        .collect()
}

/// Python's truth of a JSON value.
fn truthy(item: &Value) -> bool {
    match item {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}
