//! Private, byte-preserving clear-VOD acquisition for the ordinary conversion pipeline.
mod owned;
mod stream;
use owned::OwnedDownload;

use super::{ConversionError, ConversionReport, ConversionRequest, Converter, files};
use crate::{
    concurrency::{Pool, Stop, Unit, run_ordered},
    resources::{Budget, Claim, Hardware, Workload},
    source::{self, FetchPolicy, Fetcher, playlist},
};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fmt,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const INVALID: ConversionError = ConversionError("download-source-not-supported");
const IO: ConversionError = ConversionError("download-source-unavailable");
const CHANGED: ConversionError = ConversionError("download-source-changed");
const GRAPH_BYTES: usize = 128 * 1024 * 1024;

/// A complete retained source graph. Debug deliberately omits URLs and filesystem names.
pub struct DownloadedSource {
    entrypoint: PathBuf,
    seal: Arc<Seal>,
}
impl fmt::Debug for DownloadedSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DownloadedSource")
    }
}
impl DownloadedSource {
    /// The local projection; downloaded manifests remain unchanged in the private raw directory.
    pub fn entrypoint(&self) -> &Path {
        &self.entrypoint
    }
    /// Revalidate creation identities and every retained original without modifying them.
    pub fn check(&self, stop: &Stop) -> Result<(), ConversionError> {
        self.seal.check(stop)
    }
}

pub(super) struct Seal {
    owned: OwnedDownload,
    files: BTreeMap<PathBuf, ([u8; 32], u64)>,
}
impl Seal {
    fn check(&self, stop: &Stop) -> Result<(), ConversionError> {
        self.owned.check_registered().map_err(|_| CHANGED)?;
        let mut names = Vec::new();
        inventory(self.owned.path(), Path::new(""), &mut names, &mut 0)?;
        names.sort();
        if names.len() != self.files.len() {
            return Err(CHANGED);
        }
        for ((name, (hash, size)), actual) in self.files.iter().zip(names) {
            if *name != actual
                || digest(&self.owned.path().join(name), stop, *size)? != (*hash, *size)
            {
                return Err(CHANGED);
            }
        }
        Ok(())
    }
}
fn inventory(
    root: &Path,
    relative: &Path,
    names: &mut Vec<PathBuf>,
    count: &mut usize,
) -> Result<(), ConversionError> {
    let entries = std::fs::read_dir(root.join(relative)).map_err(|_| CHANGED)?;
    for entry in entries {
        *count = count
            .checked_add(1)
            .filter(|count| *count <= 100_010)
            .ok_or(CHANGED)?;
        let entry = entry.map_err(|_| CHANGED)?;
        let name = relative.join(entry.file_name());
        let kind = entry.file_type().map_err(|_| CHANGED)?;
        if kind.is_dir()
            && relative.as_os_str().is_empty()
            && matches!(entry.file_name().to_str(), Some("raw" | "media"))
        {
            inventory(root, &name, names, count)?;
        } else if kind.is_file() {
            names.push(name);
        } else {
            return Err(CHANGED);
        }
    }
    Ok(())
}
fn digest(
    path: &Path,
    stop: &Stop,
    expected_size: u64,
) -> Result<([u8; 32], u64), ConversionError> {
    let mut input = files::regular_input(path).map_err(|_| CHANGED)?;
    if input.metadata().map_err(|_| CHANGED)?.len() != expected_size {
        return Err(CHANGED);
    }
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = vec![0; 64 * 1024];
    loop {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        let count = input.read(&mut buffer).map_err(|_| CHANGED)?;
        if count == 0 {
            return Ok((hash.finalize().into(), size));
        }
        size = size
            .checked_add(count as u64)
            .filter(|size| *size <= expected_size && *size <= super::MAX_MEDIA_BYTES)
            .ok_or(CHANGED)?;
        hash.update(&buffer[..count]);
    }
}

/// Validate an original output spelling under the ordinary local conversion rules.
/// This reads filesystem metadata without reserving paths, writing files, or using the network.
/// Acquisition still reserves its own directory and rechecks native aliases separately.
pub fn preflight_download_output(output: &Path) -> Result<(), ConversionError> {
    super::destination(output).map(|_| ())
}

impl Converter {
    /// Fetch a complete clear local HLS graph into a new directory, retaining original bytes.
    pub fn download_hls(
        &self,
        url: &str,
        directory: &Path,
        output: &Path,
        policy: &FetchPolicy,
    ) -> Result<DownloadedSource, ConversionError> {
        self.acquire_hls_inner(url, directory, Some(output), policy)
    }
    /// Retain a complete clear VOD source before a future output location is chosen.
    pub fn acquire_hls(
        &self,
        url: &str,
        directory: &Path,
        policy: &FetchPolicy,
    ) -> Result<DownloadedSource, ConversionError> {
        self.acquire_hls_inner(url, directory, None, policy)
    }
    fn acquire_hls_inner(
        &self,
        url: &str,
        directory: &Path,
        output: Option<&Path>,
        policy: &FetchPolicy,
    ) -> Result<DownloadedSource, ConversionError> {
        let _scope = self
            .backend
            .scope(super::measurement::Phase::InputPreparation);
        self.backend.check_cancelled()?;
        valid_url(url)?;
        if let Some(output) = output {
            preflight_download_output(output)?;
        }
        // Only this bounded command label is registered. Acquired URL graphs never
        // reach subprocess arguments or diagnostics after canonical localization.
        self.backend.runner.secrets().register([url]);
        let mut policy = policy.clone();
        if policy.allowed_origins.is_empty() {
            policy
                .allowed_origins
                .push(source::origin(url).map_err(|_| INVALID)?);
        }
        if !policy.allowed_local_roots.is_empty() {
            return Err(INVALID);
        }
        policy
            .validate()
            .map_err(|_| ConversionError("invalid-fetch-policy"))?;
        policy.max_playlist_bytes = policy.max_playlist_bytes.min(4 * 1024 * 1024);
        let budget = self.acquisition_budget(&policy)?;
        let _permit = self
            .backend
            .runner
            .native_permit(
                Claim {
                    cpu_threads: budget.workers,
                    memory_bytes: 256 * 1024 * 1024,
                },
                &self.backend.stop,
                self.backend.timeout,
            )
            .map_err(|error| match error {
                crate::resources::ReservationError::Cancelled => ConversionError("cancelled"),
                _ => ConversionError("resource-budget"),
            })?;
        let mut owned = match output {
            Some(output) => OwnedDownload::reserve(directory, output)?,
            None => OwnedDownload::reserve_source(directory)?,
        };
        let result = self.acquire(url, &policy, budget.workers, &owned);
        match result {
            Ok(mut expected) => {
                // Publish the admission entrypoint only after all workers have joined.
                let (projected, files) = (&mut expected.0, &mut expected.1);
                let pending = (|| {
                    let stage = files::Stage::new(owned.path().parent())?;
                    let pending = stage.private_root().join("entrypoint");
                    let writer = write_private(&pending, projected)?;
                    let created = crate::native::FileIdentity::read(&writer).map_err(|_| IO)?;
                    Ok((stage, pending, writer, created))
                })();
                let (stage, pending, writer, created) = match pending {
                    Ok(pending) => pending,
                    Err(error) => {
                        owned.cleanup()?;
                        return Err(error);
                    }
                };
                let _stage = stage;
                let mut seal = Seal {
                    owned,
                    files: std::mem::take(files),
                };
                let commit = (|| {
                    seal.check(&self.backend.stop)?;
                    // Immutable validation denies WRITE/DELETE sharing on Windows.
                    // Keep the writer's identity, then close it before reopening the name.
                    drop(writer);
                    publish_entrypoint(&seal.owned, &pending, created, |_| {})?;
                    seal.files
                        .insert(PathBuf::from("input.m3u8"), bytes_digest(projected));
                    seal.check(&self.backend.stop)?;
                    Ok(())
                })();
                if let Err(error) = commit {
                    seal.owned.cleanup()?;
                    return Err(error);
                }
                seal.owned.retain();
                Ok(DownloadedSource {
                    entrypoint: seal.owned.path().join("input.m3u8"),
                    seal: Arc::new(seal),
                })
            }
            Err(error) => {
                owned.cleanup()?;
                Err(error)
            }
        }
    }
    fn acquisition_budget(&self, policy: &FetchPolicy) -> Result<Budget, ConversionError> {
        let capacity = self.backend.runner.native_capacity();
        let mut budget = Budget::choose(
            Hardware::detect(),
            Workload::default(),
            self.maximum_workers,
        )
        .map_err(|_| ConversionError("resource-budget"))?;
        budget.workers = budget
            .workers
            .min(policy.connections)
            .min(capacity.0)
            .max(1);
        self.backend.runner.note_resource_budget(&budget);
        Ok(budget)
    }
    fn acquire(
        &self,
        url: &str,
        policy: &FetchPolicy,
        workers: usize,
        owned: &OwnedDownload,
    ) -> Result<Acquired, ConversionError> {
        for name in ["raw", "media"] {
            owned.create_dir(Path::new(name))?;
        }
        let abort = Stop::new();
        let fetcher = Fetcher::new(
            policy,
            owned.path(),
            Arc::new(stream::CancellablePlatform {
                stop: self.backend.stop.clone(),
                abort: abort.clone(),
            }),
        )
        .with_stop(self.backend.stop.clone())
        .with_url_limit(4096);
        let mut graph = Graph {
            owned,
            policy,
            fetcher: &fetcher,
            playlists: BTreeMap::new(),
            media: BTreeMap::new(),
            weight: 0,
            text_bytes: 0,
            records: Vec::new(),
            resources: Vec::new(),
            expected: BTreeMap::new(),
            root_projection: Vec::new(),
            playlist_count: 0,
        };
        let result = (|| {
            graph.playlist(url, true)?;
            graph.fetch_media(workers, &abort)?;
            let index = serde_json::to_vec(&graph.records).map_err(|_| IO)?;
            graph.save("raw/index.json", &index)?;
            Ok((
                std::mem::take(&mut graph.root_projection),
                std::mem::take(&mut graph.expected),
            ))
        })();
        // fetch_media's scoped pool has joined before this point, including failures.
        fetcher.close_all();
        result
    }
    /// Convert only the retained graph identified by this acquisition, with a final publication seal.
    pub fn convert_downloaded(
        &self,
        source: &DownloadedSource,
        request: ConversionRequest<'_>,
    ) -> Result<ConversionReport, ConversionError> {
        source.seal.check(&self.backend.stop)?;
        if request.input.canonicalize().map_err(|_| CHANGED)?
            != source.entrypoint.canonicalize().map_err(|_| CHANGED)?
        {
            return Err(CHANGED);
        }
        source.seal.owned.check_output(request.output)?;
        let converter = Self {
            backend: self.backend.clone(),
            maximum_workers: self.maximum_workers,
            downloaded: Some(source.seal.clone()),
            frozen_source: self.frozen_source.clone(),
            planned_destination: self.planned_destination.clone(),
            output_signaling: self.output_signaling.clone(),
        };
        converter.convert(ConversionRequest {
            input: source.entrypoint(),
            ..request
        })
    }
    pub(super) fn check_downloaded(&self) -> Result<(), ConversionError> {
        if let Some(seal) = &self.downloaded {
            seal.check(&self.backend.stop)?;
        }
        Ok(())
    }
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<std::fs::File, ConversionError> {
    let mut file = crate::native::create_private_new(path).map_err(|_| IO)?;
    file.write_all(bytes).map_err(|_| IO)?;
    file.sync_all().map_err(|_| IO)?;
    Ok(file)
}

fn write_registered(
    owned: &OwnedDownload,
    name: &Path,
    bytes: &[u8],
    before_register: impl FnOnce(&Path),
) -> Result<(), ConversionError> {
    let path = owned.path().join(name);
    let writer = write_private(&path, bytes)?;
    let created = crate::native::FileIdentity::read(&writer).map_err(|_| IO)?;
    drop(writer);
    before_register(&path);
    owned.register_file(name, created)
}
fn publish_entrypoint(
    owned: &OwnedDownload,
    pending: &Path,
    created: crate::native::FileIdentity,
    before_register: impl FnOnce(&Path),
) -> Result<(), ConversionError> {
    let destination = owned.path().join("input.m3u8");
    files::atomic_move(pending, &destination)?;
    before_register(&destination);
    owned.register_file(Path::new("input.m3u8"), created)
}

fn failure(error: &source::Failure) -> ConversionError {
    if matches!(error, source::Failure::Cancelled) {
        ConversionError("cancelled")
    } else {
        IO
    }
}
type Expected = BTreeMap<PathBuf, ([u8; 32], u64)>;
type Acquired = (Vec<u8>, Expected);
fn bytes_digest(bytes: &[u8]) -> ([u8; 32], u64) {
    (Sha256::digest(bytes).into(), bytes.len() as u64)
}
#[derive(Clone)]
struct Playlist {
    local: String,
    master: bool,
    digest: ([u8; 32], u64),
}
struct Graph<'a> {
    owned: &'a OwnedDownload,
    policy: &'a FetchPolicy,
    fetcher: &'a Fetcher,
    playlists: BTreeMap<String, Playlist>,
    media: BTreeMap<String, String>,
    weight: usize,
    text_bytes: usize,
    playlist_count: usize,
    expected: Expected,
    root_projection: Vec<u8>,
    records: Vec<serde_json::Value>,
    resources: Vec<Resource>,
}
struct Resource {
    url: String,
    path: String,
}
fn valid_url(url: &str) -> Result<(), ConversionError> {
    if url.len() > 4096 || url.chars().any(char::is_control) {
        return Err(INVALID);
    }
    let parts = source::pyurl::urlsplit(url, "").map_err(|_| INVALID)?;
    if !matches!(parts.scheme.as_str(), "https" | "http") {
        return Err(INVALID);
    }
    source::origin(url).map_err(|_| INVALID)?;
    Ok(())
}
impl Graph<'_> {
    fn admit_node(&self) -> Result<(), ConversionError> {
        let count = self.playlist_count + self.resources.len();
        if count >= 100_000
            || self
                .policy
                .max_total_resources
                .is_some_and(|limit| count as u64 >= limit)
        {
            return Err(INVALID);
        }
        Ok(())
    }
    fn text_charge(&mut self, count: usize) -> Result<(), ConversionError> {
        self.text_bytes = self
            .text_bytes
            .checked_add(count)
            .filter(|total| *total <= 16 * 1024 * 1024)
            .ok_or(ConversionError("download-graph-limit"))?;
        Ok(())
    }
    fn admit_expansion(&self, lines: &[String], effective: &str) -> Result<(), ConversionError> {
        // Shared parsers retain fully resolved references. Bound that temporary
        // expansion before invoking them, including duplicate references.
        let expanded = expanded_references(lines, effective, self.fetcher.stop())?;
        if self
            .text_bytes
            .checked_add(expanded)
            .is_none_or(|total| total > 16 * 1024 * 1024)
            || self
                .weight
                .checked_add(expanded.saturating_mul(8))
                .is_none_or(|total| total > GRAPH_BYTES)
        {
            return Err(ConversionError("download-graph-limit"));
        }
        Ok(())
    }
    fn save(&mut self, name: &str, bytes: &[u8]) -> Result<(), ConversionError> {
        write_registered(self.owned, Path::new(name), bytes, |_| {})?;
        self.expected
            .insert(PathBuf::from(name), bytes_digest(bytes));
        Ok(())
    }

    fn charge(&mut self, count: usize) -> Result<(), ConversionError> {
        self.weight = self
            .weight
            .checked_add(count)
            .filter(|sum| *sum <= GRAPH_BYTES)
            .ok_or(ConversionError("download-graph-limit"))?;
        Ok(())
    }
    fn playlist(&mut self, url: &str, root: bool) -> Result<String, ConversionError> {
        valid_url(url)?;
        if let Some(node) = self.playlists.get(url) {
            if !root && node.master {
                return Err(INVALID);
            }
            return Ok(node.local.clone());
        }
        self.admit_node()?;
        if self.playlist_count as u64 >= self.policy.max_playlists || self.playlist_count >= 65 {
            return Err(INVALID);
        }
        let (body, effective) = self.fetcher.read(url, true, None).map_err(|error| {
            if self.fetcher.stop().is_set() {
                ConversionError("cancelled")
            } else {
                failure(&error)
            }
        })?;
        valid_url(&effective)?;
        self.text_charge(
            body.len()
                .saturating_add(url.len())
                .saturating_add(effective.len()),
        )?;
        self.charge(body.len().saturating_mul(8) + effective.len() * 8 + 1024)?;
        let lines = source::lines(&body).map_err(|_| INVALID)?;
        self.admit_expansion(&lines, &effective)?;
        let master = lines
            .iter()
            .any(|line| line.starts_with("#EXT-X-STREAM-INF:"));
        if !root && master {
            return Err(INVALID);
        }
        if let Some(node) = self.playlists.get(&effective).cloned() {
            if node.master || node.digest != bytes_digest(&body) {
                return Err(INVALID);
            }
            self.playlists.insert(url.into(), node.clone());
            return Ok(node.local);
        }
        self.playlist_count += 1;
        let id = self.records.len();
        let local = if root {
            "input.m3u8".into()
        } else {
            format!("p{id:06}.m3u8")
        };
        let node = Playlist {
            local: local.clone(),
            master,
            digest: bytes_digest(&body),
        };
        self.playlists.insert(url.into(), node.clone());
        self.playlists.insert(effective.clone(), node);
        self.save(&format!("raw/r{id:06}.m3u8"), &body)?;
        self.records.push(serde_json::json!({"kind":"playlist","requested":url,"effective":effective,"sha256":hex::encode(Sha256::digest(&body)),"bytes":body.len()}));
        if master {
            let plan = playlist::master_plan(&lines, &effective).map_err(|_| INVALID)?;
            if plan
                .iter()
                .any(|(_, _, kind)| *kind == source::PlaylistKind::Text)
            {
                return Err(INVALID);
            }
            for (child, _, _) in plan {
                self.playlist(&child, false)?;
            }
        } else {
            self.plan_media(&lines, &effective)?;
        }
        let projected = self.rewrite(&body, &effective, master)?;
        if root {
            self.root_projection = projected;
        } else {
            self.save(&local, &projected)?;
        }
        Ok(local)
    }
    fn plan_media(&mut self, lines: &[String], effective: &str) -> Result<(), ConversionError> {
        if lines.iter().any(|line| {
            line.starts_with("#EXT-X-BYTERANGE:")
                || line.starts_with("#EXT-X-MAP:") && line.contains("BYTERANGE=")
        }) {
            return Err(INVALID);
        }
        let mut count = 0;
        let plan = playlist::media_plan(lines, effective, self.policy, &mut || {
            if self.fetcher.stop().is_set() {
                return Err(source::Failure::Cancelled);
            }
            count += 1;
            if count > 100_000 {
                return Err(source::Failure::Source(source::SourceError::new(
                    "resource-count-limit",
                )));
            }
            Ok(PathBuf::from(format!("r{count:06}")))
        })
        .map_err(|error| {
            if matches!(error, source::Failure::Cancelled) {
                ConversionError("cancelled")
            } else {
                INVALID
            }
        })?;
        if let Some(init) = plan.init {
            self.media(&init.url, "mp4")?;
        }
        for segment in plan.segments {
            self.media(&segment.url, if segment.has_init { "m4s" } else { "ts" })?;
        }
        Ok(())
    }
    fn media(&mut self, url: &str, extension: &str) -> Result<(), ConversionError> {
        if self.media.contains_key(url) {
            return Ok(());
        }
        self.admit_node()?;
        self.text_charge(url.len())?;
        self.charge(url.len().saturating_mul(8) + 1024)?;
        let local = format!("media/r{:06}.{extension}", self.media.len());
        valid_url(url)?;
        self.resources.push(Resource {
            url: url.into(),
            path: local.clone(),
        });
        self.media.insert(url.into(), local);
        Ok(())
    }
    fn fetch_media(&mut self, workers: usize, abort: &Stop) -> Result<(), ConversionError> {
        let text_bytes = Mutex::new(self.text_bytes);
        let fetched = std::thread::scope(|scope| {
            let pool = Pool::new(scope, workers.min(self.resources.len()).max(1));
            let units = self.resources.iter().map(|resource| {
                let fetcher = self.fetcher;
                let path = self.owned.path().join(&resource.path);
                let owned = self.owned;
                let text_bytes = &text_bytes;
                Box::new(move || {
                    let (received, created) = fetcher
                        .fetch_created(&resource.url, &path, None)
                        .map_err(|error| {
                            if fetcher.stop().is_set() || abort.is_set() {
                                ConversionError("cancelled")
                            } else {
                                failure(&error)
                            }
                        })?;
                    owned.register_file(Path::new(&resource.path), created)?;
                    valid_url(&received.effective)?;
                    let mut total = text_bytes.lock().map_err(|_| IO)?;
                    *total = total
                        .checked_add(received.effective.len())
                        .filter(|total| *total <= 16 * 1024 * 1024)
                        .ok_or(ConversionError("download-graph-limit"))?;
                    Ok(received)
                }) as Unit<'_, source::Fetched, ConversionError>
            });
            run_ordered(|unit| pool.submit(unit), units, Some(abort))
        })?;
        for (resource, fetched) in self.resources.iter().zip(fetched) {
            let hash: [u8; 32] = hex::decode(&fetched.sha256)
                .map_err(|_| IO)?
                .try_into()
                .map_err(|_| IO)?;
            self.expected
                .insert(PathBuf::from(&resource.path), (hash, fetched.size));
            self.records.push(serde_json::json!({"kind":"media","requested":resource.url,"effective":fetched.effective,"sha256":fetched.sha256,"bytes":fetched.size}));
        }
        Ok(())
    }
    fn rewrite(
        &self,
        body: &[u8],
        effective: &str,
        master: bool,
    ) -> Result<Vec<u8>, ConversionError> {
        let text = std::str::from_utf8(body).map_err(|_| INVALID)?;
        let mut result = Vec::new();
        for (position, raw) in text.split_inclusive('\n').enumerate() {
            // Match the shared parser's optional first-line BOM without altering
            // the preserved header or any byte offsets used for URI replacement.
            let content = if position == 0 {
                raw.strip_prefix('\u{feff}').unwrap_or(raw)
            } else {
                raw
            };
            let line = content.trim();
            let uri = if !line.is_empty() && !line.starts_with('#') {
                Some((raw.find(line).ok_or(INVALID)?, line.len(), line.to_owned()))
            } else if line.starts_with("#EXT-X-MAP:") || line.starts_with("#EXT-X-MEDIA:") {
                Some(uri_span(raw)?)
            } else {
                None
            };
            if let Some((start, length, uri)) = uri {
                let url = source::child_url(effective, &uri).map_err(|_| INVALID)?;
                let target = if master {
                    self.playlists.get(&url).map(|node| &node.local)
                } else {
                    self.media.get(&url)
                }
                .ok_or(INVALID)?;
                result.extend_from_slice(&raw.as_bytes()[..start]);
                result.extend_from_slice(target.as_bytes());
                result.extend_from_slice(&raw.as_bytes()[start + length..]);
            } else {
                result.extend_from_slice(raw.as_bytes());
            }
        }
        Ok(result)
    }
}
fn expanded_references(
    lines: &[String],
    effective: &str,
    stop: &Stop,
) -> Result<usize, ConversionError> {
    let mut bytes = 0usize;
    let mut count = 0usize;
    let mut add = |uri: &str| {
        let resolved = source::child_url(effective, uri).map_err(|_| INVALID)?;
        valid_url(&resolved)?;
        count += 1;
        bytes = bytes
            .checked_add(resolved.len())
            .filter(|bytes| *bytes <= 16 * 1024 * 1024)
            .ok_or(ConversionError("download-graph-limit"))?;
        if count > 100_000 {
            return Err(ConversionError("download-graph-limit"));
        }
        Ok(())
    };
    for line in lines {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        if !line.is_empty() && !line.starts_with('#') {
            add(line)?;
        } else if line.starts_with("#EXT-X-MAP:") || line.starts_with("#EXT-X-MEDIA:") {
            let (_, attrs) = line.split_once(':').ok_or(INVALID)?;
            let attrs = crate::m3u8::parse_attributes(attrs).map_err(|_| INVALID)?;
            if let Some(uri) = attrs.get("URI") {
                add(uri)?;
            }
        }
    }
    Ok(bytes)
}
fn uri_span(raw: &str) -> Result<(usize, usize, String), ConversionError> {
    let line = raw.trim();
    let colon = line.find(':').ok_or(INVALID)?;
    let values = &line[colon + 1..];
    let attrs = crate::m3u8::parse_attributes(values).map_err(|_| INVALID)?;
    let uri = attrs.get("URI").ok_or(INVALID)?;
    let mut at = 0;
    while at < values.len() {
        let key_end = values[at..].find('=').ok_or(INVALID)? + at;
        let key = &values[at..key_end];
        let start = key_end + 1;
        let quoted = values.as_bytes().get(start) == Some(&b'"');
        let start = start + usize::from(quoted);
        let end = values[start..]
            .find(if quoted { '"' } else { ',' })
            .map_or(values.len(), |offset| start + offset);
        if key == "URI" {
            return Ok((
                raw.find(line).ok_or(INVALID)? + colon + 1 + start,
                end - start,
                uri.into(),
            ));
        }
        at = end + usize::from(quoted) + 1;
    }
    Err(INVALID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        events::NoSink,
        toolrun::{Secrets, ToolRunner},
    };

    fn replace_before_registration(path: &Path, original: &Path) {
        std::fs::rename(path, original).unwrap();
        write_private(path, b"foreign sentinel").unwrap();
    }

    #[test]
    fn saved_and_published_files_keep_their_writer_identity_until_registration() {
        for name in ["raw/playlist.m3u8", "projection.m3u8", "input.m3u8"] {
            let parent = crate::native::private_tempdir().unwrap();
            let mut owned = OwnedDownload::reserve(
                &parent.path().join("download"),
                &parent.path().join("output"),
            )
            .unwrap();
            owned.create_dir(Path::new("raw")).unwrap();
            let original = parent.path().join("original");
            let result = if name == "input.m3u8" {
                let pending = parent.path().join("pending");
                let writer = write_private(&pending, b"original").unwrap();
                let created = crate::native::FileIdentity::read(&writer).unwrap();
                drop(writer);
                publish_entrypoint(&owned, &pending, created, |path| {
                    replace_before_registration(path, &original);
                })
            } else {
                write_registered(&owned, Path::new(name), b"original", |path| {
                    replace_before_registration(path, &original);
                })
            };
            assert!(result.is_err());
            assert_eq!(
                owned.cleanup().unwrap_err().0,
                "download-cleanup-unconfirmed"
            );
            assert_eq!(
                std::fs::read(owned.path().join(name)).unwrap(),
                b"foreign sentinel"
            );
            assert_eq!(std::fs::read(original).unwrap(), b"original");
        }
    }

    #[test]
    fn fetched_media_handoff_keeps_creation_identity_after_the_writer_closes() {
        let parent = crate::native::private_tempdir().unwrap();
        let source = parent.path().join("source");
        write_private(&source, b"original media").unwrap();
        let policy = FetchPolicy {
            allowed_local_roots: vec![parent.path().to_path_buf()],
            ..FetchPolicy::default()
        };
        let mut owned = OwnedDownload::reserve(
            &parent.path().join("download"),
            &parent.path().join("output"),
        )
        .unwrap();
        let fetcher = Fetcher::new(
            &policy,
            owned.path(),
            Arc::new(source::platform::SystemPlatform),
        );
        let path = owned.path().join("media");
        let (received, created) = fetcher
            .fetch_created(&source::pypath::as_uri(&source).unwrap(), &path, None)
            .unwrap();
        assert_eq!(received.size, b"original media".len() as u64);
        assert_eq!(
            received.sha256,
            hex::encode(Sha256::digest(b"original media"))
        );
        let original = parent.path().join("original");
        replace_before_registration(&path, &original);
        assert!(owned.register_file(Path::new("media"), created).is_err());
        assert_eq!(
            owned.cleanup().unwrap_err().0,
            "download-cleanup-unconfirmed"
        );
        assert_eq!(std::fs::read(path).unwrap(), b"foreign sentinel");
        assert_eq!(std::fs::read(original).unwrap(), b"original media");
    }

    #[test]
    fn long_relative_base_expansion_is_bounded_before_shared_parser_allocation() {
        let base = format!(
            "https://media.example.test/{}/source.m3u8",
            "a".repeat(3500)
        );
        let compact = vec!["segment.ts".to_owned(); 4000];
        let bytes = expanded_references(&compact, &base, &Stop::new()).unwrap();
        assert!(bytes > 14_000_000 && bytes < 16 * 1024 * 1024);
        let oversized = vec!["segment.ts".to_owned(); 5000];
        assert_eq!(
            expanded_references(&oversized, &base, &Stop::new())
                .unwrap_err()
                .0,
            "download-graph-limit"
        );
    }

    #[test]
    fn retained_source_seal_is_checked_after_output_copy_and_directory_preparation() {
        for directory in [false, true] {
            let parent = tempfile::tempdir().unwrap();
            let output = parent.path().join("output");
            let mut owned =
                OwnedDownload::reserve(&parent.path().join("download"), &output).unwrap();
            let original = owned.path().join("original");
            let writer = write_private(&original, b"immutable source").unwrap();
            let created = crate::native::FileIdentity::read(&writer).unwrap();
            drop(writer);
            owned.register_file(Path::new("original"), created).unwrap();
            owned.retain();
            let seal = Arc::new(Seal {
                owned,
                files: BTreeMap::from([(
                    PathBuf::from("original"),
                    bytes_digest(b"immutable source"),
                )]),
            });
            let mut converter = Converter::new(
                Arc::new(ToolRunner::new(
                    Arc::new(Secrets::default()),
                    Arc::new(NoSink),
                )),
                "unused",
                "unused",
            );
            converter.downloaded = Some(seal);
            converter.check_downloaded().unwrap();
            let stage = files::Stage::new(Some(parent.path())).unwrap();
            let candidate = stage.path().join("candidate");
            files::create_new(&candidate)
                .unwrap()
                .write_all(b"verified candidate")
                .unwrap();
            let mutate = |_: &Path| std::fs::write(&original, b"changed source").unwrap();
            let result = if directory {
                files::publish_directory_observed(stage.path(), &output, None, mutate, || {
                    converter.check_downloaded()
                })
            } else {
                files::publish_file_observed(&candidate, &output, None, mutate, || {
                    converter.check_downloaded()
                })
            };
            assert_eq!(result.unwrap_err(), CHANGED);
            assert!(!output.exists());
            assert_eq!(std::fs::read(&candidate).unwrap(), b"verified candidate");
            assert_eq!(std::fs::read(&original).unwrap(), b"changed source");
        }
    }
}
