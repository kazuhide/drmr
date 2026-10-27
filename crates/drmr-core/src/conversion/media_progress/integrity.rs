//! Observe existing inventory reads without retaining paths or allocating per file.
use super::{Arc, Duration, Instant, Mutex, PoisonError, Record, Sink, next_batch};

const INTERVAL: Duration = Duration::from_millis(250);
const MAX_FILES: usize = 100_130;

struct State {
    bytes: u64,
    files: usize,
    last: Instant,
    terminal: bool,
}

pub(in crate::conversion) struct Integrity {
    sink: Arc<dyn Sink>,
    id: u64,
    bytes: u64,
    files: usize,
    state: Mutex<State>,
}
impl Integrity {
    pub(in crate::conversion) fn new(
        sink: Option<Arc<dyn Sink>>,
        bytes: u64,
        files: usize,
    ) -> Option<Self> {
        let sink = sink?;
        if bytes == 0 || files == 0 || files > MAX_FILES {
            return None;
        }
        let batch = Self {
            sink,
            id: next_batch()?,
            bytes,
            files,
            state: Mutex::new(State {
                bytes: 0,
                files: 0,
                last: Instant::now(),
                terminal: false,
            }),
        };
        batch.emit(
            "started",
            &batch.state.lock().unwrap_or_else(PoisonError::into_inner),
        );
        Some(batch)
    }

    pub(in crate::conversion) fn read(&self, bytes: u64) {
        self.advance(bytes, 0, Instant::now());
    }
    pub(in crate::conversion) fn completed_file(&self) {
        self.advance(0, 1, Instant::now());
    }

    pub(super) fn advance(&self, bytes: u64, files: usize, now: Instant) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.terminal {
            return;
        }
        let counts = state
            .bytes
            .checked_add(bytes)
            .zip(state.files.checked_add(files));
        let Some((bytes, files)) =
            counts.filter(|(bytes, files)| *bytes <= self.bytes && *files <= self.files)
        else {
            state.terminal = true;
            self.emit("aborted", &state);
            return;
        };
        state.bytes = bytes;
        state.files = files;
        if now.checked_duration_since(state.last).unwrap_or_default() < INTERVAL {
            return;
        }
        state.last = now;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.emit("running", &state);
        }));
        if let Err(panic) = result {
            state.terminal = true;
            std::panic::resume_unwind(panic);
        }
    }

    pub(in crate::conversion) fn finish(&self) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.terminal && state.bytes == self.bytes && state.files == self.files {
            state.terminal = true;
            self.emit("finished", &state);
        }
    }

    fn emit(&self, status: &'static str, state: &State) {
        let record: Record = serde_json::json!({
            "stage":"media-progress", "event":"progress", "task":"integrity",
            "state":status, "batch_id":self.id, "estimated":false, "unit":"files",
            "completed_bytes":state.bytes, "total_bytes":self.bytes,
            "completed_units":state.files, "total_units":self.files,
        })
        .as_object()
        .expect("fixed numeric integrity record")
        .clone();
        self.sink.emit(&record);
    }
}
impl Drop for Integrity {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.terminal {
            state.terminal = true;
            self.emit("aborted", &state);
        }
    }
}
