//! Concurrent clear-HLS analysis with deterministic resource IDs and records.
//!
//! Read the master first, plan resources in playlist order, and use separate bounded
//! fetch and inspection pools. On failure, stop new work, drain workers, report the
//! earliest failure in source order, and remove the partial snapshot.

use super::fetch::{ByteRange, Fetched, Fetcher, open_exclusive, read_full};
use super::platform::{Platform, SystemPlatform};
use super::playlist::{self, MediaPlan, PlaylistAttrs, PlaylistKind};
use super::probe::{FfprobeProber, ProbeStream, Prober, check_init, detect_container};
use super::pypath::{as_uri, resolve, stem, with_suffix};
use super::pyurl::urlsplit;
use super::verify::py_eq;
use super::{
    Escaped, Failure, FetchPolicy, SourceAnalysis, SourceError, SourceRequest, SourceSegment,
    SourceTrack,
};
use crate::canonical::Dumps;
use crate::concurrency::{MAX_PENDING_TASKS, Outcome, Pool, Task, Unit, run_ordered};
use crate::events::{Record, Sink};
use crate::m3u8::Attributes;
use crate::snapshot::{analysis_id, sha256_hex, snapshot_digest};
use crate::toolrun::ToolRunner;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::File;
use std::io::Write as _;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, mpsc};

/// How the analyzer reads playlists and fetches media: by default the [`Fetcher`] itself;
/// tests stand in for it as the tests dependency injection `_Fetcher.read` and `_Fetcher.fetch`
/// (holding a download, recording the order, failing one).
pub trait FetchHooks: Send + Sync {
    /// `fetch.read(url, playlist=…)`.
    fn read(
        &self,
        fetcher: &Fetcher,
        url: &str,
        playlist: bool,
    ) -> Result<(Vec<u8>, String), Failure> {
        fetcher.read(url, playlist, None)
    }

    /// `fetch.fetch(url, destination, byte_range=…)`.
    fn fetch(
        &self,
        fetcher: &Fetcher,
        url: &str,
        destination: &Path,
        byte_range: Option<ByteRange>,
    ) -> Result<Fetched, Failure> {
        fetcher.fetch(url, destination, byte_range)
    }
}

/// The fetcher as it is.
#[derive(Clone, Copy, Debug, Default)]
pub struct Direct;

impl FetchHooks for Direct {}

/// What an analysis runs with besides its request and policy.
pub struct AnalyzeOptions<'a> {
    /// Where the progress records go (`on_log`); the clear-HLS records carry no `ts`.
    pub on_log: Option<&'a dyn Sink>,
    /// How a landed segment is inspected (`_probe`).
    pub prober: &'a dyn Prober,
    /// The operating system the fetcher uses.
    pub platform: Arc<dyn Platform>,
    /// How playlists are read and media fetched.
    pub hooks: &'a dyn FetchHooks,
}

impl fmt::Debug for AnalyzeOptions<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnalyzeOptions")
            .field("on_log", &self.on_log.is_some())
            .finish_non_exhaustive()
    }
}

fn escaped(class: &'static str) -> Failure {
    Failure::Escaped(Escaped::new(class))
}

/// `analyze_hls(source, asset_id=…, output_root=…, policy=…, on_log=…)` with ffprobe run by
/// `runner` (its records go to the runner's sink) and the system's network and disk.
pub fn analyze_hls(
    source: &SourceRequest,
    asset_id: &str,
    output_root: &Path,
    policy: &FetchPolicy,
    on_log: Option<&dyn Sink>,
    runner: &ToolRunner,
) -> Result<SourceAnalysis, Failure> {
    policy.validate()?;
    let budget = crate::resources::Budget::choose(
        crate::resources::Hardware::detect(),
        crate::resources::Workload::default(),
        policy.probe_workers,
    )
    .map_err(|_| Failure::source("resource-budget"))?;
    let mut bounded_policy = policy.clone();
    bounded_policy.probe_workers = Some(budget.workers);
    let prober = FfprobeProber::new(runner);
    let options = AnalyzeOptions {
        on_log,
        prober: &prober,
        platform: Arc::new(SystemPlatform),
        hooks: &Direct,
    };
    let analysis = analyze_hls_with(source, asset_id, output_root, &bounded_policy, &options)?;
    runner.note_resource_budget(&budget);
    Ok(analysis)
}

/// `[A-Za-z0-9][A-Za-z0-9_.-]{0,127}`.
fn valid_asset_id(asset_id: &str) -> bool {
    #[cfg(windows)]
    if crate::native::validate_windows_relative_path(asset_id).is_err() {
        return false;
    }
    let bytes = asset_id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

fn make_private_dir(directory: &Path) -> Result<(), Failure> {
    crate::native::create_private_dir_new(directory).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AlreadyExists {
            // Never replace an admitted snapshot implicitly: a source change needs a new id.
            Failure::source("source-snapshot-exists")
        } else {
            escaped("OSError")
        }
    })
}

/// [`analyze_hls`] with its collaborators given (`options`).
///
/// The policy is validated first (`invalid-fetch-policy`, as constructing a Python
/// `FetchPolicy` would have refused it). A returned error is never [`Failure::Cancelled`].
pub fn analyze_hls_with(
    source: &SourceRequest,
    asset_id: &str,
    output_root: &Path,
    policy: &FetchPolicy,
    options: &AnalyzeOptions<'_>,
) -> Result<SourceAnalysis, Failure> {
    policy.validate()?;
    if !valid_asset_id(asset_id) {
        return Err(Failure::source("invalid-asset-id"));
    }
    let asset_dir = resolve(output_root)
        .map_err(Failure::Escaped)?
        .join(asset_id);
    let parent = asset_dir.join("clear");
    if is_symlink(&asset_dir) || is_symlink(&parent) {
        return Err(Failure::source("unsafe-output-path"));
    }
    crate::native::create_private_dir_all(&parent).map_err(|_| escaped("OSError"))?;
    #[cfg(windows)]
    let _output_guard =
        crate::native::pin_private_directory(&asset_dir).map_err(|_| escaped("OSError"))?;
    let directory = parent.join("source");
    make_private_dir(&directory)?;
    let fetch = Fetcher::new(policy, &directory, Arc::clone(&options.platform));
    let outcome = std::thread::scope(|scope| {
        let fetch_pool = Pool::new(scope, policy.connections);
        let probe_pool = Pool::new(scope, policy.effective_probe_workers());
        let run = Run {
            source,
            asset_id,
            directory: &directory,
            policy,
            on_log: options.on_log,
            fetch: &fetch,
            hooks: options.hooks,
            prober: options.prober,
            fetch_pool: &fetch_pool,
            probe_pool: &probe_pool,
        };
        let outcome = panic::catch_unwind(AssertUnwindSafe(|| run.analyze()));
        if !matches!(outcome, Ok(Ok(_))) {
            // Stop every worker; the ones in flight finish; nothing queued starts.
            fetch.stop().set();
        }
        fetch_pool.shutdown(true);
        probe_pool.shutdown(true);
        outcome // the scope joins every worker before the snapshot can be removed
    });
    fetch.close_all();
    match outcome {
        Ok(Ok(analysis)) => Ok(analysis),
        Ok(Err(failure)) => {
            // The partial snapshot cannot later masquerade as admitted input or block retries.
            let _ = std::fs::remove_dir_all(&directory);
            Err(match failure {
                Failure::Cancelled => escaped("Cancelled"),
                other => other,
            })
        }
        Err(payload) => {
            let _ = std::fs::remove_dir_all(&directory);
            panic::resume_unwind(payload)
        }
    }
}

/// The content type of a track spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContentType {
    Video,
    Audio,
    Text,
}

impl ContentType {
    fn as_str(self) -> &'static str {
        match self {
            ContentType::Video => "video",
            ContentType::Audio => "audio",
            ContentType::Text => "text",
        }
    }

    fn rank(name: &str) -> u8 {
        match name {
            "video" => 0,
            "audio" => 1,
            _ => 2,
        }
    }
}

/// A track a playlist contributes (`_validate_playlist`'s specs), before its id.
struct Spec {
    ctype: ContentType,
    container: String,
    codec: String,
    stream: ProbeStream,
    metadata: Attributes,
    group: String,
    muxed: bool,
}

/// What became of a planned resource.
#[derive(Clone, Debug, Default)]
struct Landed {
    final_path: Option<PathBuf>,
    sha256: Option<String>,
    size: u64,
}

/// Position in the download plan without materializing a closure for every resource.
#[derive(Default)]
struct DownloadCursor {
    playlist: usize,
    resource: usize,
}

impl DownloadCursor {
    fn next(&mut self, state: &State) -> Option<(usize, Which)> {
        loop {
            let playlist = state.playlists.get(self.playlist)?;
            if self.resource == 0 {
                self.resource = 1;
                if playlist.plan.init.is_some() {
                    return Some((self.playlist, Which::Init));
                }
            }
            let index = self.resource - 1;
            if index < playlist.plan.segments.len() {
                self.resource += 1;
                return Some((self.playlist, Which::Segment(index)));
            }
            self.playlist += 1;
            self.resource = 0;
        }
    }
}

/// A media playlist and its coordinator-owned download and inspection state.
struct PlaylistState {
    path: PathBuf,
    attrs: PlaylistAttrs,
    kind: Option<PlaylistKind>,
    total: usize,
    plan: MediaPlan,
    /// The initialization segment as its download left it (set by the worker as it lands,
    /// as Python's `_Resource.final` is).
    init_landed: Arc<OnceLock<(PathBuf, u64)>>,
    init_sha256: Option<String>,
    segments: Vec<Landed>,
    containers: Vec<&'static str>,
    downloaded: usize,
    probed: usize,
    probe_started: bool,
    waiting: VecDeque<usize>,
    streams: Vec<Option<Vec<ProbeStream>>>,
    specs: Vec<Spec>,
}

impl PlaylistState {
    fn new(
        path: PathBuf,
        attrs: PlaylistAttrs,
        kind: Option<PlaylistKind>,
        total: usize,
        plan: MediaPlan,
    ) -> Self {
        let count = plan.segments.len();
        PlaylistState {
            path,
            attrs,
            kind,
            total,
            plan,
            init_landed: Arc::new(OnceLock::new()),
            init_sha256: None,
            segments: vec![Landed::default(); count],
            containers: Vec::new(),
            downloaded: 0,
            probed: 0,
            probe_started: false,
            waiting: VecDeque::new(),
            streams: vec![None; count],
            specs: Vec::new(),
        }
    }

    fn rendition_id(&self) -> String {
        stem(&self.path)
    }

    /// `init_ready`: no initialization segment, or it has landed.
    fn init_ready(&self) -> bool {
        self.plan.init.is_none() || self.init_landed.get().is_some()
    }
}

/// The resource ids and their records (`allocate` / `record`).
struct Resources {
    directory: PathBuf,
    budget: Option<u64>,
    rows: Vec<Option<Map<String, Value>>>,
}

impl Resources {
    /// The next resource's base path (`r000000`, `r000001`, …), within the total-resource
    /// budget when the policy sets one.
    fn allocate(&mut self) -> Result<PathBuf, Failure> {
        if self
            .budget
            .is_some_and(|budget| self.rows.len() as u64 >= budget)
        {
            return Err(Failure::source("resource-budget-exceeded"));
        }
        let base = self.directory.join(format!("r{:06}", self.rows.len()));
        self.rows.push(None);
        Ok(base)
    }

    fn record(
        &mut self,
        path: &Path,
        sha256: &str,
        kind: &str,
        url: &str,
        range: Option<ByteRange>,
    ) {
        let id = stem(path);
        let index: usize = id[1..].parse().expect("a resource id is r and its number");
        let mut row = Map::new();
        row.insert("id".into(), id.into());
        row.insert(
            "path".into(),
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into(),
        );
        row.insert("sha256".into(), sha256.into());
        row.insert("kind".into(), kind.into());
        row.insert("url".into(), url.into());
        row.insert(
            "byte_range".into(),
            range.map_or(Value::Null, |range| {
                Value::Array(vec![range.offset.into(), range.length.into()])
            }),
        );
        self.rows[index] = Some(row);
    }
}

/// `_write(path, data)`: a new file (exclusive, mode 0600) holding `data`.
fn write_new(path: &Path, data: &[u8]) -> Result<(), Failure> {
    let mut file = open_exclusive(path)?;
    file.write_all(data)
        .map_err(|_| Failure::source("snapshot-write-failed"))
}

/// Which resource of a playlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which {
    Init,
    Segment(usize),
}

/// `(playlist order, 0 download / 1 inspection, index)`: failures are ordered by it.
type Key = (usize, u8, i64);

enum Output {
    Downloaded(Downloaded),
    Probed(Vec<ProbeStream>),
}

struct Downloaded {
    final_path: PathBuf,
    sha256: String,
    size: u64,
    detected: Option<super::Container>,
}

struct Pending {
    key: Key,
    playlist: usize,
    which: Which,
    task: Task<Result<Output, Failure>>,
}

/// The coordinator: `'e` borrows outlive the scope `'s` (units may hold them), `'p` borrows
/// the pools inside it.
struct Run<'e, 's, 'p> {
    source: &'e SourceRequest,
    asset_id: &'e str,
    directory: &'e Path,
    policy: &'e FetchPolicy,
    on_log: Option<&'e dyn Sink>,
    fetch: &'e Fetcher,
    hooks: &'e dyn FetchHooks,
    prober: &'e dyn Prober,
    fetch_pool: &'p Pool<'s>,
    probe_pool: &'p Pool<'s>,
}

/// The coordinator's state.
struct State {
    resources: Resources,
    playlists: Vec<PlaylistState>,
    track_total: usize,
}

/// The coordinator's bookkeeping of submitted units.
struct Units {
    sender: mpsc::Sender<usize>,
    receiver: mpsc::Receiver<usize>,
    pending: HashMap<usize, Pending>,
    next: usize,
}

impl Units {
    /// Follow a submitted unit: its id reaches the completion queue once it is done.
    fn track(
        &mut self,
        key: Key,
        playlist: usize,
        which: Which,
        task: Task<Result<Output, Failure>>,
    ) {
        debug_assert!(self.pending.len() < MAX_PENDING_TASKS);
        let id = self.next;
        self.next += 1;
        let sender = self.sender.clone();
        task.on_done(move || {
            let _ = sender.send(id);
        });
        self.pending.insert(
            id,
            Pending {
                key,
                playlist,
                which,
                task,
            },
        );
    }
}

fn checkpoint(index: usize, total: usize) -> bool {
    index == 1 || index.is_multiple_of(10) || index == total
}

impl<'e: 's, 's> Run<'e, 's, '_> {
    /// Callers receive only fixed tokens, opaque local ids and counters: source locations,
    /// playlist attributes and upstream errors stay private.
    fn progress(&self, message: &str, counts: &[(&str, Value)]) {
        let Some(sink) = self.on_log else {
            return;
        };
        let mut record = Record::new();
        record.insert("stage".into(), "hls-source".into());
        record.insert("level".into(), "info".into());
        record.insert("message".into(), message.into());
        for (key, value) in counts {
            record.insert((*key).to_string(), value.clone());
        }
        sink.emit(&record);
    }

    fn analyze(&self) -> Result<SourceAnalysis, Failure> {
        let mut state = State {
            resources: Resources {
                directory: self.directory.to_path_buf(),
                budget: self.policy.max_total_resources,
                rows: Vec::new(),
            },
            playlists: Vec::new(),
            track_total: 0,
        };
        self.progress("analysis-started", &[]);
        let mut location = self.source.url.clone();
        let local_path = urlsplit(&location, "")
            .map_err(|_| escaped("ValueError"))?
            .scheme
            .is_empty();
        #[cfg(windows)]
        let local_path = local_path || super::pypath::is_native_path(&location);
        if local_path {
            location = as_uri(&resolve(Path::new(&location)).map_err(Failure::Escaped)?)
                .map_err(Failure::Escaped)?;
        }
        let (body, effective) = self.hooks.read(self.fetch, &location, true)?;
        let (root, lines, total) = self.start_playlist(&mut state, &body, &effective)?;
        let is_master = lines
            .iter()
            .any(|line| line.starts_with("#EXT-X-STREAM-INF:"));
        if is_master {
            self.plan_master(&mut state, &lines, &effective, &location)?;
        } else {
            let plan = playlist::media_plan(&lines, &effective, self.policy, &mut || {
                state.resources.allocate()
            })?;
            state.playlists.push(PlaylistState::new(
                root.clone(),
                PlaylistAttrs::default(),
                None,
                total,
                plan,
            ));
        }
        self.transfer(&mut state)?;
        self.finish(state, &root, total, is_master)
    }

    fn start_playlist(
        &self,
        state: &mut State,
        body: &[u8],
        effective: &str,
    ) -> Result<(PathBuf, Vec<String>, usize), Failure> {
        let path = with_suffix(&state.resources.allocate()?, ".m3u8");
        write_new(&path, body)?;
        state
            .resources
            .record(&path, &sha256_hex(body), "playlist", effective, None);
        let lines = playlist::lines(body)?;
        let total = lines
            .iter()
            .filter(|line| line.starts_with("#EXTINF:"))
            .count();
        self.progress(
            "playlist-started",
            &[
                ("rendition_id", stem(&path).into()),
                ("total", total.into()),
            ],
        );
        Ok((path, lines, total))
    }

    fn plan_master(
        &self,
        state: &mut State,
        lines: &[String],
        effective: &str,
        location: &str,
    ) -> Result<(), Failure> {
        if lines.iter().any(|line| line.starts_with("#EXTINF:")) {
            return Err(Failure::source("ambiguous-playlist"));
        }
        let plan = playlist::master_plan(lines, effective)?;
        let mut seen: HashSet<&str> = HashSet::from([location]);
        for (url, _, _) in &plan {
            if url == location {
                return Err(Failure::source("playlist-cycle"));
            }
            if !seen.insert(url.as_str()) {
                return Err(Failure::source("duplicate-playlist"));
            }
        }
        if 1 + plan.len() as u64 > self.policy.max_playlists {
            return Err(Failure::source("playlist-budget-exceeded"));
        }
        // Every media playlist at once; results and failures are taken in master order.
        let fetch = self.fetch;
        let hooks = self.hooks;
        let units = plan.iter().map(|(url, _, _)| {
            let url = url.clone();
            let unit: Unit<'s, (Vec<u8>, String), Failure> =
                Box::new(move || hooks.read(fetch, &url, true));
            unit
        });
        let bodies = run_ordered(|unit| self.fetch_pool.submit(unit), units, None)?;
        for ((url, attrs, kind), (child_body, child_effective)) in plan.into_iter().zip(bodies) {
            if child_effective == location && child_effective != url {
                return Err(Failure::source("playlist-cycle"));
            }
            let (path, child_lines, child_total) =
                self.start_playlist(state, &child_body, &child_effective)?;
            let resources = &mut state.resources;
            let media =
                playlist::media_plan(&child_lines, &child_effective, self.policy, &mut || {
                    resources.allocate()
                })?;
            state.playlists.push(PlaylistState::new(
                path,
                attrs,
                Some(kind),
                child_total,
                media,
            ));
        }
        Ok(())
    }

    // --- downloads and inspections: one connection pool, one probe pool, coordinated here --

    fn transfer(&self, state: &mut State) -> Result<(), Failure> {
        let (sender, receiver) = mpsc::channel();
        let mut units = Units {
            sender,
            receiver,
            pending: HashMap::new(),
            next: 0,
        };
        let mut cursor = DownloadCursor::default();
        self.fill_window(state, &mut units, &mut cursor);
        let mut failures: Vec<(Key, SourceError)> = Vec::new();
        while !units.pending.is_empty() && failures.is_empty() {
            // One completion queue: collecting a finished unit costs O(1) whatever the plan.
            let id = units
                .receiver
                .recv()
                .expect("every pending unit holds a sender");
            let Some(done) = units.pending.remove(&id) else {
                continue;
            };
            let handled = match done.task.take() {
                None => Ok(()), // cancelled before it started
                Some(Outcome::Panicked(payload)) => panic::resume_unwind(payload),
                Some(Outcome::Returned(Err(failure))) => Err(failure),
                Some(Outcome::Returned(Ok(Output::Downloaded(landed)))) => {
                    self.downloaded(state, done.playlist, done.which, landed)
                }
                Some(Outcome::Returned(Ok(Output::Probed(streams)))) => {
                    self.probed(state, done.playlist, done.which, streams)
                }
            };
            match handled {
                Ok(()) | Err(Failure::Cancelled) => {}
                Err(Failure::Source(error)) => {
                    failures.push((done.key, error));
                    self.fetch.stop().set();
                }
                Err(escaping @ Failure::Escaped(_)) => return Err(escaping),
            }
            if failures.is_empty() {
                self.fill_window(state, &mut units, &mut cursor);
            }
        }
        if failures.is_empty() {
            return Ok(());
        }
        // Nothing queued starts; work in flight stops at its next chunk (downloads) or finishes
        // (an inspection already running); then the earliest failure in playlist order among
        // the work that ran is the analysis error — for a single fault, the sequential one.
        for pending in units.pending.values() {
            pending.task.cancel();
        }
        for pending in units.pending.values() {
            pending.task.wait();
        }
        for pending in units.pending.values() {
            if let Some(Outcome::Returned(Err(Failure::Source(error)))) = pending.task.take() {
                failures.push((pending.key, error));
            }
        }
        let earliest = failures
            .into_iter()
            .reduce(|best, item| if item.0 < best.0 { item } else { best })
            .expect("a failure was recorded");
        Err(Failure::Source(earliest.1))
    }

    fn fill_window(&self, state: &mut State, units: &mut Units, cursor: &mut DownloadCursor) {
        while units.pending.len() < MAX_PENDING_TASKS {
            let ready = state
                .playlists
                .iter_mut()
                .enumerate()
                .find_map(|(order, playlist)| {
                    playlist
                        .init_ready()
                        .then(|| playlist.waiting.pop_front())
                        .flatten()
                        .map(|index| (order, index))
                });
            if let Some((order, index)) = ready {
                self.submit_probe(state, units, order, index);
            } else if let Some((order, which)) = cursor.next(state) {
                self.submit_download(state, units, order, which);
            } else {
                break;
            }
        }
    }

    fn submit_download(&self, state: &State, units: &mut Units, order: usize, which: Which) {
        let playlist = &state.playlists[order];
        let (planned, index) = match which {
            Which::Init => (playlist.plan.init.clone().expect("planned"), -1),
            Which::Segment(index) => (
                playlist.plan.segments[index].clone(),
                i64::try_from(index).expect("segment count"),
            ),
        };
        let fetch = self.fetch;
        let hooks = self.hooks;
        let landed = Arc::clone(&playlist.init_landed);
        let task = self.fetch_pool.submit(move || {
            download(fetch, hooks, &planned, which == Which::Init, &landed).map(Output::Downloaded)
        });
        units.track((order, 0, index), order, which, task);
    }

    fn submit_probe(&self, state: &mut State, units: &mut Units, order: usize, index: usize) {
        let playlist = &mut state.playlists[order];
        if !playlist.probe_started {
            playlist.probe_started = true;
            let total = playlist.plan.segments.len();
            self.progress(
                "probe-started",
                &[
                    ("rendition_id", playlist.rendition_id().into()),
                    ("total", total.into()),
                ],
            );
        }
        let segment = &playlist.segments[index];
        let final_path = segment
            .final_path
            .clone()
            .expect("landed before inspection");
        let size = segment.size;
        let init = playlist.init_landed.get().cloned();
        let fetch = self.fetch;
        let prober = self.prober;
        let task = self
            .probe_pool
            .submit(move || inspect(fetch, prober, &final_path, size, init).map(Output::Probed));
        let key = (order, 1, i64::try_from(index).expect("segment count"));
        units.track(key, order, Which::Segment(index), task);
    }

    fn downloaded(
        &self,
        state: &mut State,
        order: usize,
        which: Which,
        landed: Downloaded,
    ) -> Result<(), Failure> {
        let playlist = &mut state.playlists[order];
        match which {
            Which::Init => {
                let planned = playlist.plan.init.as_ref().expect("planned");
                state.resources.record(
                    &landed.final_path,
                    &landed.sha256,
                    "init",
                    &planned.url,
                    planned.byte_range,
                );
                playlist.init_sha256 = Some(landed.sha256);
                Ok(())
            }
            Which::Segment(index) => {
                let planned = &playlist.plan.segments[index];
                state.resources.record(
                    &landed.final_path,
                    &landed.sha256,
                    "segment",
                    &planned.url,
                    planned.byte_range,
                );
                playlist.segments[index] = Landed {
                    final_path: Some(landed.final_path),
                    sha256: Some(landed.sha256),
                    size: landed.size,
                };
                let detected = landed.detected.expect("a segment's container").as_str();
                if !playlist.containers.contains(&detected) {
                    playlist.containers.push(detected);
                }
                if playlist.containers.len() != 1 {
                    return Err(Failure::source("changing-container-unsupported"));
                }
                playlist.downloaded += 1;
                let total = playlist.plan.segments.len();
                if checkpoint(playlist.downloaded, total) {
                    self.progress(
                        "segment-downloaded",
                        &[
                            ("rendition_id", playlist.rendition_id().into()),
                            ("index", playlist.downloaded.into()),
                            ("total", total.into()),
                        ],
                    );
                }
                state.playlists[order].waiting.push_back(index);
                Ok(())
            }
        }
    }

    fn probed(
        &self,
        state: &mut State,
        order: usize,
        which: Which,
        streams: Vec<ProbeStream>,
    ) -> Result<(), Failure> {
        let Which::Segment(index) = which else {
            unreachable!("only segments are inspected")
        };
        let playlist = &mut state.playlists[order];
        playlist.streams[index] = Some(streams);
        playlist.probed += 1;
        let total = playlist.plan.segments.len();
        if checkpoint(playlist.probed, total) {
            self.progress(
                "segment-probed",
                &[
                    ("rendition_id", playlist.rendition_id().into()),
                    ("index", playlist.probed.into()),
                    ("total", total.into()),
                ],
            );
        }
        if playlist.probed == total {
            playlist.specs = validate_playlist(playlist)?;
            state.track_total += playlist.specs.len();
            let playlist = &state.playlists[order];
            self.progress(
                "playlist-complete",
                &[
                    ("rendition_id", playlist.rendition_id().into()),
                    ("total", playlist.total.into()),
                    ("track_count", state.track_total.into()),
                ],
            );
        }
        Ok(())
    }

    fn finish(
        &self,
        state: State,
        root: &Path,
        total: usize,
        is_master: bool,
    ) -> Result<SourceAnalysis, Failure> {
        let mut counters = [0_usize; 3];
        let mut tracks = Vec::new();
        for playlist in &state.playlists {
            for spec in &playlist.specs {
                let (prefix, slot) = match spec.ctype {
                    ContentType::Video => ("v", 0),
                    ContentType::Audio => ("a", 1),
                    ContentType::Text => ("t", 2),
                };
                let identifier = format!("{prefix}{:03}", counters[slot]);
                counters[slot] += 1;
                tracks.push(build_track(identifier, spec, playlist)?);
            }
        }
        if is_master {
            self.progress(
                "playlist-complete",
                &[
                    ("rendition_id", stem(root).into()),
                    ("total", total.into()),
                    ("track_count", tracks.len().into()),
                ],
            );
        }
        // Stable census order simplifies selections without using URL material as ids.
        tracks.sort_by(|a, b| {
            (ContentType::rank(&a.content_type), &a.id)
                .cmp(&(ContentType::rank(&b.content_type), &b.id))
        });
        let mut private = Map::new();
        private.insert("schema_version".into(), "1".into());
        private.insert("asset_id".into(), self.asset_id.into());
        private.insert(
            "resources".into(),
            Value::Array(
                state
                    .resources
                    .rows
                    .into_iter()
                    .map(|row| row.map_or(Value::Null, Value::Object))
                    .collect(),
            ),
        );
        let mut rows = Vec::with_capacity(tracks.len());
        for track in &tracks {
            rows.push(Value::Object(
                track
                    .to_private_dict(self.directory)
                    .ok_or_else(|| escaped("ValueError"))?,
            ));
        }
        private.insert("tracks".into(), Value::Array(rows));
        let digest = snapshot_digest(&Value::Object(private.clone()));
        let id = analysis_id(&digest);
        private.insert("snapshot_digest".into(), digest.clone().into());
        private.insert("analysis_id".into(), id.clone().into());
        let snapshot = self.directory.join("analysis.json");
        write_new(
            &snapshot,
            Dumps::SORTED.dumps(&Value::Object(private)).as_bytes(),
        )?;
        let track_count = tracks.len();
        let result = SourceAnalysis {
            schema_version: "1".to_string(),
            source_kind: "hls-clear".to_string(),
            analysis_id: id,
            asset_id: self.asset_id.to_string(),
            admitted: true,
            tracks,
            findings: Vec::new(),
            snapshot_path: snapshot,
            snapshot_digest: digest,
        };
        self.progress(
            "analysis-complete",
            &[
                ("status", "pass".into()),
                ("track_count", track_count.into()),
            ],
        );
        Ok(result)
    }
}

/// The download unit: the resource streamed to `rNNNNNN.part`, checked (an initialization
/// segment's first box; a segment's container from its first 512 bytes, judged with the
/// `EXT-X-MAP` in effect at its own line) and renamed to its detected suffix.
fn download(
    fetch: &Fetcher,
    hooks: &dyn FetchHooks,
    planned: &playlist::Planned,
    init: bool,
    init_landed: &OnceLock<(PathBuf, u64)>,
) -> Result<Downloaded, Failure> {
    if fetch.stop().is_set() {
        return Err(Failure::Cancelled);
    }
    let part = with_suffix(&planned.base, ".part");
    let fetched = hooks.fetch(fetch, &planned.url, &part, planned.byte_range)?;
    let (detected, final_path) = if init {
        check_init(&part)?;
        (None, with_suffix(&planned.base, ".mp4"))
    } else {
        let head = File::open(&part)
            .and_then(|mut file| read_full(&mut file, 512))
            .map_err(|_| Failure::source("source-read-failed"))?;
        let detected = detect_container(&head, planned.has_init)?;
        (
            Some(detected),
            with_suffix(&planned.base, detected.suffix()),
        )
    };
    std::fs::rename(&part, &final_path).map_err(|_| Failure::source("snapshot-write-failed"))?;
    if init {
        let _ = init_landed.set((final_path.clone(), fetched.size));
    }
    Ok(Downloaded {
        final_path,
        sha256: fetched.sha256,
        size: fetched.size,
        detected,
    })
}

/// Releases a disk claim however the inspection ends (Python's `finally`).
struct Claim<'f> {
    fetch: &'f Fetcher,
    count: u128,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        self.fetch.release_disk(self.count);
    }
}

/// The inspection unit: with an initialization segment, its temporary copy (init + segment)
/// is claimed against the disk floor first.
fn inspect(
    fetch: &Fetcher,
    prober: &dyn Prober,
    path: &Path,
    size: u64,
    init: Option<(PathBuf, u64)>,
) -> Result<Vec<ProbeStream>, Failure> {
    if fetch.stop().is_set() {
        return Err(Failure::Cancelled);
    }
    let Some((init_path, init_size)) = init else {
        return prober.probe_with_stop(path, None, fetch.stop());
    };
    let count = u128::from(init_size) + u128::from(size);
    fetch.claim_disk_or_escape(count)?;
    let _claim = Claim { fetch, count };
    prober.probe_with_stop(path, Some(&init_path), fetch.stop())
}

/// One stream's topology (`index` defaulting to 0, `codec_type`, `codec_name`, `profile`,
/// `width`, `height`).
fn topology(streams: &[ProbeStream]) -> Vec<[Value; 6]> {
    streams
        .iter()
        .map(|stream| {
            let get = |key: &str| stream.get(key).cloned().unwrap_or(Value::Null);
            [
                stream
                    .get("index")
                    .cloned()
                    .unwrap_or_else(|| Value::from(0)),
                get("codec_type"),
                get("codec_name"),
                get("profile"),
                get("width"),
                get("height"),
            ]
        })
        .collect()
}

fn topology_eq(left: &[[Value; 6]], right: &[[Value; 6]]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(a, b)| a.iter().zip(b).all(|(x, y)| py_eq(x, y)))
}

fn is_str(value: Option<&Value>, text: &str) -> bool {
    value.and_then(Value::as_str) == Some(text)
}

/// `_validate_playlist`: the admission checks over a playlist's inspected segments; the
/// tracks it contributes, without their ids.
fn validate_playlist(playlist: &PlaylistState) -> Result<Vec<Spec>, Failure> {
    let sets: Vec<&Vec<ProbeStream>> = playlist
        .streams
        .iter()
        .map(|streams| streams.as_ref().expect("every segment was inspected"))
        .collect();
    let first = topology(sets[0]);
    if sets[1..]
        .iter()
        .any(|streams| !topology_eq(&topology(streams), &first))
    {
        return Err(Failure::source("changing-codec-configuration"));
    }
    let streams = sets[0];
    let count = |kind: &str| {
        streams
            .iter()
            .filter(|stream| is_str(stream.get("codec_type"), kind))
            .count()
    };
    let has_video = count("video") > 0;
    let has_audio = count("audio") > 0;
    if ["video", "audio", "subtitle"]
        .iter()
        .any(|kind| count(kind) > 1)
    {
        return Err(Failure::source("ambiguous-source-topology"));
    }
    let inband = playlist.attrs.inband_audio.is_some();
    if (playlist.kind == Some(PlaylistKind::Audio) && (has_video || !has_audio))
        || (playlist.kind == Some(PlaylistKind::Text) && (has_video || has_audio))
        || (inband && !has_audio)
    {
        return Err(Failure::source("ambiguous-source-topology"));
    }
    let mut specs = Vec::new();
    for stream in streams {
        let codec = match stream.get("codec_name") {
            None => Some(""),
            Some(value) => value.as_str(),
        };
        let profile = stream.get("profile");
        if codec == Some("aac")
            && !matches!(profile, None | Some(Value::Null))
            && !is_str(profile, "LC")
        {
            return Err(Failure::source("unsupported-aac-profile"));
        }
        let ctype = match stream.get("codec_type").and_then(Value::as_str) {
            Some("video") => Some(ContentType::Video),
            Some("audio") => Some(ContentType::Audio),
            Some("subtitle") => Some(ContentType::Text),
            _ => None,
        };
        let (Some(ctype), Some(codec)) = (ctype, codec) else {
            return Err(Failure::source("unsupported-source-track"));
        };
        let admitted = matches!(
            (ctype, codec),
            (ContentType::Video, "h264" | "hevc")
                | (ContentType::Audio, "aac")
                | (ContentType::Text, "webvtt" | "ttml")
        );
        if !admitted {
            return Err(Failure::source("unsupported-source-track"));
        }
        let mut container = playlist.containers[0].to_string();
        if codec == "ttml" {
            // IMSC qualification is the packaging validator's; standalone TTML is not fMP4.
            if playlist.plan.init.is_none() {
                return Err(Failure::source("unsupported-subtitle-profile"));
            }
            container = "imsc".to_string();
        }
        let muxed = has_video && has_audio;
        let attrs = &playlist.attrs;
        let metadata = if ctype == ContentType::Audio && muxed {
            attrs
                .inband_audio
                .clone()
                .unwrap_or_else(|| attrs.attrs.clone())
        } else {
            attrs.attrs.clone()
        };
        let group = if ctype == ContentType::Audio && muxed {
            attrs.get("AUDIO").unwrap_or("muxed-audio")
        } else {
            attrs.get("GROUP-ID").unwrap_or("")
        };
        specs.push(Spec {
            ctype,
            container,
            codec: codec.to_string(),
            stream: stream.clone(),
            metadata,
            group: group.to_string(),
            muxed,
        });
    }
    Ok(specs)
}

/// A probe's `width` / `height`: a non-negative integer or nothing. ffprobe prints nothing
/// else; a stand-in prober that does is refused (`media-probe-failed`) where Python would
/// carry the value into the record.
fn dimension(stream: &ProbeStream, key: &str) -> Result<Option<u64>, Failure> {
    match stream.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| Failure::source("media-probe-failed")),
    }
}

/// `int(stream.get("index", 0))`.
fn stream_index(stream: &ProbeStream) -> Result<i64, Failure> {
    match stream.get("index") {
        None => Ok(0),
        Some(Value::Bool(flag)) => Ok(i64::from(*flag)),
        Some(Value::Number(number)) => number.as_i64().map_or_else(
            || {
                number
                    .as_f64()
                    .filter(|float| float.is_finite() && float.abs() < 9.0e18)
                    .map(|float| {
                        #[allow(clippy::cast_possible_truncation)] // int truncates; in range
                        let truncated = float.trunc() as i64;
                        truncated
                    })
                    .ok_or_else(|| escaped("OverflowError"))
            },
            Ok,
        ),
        Some(Value::String(text)) => super::pytext::py_int(text)
            .and_then(|value| i64::try_from(value).ok())
            .ok_or_else(|| escaped("ValueError")),
        Some(_) => Err(escaped("TypeError")),
    }
}

/// `_build_track`.
fn build_track(
    identifier: String,
    spec: &Spec,
    playlist: &PlaylistState,
) -> Result<SourceTrack, Failure> {
    let attrs = &playlist.attrs;
    let metadata = &spec.metadata;
    let segments = playlist
        .plan
        .segments
        .iter()
        .zip(&playlist.segments)
        .enumerate()
        .map(|(index, (planned, landed))| SourceSegment {
            index: i64::try_from(index).expect("segment count"),
            path: landed.final_path.clone().expect("every segment landed"),
            duration: planned.duration.expect("a segment's EXTINF"),
            sha256: landed.sha256.clone().expect("every segment landed"),
            byte_range: planned.byte_range,
        })
        .collect();
    let bandwidth = match attrs.get("BANDWIDTH") {
        None => None,
        Some(value) => Some(
            playlist::positive_int(value)?
                .parse::<u64>()
                .map_err(|_| Failure::source("invalid-playlist"))?,
        ),
    };
    let default = metadata.get("DEFAULT") == Some("YES");
    let autoselect = metadata
        .get("AUTOSELECT")
        .unwrap_or(if default { "YES" } else { "NO" })
        == "YES";
    let profile = match spec.stream.get("profile") {
        None => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(_) => return Err(Failure::source("media-probe-failed")),
    };
    Ok(SourceTrack {
        id: identifier,
        content_type: spec.ctype.as_str().to_string(),
        container: spec.container.clone(),
        codec: spec.codec.clone(),
        segments,
        init_path: playlist.init_landed.get().map(|(path, _)| path.clone()),
        init_sha256: playlist.init_sha256.clone(),
        language: metadata.get("LANGUAGE").unwrap_or("und").to_string(),
        role: metadata
            .get("CHARACTERISTICS")
            .unwrap_or("main")
            .to_string(),
        group_id: spec.group.clone(),
        name: metadata.get("NAME").unwrap_or("").to_string(),
        width: dimension(&spec.stream, "width")?,
        height: dimension(&spec.stream, "height")?,
        bandwidth,
        codecs: attrs.get("CODECS").unwrap_or("").to_string(),
        stream_index: stream_index(&spec.stream)?,
        playlist_id: playlist.rendition_id(),
        muxed: spec.muxed,
        audio_group: attrs.get("AUDIO").unwrap_or("").to_string(),
        subtitle_group: attrs.get("SUBTITLES").unwrap_or("").to_string(),
        profile,
        is_default: default,
        autoselect,
    })
}
