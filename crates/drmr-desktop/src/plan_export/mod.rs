//! A single native owner for private copies of saved execution plans.

mod files;

use crate::ConsoleOrigin;
use files::{Destination, MediaFolders, OwnedFile};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;
use url::Url;

const MAX_PLAN_BYTES: usize = 32 * 1024 * 1024;
const FETCH_BUDGET: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice {
    Saved,
    Failed,
    ChoosePrivateFile,
    CleanupUnconfirmed,
}

/// The chooser and its eventual callback belong to this exact generation.
pub struct Selection {
    id: u64,
    directory: PathBuf,
}

impl Selection {
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn into_id(self) -> u64 {
        self.id
    }
}

enum Phase {
    Choosing,
    Ready(PathBuf),
    Working,
    CleanupFailed,
}

struct Attempt {
    id: u64,
    url: Url,
    phase: Phase,
}

#[derive(Default)]
struct State {
    stopping: bool,
    generation: u64,
    current: Option<Attempt>,
    pending_dialog: Option<Selection>,
    partial: Option<OwnedFile>,
    worker_failed: bool,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
    origin: ConsoleOrigin,
    media: MediaFolders,
    directory: PathBuf,
    notice: Box<dyn Fn(Notice) + Send + Sync>,
    choose: Box<dyn Fn(Selection) + Send + Sync>,
    fetch_budget: Duration,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn active(state: &State, id: u64) -> bool {
        !state.stopping
            && state
                .current
                .as_ref()
                .is_some_and(|attempt| attempt.id == id)
    }

    fn cancelled(&self, id: u64) -> bool {
        !Self::active(&self.lock(), id)
    }
}

/// Own the service thread until orderly shutdown has joined it. The service
/// waits for an outstanding save-dialog callback before it can exit.
pub struct PlanExports {
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl PlanExports {
    pub fn start(
        origin: ConsoleOrigin,
        input: &Path,
        served_output: &Path,
        directory: &Path,
        notice: impl Fn(Notice) + Send + Sync + 'static,
        choose: impl Fn(Selection) + Send + Sync + 'static,
    ) -> io::Result<Self> {
        Self::start_with_budget(
            origin,
            input,
            served_output,
            directory,
            notice,
            choose,
            FETCH_BUDGET,
        )
    }

    fn start_with_budget(
        origin: ConsoleOrigin,
        input: &Path,
        served_output: &Path,
        directory: &Path,
        notice: impl Fn(Notice) + Send + Sync + 'static,
        choose: impl Fn(Selection) + Send + Sync + 'static,
        fetch_budget: Duration,
    ) -> io::Result<Self> {
        let media = MediaFolders::capture(input, served_output)?;
        let directory = media.prepare_directory(directory)?;
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            origin,
            media,
            directory,
            notice: Box::new(notice),
            choose: Box::new(choose),
            fetch_budget,
        });
        let service = shared.clone();
        let worker = std::thread::Builder::new()
            .name("drmr-plan-export".into())
            .spawn(move || run(&service))?;
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Reserve one native chooser. A second click cannot replace its owner.
    pub fn reserve(&self, url: &Url) -> Option<Selection> {
        if !self.shared.origin.permits_plan_export(url) {
            return None;
        }
        let mut state = self.shared.lock();
        if state.stopping || state.current.is_some() || state.worker_failed {
            return None;
        }
        state.generation = state.generation.checked_add(1)?;
        let id = state.generation;
        state.current = Some(Attempt {
            id,
            url: url.clone(),
            phase: Phase::Choosing,
        });
        Some(Selection {
            id,
            directory: self.shared.directory.clone(),
        })
    }

    /// Queue the chooser on the owned service thread, keeping native panel
    /// initialization out of the platform download callback.
    pub fn request(&self, url: &Url) -> bool {
        let Some(selection) = self.reserve(url) else {
            return false;
        };
        let mut state = self.shared.lock();
        if !Shared::active(&state, selection.id) {
            if state
                .current
                .as_ref()
                .is_some_and(|attempt| attempt.id == selection.id)
            {
                state.current = None;
            }
            self.shared.changed.notify_all();
            return false;
        }
        state.pending_dialog = Some(selection);
        self.shared.changed.notify_all();
        true
    }

    pub fn chooser_is_current(&self, selection: &Selection) -> bool {
        let state = self.shared.lock();
        Shared::active(&state, selection.id)
            && state
                .current
                .as_ref()
                .is_some_and(|attempt| matches!(attempt.phase, Phase::Choosing))
    }

    /// A late callback releases only its own chooser; closing discards its path.
    pub fn selected(&self, selection: Selection, path: Option<PathBuf>) {
        let id = selection.into_id();
        let mut state = self.shared.lock();
        if !state
            .current
            .as_ref()
            .is_some_and(|attempt| attempt.id == id && matches!(attempt.phase, Phase::Choosing))
        {
            return;
        }
        if state.stopping || path.is_none() {
            state.current = None;
        } else if let Some(path) = path
            && let Some(attempt) = state.current.as_mut()
        {
            attempt.phase = Phase::Ready(path);
        }
        self.shared.changed.notify_all();
    }

    /// Close admission under the same fence used for creation and every write.
    pub fn request_stop(&self) -> bool {
        let mut state = self.shared.lock();
        state.stopping = true;
        if state.pending_dialog.take().is_some()
            || state
                .current
                .as_ref()
                .is_some_and(|attempt| matches!(attempt.phase, Phase::Ready(_)))
        {
            state.current = None;
        }
        let choosing = state
            .current
            .as_ref()
            .is_some_and(|attempt| matches!(attempt.phase, Phase::Choosing));
        self.shared.changed.notify_all();
        choosing
    }

    /// Call on the shutdown worker, never the `WebView` thread. An open chooser
    /// remains owned until the native callback returns; a failed cleanup remains
    /// owned after joining and can be retried by another orderly close request.
    pub fn shutdown(&self) -> bool {
        self.request_stop();
        // Keep the join owner locked until join and cleanup finish. Concurrent
        // close callers cannot mistake a temporarily taken handle for success.
        let mut owner = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(worker) = owner.take()
            && worker.join().is_err()
        {
            self.shared.lock().worker_failed = true;
        }
        let mut state = self.shared.lock();
        if let Some(partial) = state.partial.as_ref() {
            if partial.cleanup().is_err() {
                return false;
            }
            state.partial = None;
            state.current = None;
        }
        !state.worker_failed && state.current.is_none()
    }
}

fn run(shared: &Shared) {
    'service: loop {
        let (id, url, path) = {
            let mut state = shared.lock();
            loop {
                if state.stopping
                    && !state
                        .current
                        .as_ref()
                        .is_some_and(|attempt| matches!(attempt.phase, Phase::Choosing))
                {
                    return;
                }
                if let Some(selection) = state.pending_dialog.take() {
                    drop(state);
                    (shared.choose)(selection);
                    continue 'service;
                }
                if let Some(attempt) = state.current.as_mut()
                    && let Phase::Ready(path) = &attempt.phase
                {
                    let pending = (attempt.id, attempt.url.clone(), path.clone());
                    attempt.phase = Phase::Working;
                    break pending;
                }
                state = shared
                    .changed
                    .wait(state)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let result = export(shared, id, &url, &path);
        if let Some(notice) = complete(shared, id, result) {
            (shared.notice)(notice);
        }
    }
}

fn complete(shared: &Shared, id: u64, result: Result<(), ExportError>) -> Option<Notice> {
    let mut state = shared.lock();
    if state
        .current
        .as_ref()
        .is_none_or(|attempt| attempt.id != id)
    {
        state.worker_failed = true;
        return None;
    }
    let notice = if state
        .partial
        .as_ref()
        .is_some_and(|partial| partial.cleanup().is_err())
    {
        if let Some(attempt) = state.current.as_mut() {
            attempt.phase = Phase::CleanupFailed;
        }
        Notice::CleanupUnconfirmed
    } else {
        state.partial = None;
        state.current = None;
        match result {
            Ok(()) => Notice::Saved,
            Err(ExportError::PrivateDestination) => Notice::ChoosePrivateFile,
            Err(ExportError::Failed | ExportError::Cancelled) => Notice::Failed,
        }
    };
    shared.changed.notify_all();
    (!state.stopping).then_some(notice)
}

#[derive(Clone, Copy)]
enum ExportError {
    Cancelled,
    PrivateDestination,
    Failed,
}

fn fetch(shared: &Shared, id: u64, url: &Url) -> Result<Vec<u8>, ExportError> {
    if shared.cancelled(id) || !shared.origin.permits_plan_export(url) {
        return Err(ExportError::Cancelled);
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .max_redirects(0)
        .http_status_as_error(false)
        .timeout_global(Some(shared.fetch_budget))
        .build()
        .into();
    let mut response = agent
        .get(url.as_str())
        .call()
        .map_err(|_| ExportError::Failed)?;
    if response.status().as_u16() != 200
        || response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .is_none_or(|value| {
                value
                    .split(';')
                    .next()
                    .is_none_or(|mime| !mime.trim().eq_ignore_ascii_case("application/json"))
            })
        || response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .parse::<usize>()
                    .ok()
                    .is_none_or(|length| length > MAX_PLAN_BYTES)
            })
    {
        return Err(ExportError::Failed);
    }
    let reader = response
        .body_mut()
        .with_config()
        .limit((MAX_PLAN_BYTES + 1) as u64)
        .reader();
    read_plan(reader, || shared.cancelled(id), MAX_PLAN_BYTES)
}

fn read_plan(
    mut reader: impl Read,
    cancelled: impl Fn() -> bool,
    limit: usize,
) -> Result<Vec<u8>, ExportError> {
    let mut bytes = Vec::new();
    let mut chunk = [0; 16 * 1024];
    loop {
        if cancelled() {
            return Err(ExportError::Cancelled);
        }
        let length = reader.read(&mut chunk).map_err(|_| ExportError::Failed)?;
        if length == 0 {
            return Ok(bytes);
        }
        if bytes.len() + length > limit {
            return Err(ExportError::Failed);
        }
        bytes.extend_from_slice(&chunk[..length]);
    }
}

fn export(shared: &Shared, id: u64, url: &Url, path: &Path) -> Result<(), ExportError> {
    let destination =
        Destination::capture(path, &shared.media).map_err(|_| ExportError::PrivateDestination)?;
    let bytes = fetch(shared, id, url)?;
    let mut state = shared.lock();
    if !Shared::active(&state, id) {
        return Err(ExportError::Cancelled);
    }
    state.partial = Some(
        destination
            .create(&shared.media)
            .map_err(|_| ExportError::Failed)?,
    );
    drop(state);
    write_plan(shared, id, &bytes)
}

fn write_plan(shared: &Shared, id: u64, bytes: &[u8]) -> Result<(), ExportError> {
    for chunk in bytes.chunks(16 * 1024) {
        let mut state = shared.lock();
        if !Shared::active(&state, id) {
            return Err(ExportError::Cancelled);
        }
        let partial = state.partial.as_mut().ok_or(ExportError::Failed)?;
        partial
            .check(&shared.media)
            .map_err(|_| ExportError::Failed)?;
        partial
            .file
            .write_all(chunk)
            .map_err(|_| ExportError::Failed)?;
    }
    let mut state = shared.lock();
    if !Shared::active(&state, id) {
        return Err(ExportError::Cancelled);
    }
    let partial = state.partial.as_mut().ok_or(ExportError::Failed)?;
    partial
        .check(&shared.media)
        .map_err(|_| ExportError::Failed)?;
    partial.file.sync_all().map_err(|_| ExportError::Failed)?;
    partial
        .check(&shared.media)
        .map_err(|_| ExportError::Failed)?;
    // Publishing the completed copy and closing admission use the same mutex.
    state.partial = None;
    Ok(())
}

#[cfg(test)]
mod tests;
