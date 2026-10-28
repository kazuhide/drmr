//! Numeric observations describe work, never media validity or publication authority.

use crate::events::{Record, Sink};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_TRACKS: usize = 64;
const MAX_PROBES: usize = 1024;
const SAMPLE_BUDGET: Duration = Duration::from_millis(5);
const MAX_NUMBER: u32 = 100_130;
static NEXT_BATCH: AtomicU64 = AtomicU64::new(1);

mod preparation;
pub(super) use preparation::{PreparationContext, scoped};
mod units;
pub(super) use units::{Operation, UnitTask, Units};
mod integrity;
pub(super) use integrity::Integrity;

#[cfg(test)]
mod integrity_tests;
#[cfg(test)]
mod units_tests;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Subject {
    Source,
    Result,
    Output,
}
impl Subject {
    fn token(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Result => "result",
            Self::Output => "output",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Purpose {
    Conversion,
    Verification,
    Admission,
}
impl Purpose {
    fn token(self) -> &'static str {
        match self {
            Self::Conversion => "conversion",
            Self::Verification => "verification",
            Self::Admission => "admission",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Unit {
    Renditions,
    Tracks,
}
impl Unit {
    fn token(self) -> &'static str {
        match self {
            Self::Renditions => "renditions",
            Self::Tracks => "tracks",
        }
    }
}

fn next_batch() -> Option<u64> {
    let mut id = NEXT_BATCH.load(Ordering::Relaxed);
    loop {
        let next = id.checked_add(1)?;
        match NEXT_BATCH.compare_exchange_weak(id, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Some(id),
            Err(current) => id = current,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Task {
    Packaging,
    Comparison,
    Preparation(Subject, Purpose, Unit),
}
impl Task {
    fn token(self) -> &'static str {
        match self {
            Self::Packaging => "packaging",
            Self::Comparison => "comparison",
            Self::Preparation(..) => "preparation",
        }
    }
}

#[derive(Default)]
struct State {
    completed: u64,
    pairs: u64,
    terminal: bool,
}

pub(super) struct Batch {
    sink: Arc<dyn Sink>,
    id: u64,
    task: Task,
    total: u64,
    weights: Vec<u64>,
    state: Mutex<State>,
}
impl Batch {
    pub(super) fn units(
        sink: Option<Arc<dyn Sink>>,
        operation: Operation,
        subject: Subject,
        purpose: Purpose,
        unit: Unit,
        total: usize,
    ) -> Option<Units> {
        Units::new(sink?, operation, subject, purpose, unit, total)
    }

    fn new(sink: Option<Arc<dyn Sink>>, task: Task, total: u64, weights: Vec<u64>) -> Option<Self> {
        let sink = sink?;
        if total == 0 {
            return None;
        }
        let id = next_batch()?;
        let batch = Self {
            sink,
            id,
            task,
            total,
            weights,
            state: Mutex::new(State::default()),
        };
        batch.emit("started", &State::default());
        Some(batch)
    }

    fn packaging(sink: Option<Arc<dyn Sink>>, total: u64) -> Option<Self> {
        Self::new(sink, Task::Packaging, total, Vec::new())
    }

    /// Pair sizes are logical work weights, not measured process or filesystem I/O.
    pub(super) fn comparison(sink: Option<Arc<dyn Sink>>, weights: Vec<u64>) -> Option<Self> {
        if weights.is_empty() || weights.len() > MAX_TRACKS || weights.contains(&0) {
            return None;
        }
        let total = weights
            .iter()
            .try_fold(0_u64, |sum, value| sum.checked_add(*value))?;
        Self::new(sink, Task::Comparison, total, weights)
    }

    fn preparation(
        sink: Arc<dyn Sink>,
        subject: Subject,
        purpose: Purpose,
        unit: Unit,
        total: usize,
    ) -> Option<Self> {
        if total == 0 || total > MAX_TRACKS {
            return None;
        }
        Self::new(
            Some(sink),
            Task::Preparation(subject, purpose, unit),
            u64::try_from(total).ok()?,
            vec![1; total],
        )
    }

    pub(super) fn completed(&self, ordinal: usize) {
        let Some(weight) = self.weights.get(ordinal) else {
            return;
        };
        let bit = 1_u64 << ordinal;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.terminal || state.pairs & bit != 0 {
            return;
        }
        state.pairs |= bit;
        state.completed += weight;
        // Serialize cumulative snapshots, including concurrently completed pairs.
        self.emit_running(&mut state);
    }

    fn observed(&self, completed: u64) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.terminal || completed <= state.completed {
            return;
        }
        state.completed = completed;
        self.emit_running(&mut state);
    }

    fn emit_running(&self, state: &mut State) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.emit("running", state);
        }));
        if let Err(panic) = result {
            // Unconfirmed child cleanup can supersede the panic with a returned
            // error. Drop must not call the same failed observer a second time.
            state.terminal = true;
            std::panic::resume_unwind(panic);
        }
    }

    pub(super) fn finish(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.terminal || (self.task != Task::Packaging && state.completed != self.total) {
            return;
        }
        state.terminal = true;
        self.emit("finished", &state);
    }

    fn abort(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.terminal {
            state.terminal = true;
            self.emit("aborted", &state);
        }
    }

    fn silence(&self) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .terminal = true;
    }

    fn emit(&self, status: &'static str, state: &State) {
        let mut record: Record = serde_json::json!({
            "stage":"media-progress", "event":"progress", "task":self.task.token(),
            "state":status, "batch_id":self.id, "estimated":self.task == Task::Packaging,
        })
        .as_object()
        .expect("fixed numeric progress record")
        .clone();
        if let Task::Preparation(subject, purpose, unit) = self.task {
            record.insert("subject".into(), subject.token().into());
            record.insert("purpose".into(), purpose.token().into());
            record.insert("unit".into(), unit.token().into());
        } else {
            record.insert("completed_bytes".into(), state.completed.into());
            record.insert("total_bytes".into(), self.total.into());
        }
        if self.task == Task::Packaging {
            record.insert("provisional".into(), true.into());
        } else {
            record.insert("completed_units".into(), state.pairs.count_ones().into());
            record.insert("total_units".into(), self.weights.len().into());
        }
        self.sink.emit(&record);
    }
}
impl Drop for Batch {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.abort();
        }
    }
}

enum Entry {
    File(u64),
    Directory,
    Missing,
}
fn entry(path: &Path) -> Entry {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Entry::Missing;
    };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x400 != 0 {
            return Entry::Missing;
        }
    }
    if metadata.is_file() && metadata.len() <= super::MAX_MEDIA_BYTES {
        Entry::File(metadata.len())
    } else if metadata.is_dir() {
        Entry::Directory
    } else {
        Entry::Missing
    }
}

pub(super) fn file_bytes(path: &Path) -> Option<u64> {
    match entry(path) {
        Entry::File(bytes) => Some(bytes),
        Entry::Directory | Entry::Missing => None,
    }
}

struct Cursor {
    directory: PathBuf,
    number: u32,
    closed: u64,
    current: u64,
    initialization: Option<u64>,
}
impl Cursor {
    /// At most four metadata probes, never an open handle to the growing file.
    fn advance(&mut self, probe: &mut impl FnMut(&Path) -> Entry) -> bool {
        if !matches!(probe(&self.directory), Entry::Directory) {
            return false;
        }
        if self.initialization.is_none()
            && let Entry::File(bytes) = probe(&self.directory.join("init.mp4"))
            && bytes > 0
        {
            self.initialization = Some(bytes);
        }
        let path = self.directory.join(format!("seg_{}.m4s", self.number));
        let Entry::File(bytes) = probe(&path) else {
            return false;
        };
        self.current = self.current.max(bytes);
        if self.number >= MAX_NUMBER {
            return false;
        }
        let next = self.directory.join(format!("seg_{}.m4s", self.number + 1));
        if let Entry::File(bytes) = probe(&next) {
            // A successor permits moving this bounded cursor forward. These
            // sizes remain observations; complete media checks establish validity.
            self.closed += self.current;
            self.current = bytes;
            self.number += 1;
            true
        } else {
            false
        }
    }
    fn bytes(&self) -> u64 {
        // Fixed track, segment-count and per-file-size bounds fit within u64.
        self.closed + self.current + self.initialization.unwrap_or(0)
    }
}

struct Sampler {
    tracks: Vec<Cursor>,
    next: usize,
}
impl Sampler {
    fn new(root: &Path, tracks: usize) -> Option<Self> {
        if tracks == 0 || tracks > MAX_TRACKS {
            return None;
        }
        Some(Self {
            tracks: (0..tracks)
                .map(|index| Cursor {
                    directory: root.join(format!("track-{index:04}")),
                    number: 1,
                    closed: 0,
                    current: 0,
                    initialization: None,
                })
                .collect(),
            next: 0,
        })
    }
    fn sample(&mut self) -> u64 {
        self.sample_with(entry)
    }
    fn sample_with(&mut self, mut probe: impl FnMut(&Path) -> Entry) -> u64 {
        let began = Instant::now();
        self.sample_with_clock(&mut probe, || began.elapsed())
    }
    fn sample_with_clock(
        &mut self,
        mut probe: impl FnMut(&Path) -> Entry,
        mut elapsed: impl FnMut() -> Duration,
    ) -> u64 {
        let mut remaining = MAX_PROBES;
        let mut idle = 0;
        while remaining >= 4 && idle < self.tracks.len() && elapsed() < SAMPLE_BUDGET {
            let advanced = self.tracks[self.next].advance(&mut |path| {
                remaining -= 1;
                probe(path)
            });
            idle = if advanced { 0 } else { idle + 1 };
            self.next = (self.next + 1) % self.tracks.len();
        }
        self.tracks.iter().map(Cursor::bytes).sum()
    }
}

pub(super) struct Packaging {
    batch: Batch,
    sampler: Sampler,
}
impl Packaging {
    pub(super) fn new(sink: Option<Arc<dyn Sink>>, root: &Path, clear: &[PathBuf]) -> Option<Self> {
        let sink = sink?;
        let total = clear
            .iter()
            .try_fold(0_u64, |sum, path| sum.checked_add(file_bytes(path)?))?;
        let sampler = Sampler::new(root, clear.len())?;
        let batch = Batch::packaging(Some(sink), total)?;
        Some(Self { batch, sampler })
    }
    pub(super) fn poll(&mut self) {
        self.batch.observed(self.sampler.sample());
    }
    pub(super) fn finish(&self) {
        self.batch.finish();
    }
}

#[cfg(test)]
mod tests;
