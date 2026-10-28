//! Actual task starts and exits, independent of CPU admission and media validity.
use super::{Arc, MAX_TRACKS, Mutex, PoisonError, Purpose, Record, Sink, Subject, Unit};
use crate::inspection::TrackKind;

#[derive(Clone, Copy)]
pub(in crate::conversion) enum Operation {
    Preparation,
    Snapshot,
    Normalization,
    SourceComparison,
    Comparison,
}
impl Operation {
    fn token(self) -> &'static str {
        match self {
            Self::Preparation => "preparation",
            Self::Snapshot => "snapshot",
            Self::Normalization => "normalization",
            Self::SourceComparison => "source-comparison",
            Self::Comparison => "comparison",
        }
    }
}

#[derive(Clone, Copy, serde::Serialize)]
struct Description {
    kind: TrackKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    resolution: Option<[u32; 2]>,
}
impl Description {
    fn new(kind: TrackKind, resolution: Option<(u64, u64)>) -> Option<Self> {
        if kind == TrackKind::Unknown {
            return None;
        }
        let resolution = if kind == TrackKind::Video {
            resolution.and_then(|(width, height)| {
                let width = u32::try_from(width).ok()?;
                let height = u32::try_from(height).ok()?;
                (width > 0 && height > 0).then_some([width, height])
            })
        } else {
            None
        };
        Some(Self { kind, resolution })
    }
}

struct State {
    started: u64,
    active: u64,
    completed: u64,
    stopped: u64,
    terminal: bool,
    descriptions: [Option<Description>; MAX_TRACKS],
}
impl Default for State {
    fn default() -> Self {
        Self {
            started: 0,
            active: 0,
            completed: 0,
            stopped: 0,
            terminal: false,
            descriptions: [None; MAX_TRACKS],
        }
    }
}
struct Inner {
    sink: Arc<dyn Sink>,
    id: u64,
    operation: Operation,
    subject: Subject,
    purpose: Purpose,
    unit: Unit,
    total: usize,
    state: Mutex<State>,
}
impl Inner {
    fn emit(&self, status: &'static str, state: &mut State) {
        let unit_states: Vec<_> = (0..self.total)
            .map(|ordinal| {
                let bit = 1_u64 << ordinal;
                if state.completed & bit != 0 {
                    "completed"
                } else if state.stopped & bit != 0 {
                    "stopped"
                } else if state.active & bit != 0 {
                    "active"
                } else {
                    "waiting"
                }
            })
            .collect();
        let record: Record = serde_json::json!({
            "stage":"rendition-progress", "event":"progress", "state":status,
            "batch_id":self.id, "operation":self.operation.token(),
            "subject":self.subject.token(), "purpose":self.purpose.token(),
            "unit":self.unit.token(), "total_units":self.total,
            "started_units":state.started.count_ones(),
            "active_units":state.active.count_ones(),
            "waiting_units":self.total - state.started.count_ones() as usize,
            "completed_units":state.completed.count_ones(),
            "stopped_units":state.stopped.count_ones(),
            "unit_states":unit_states,
            "unit_descriptions":&state.descriptions[..self.total],
        })
        .as_object()
        .expect("fixed typed rendition progress")
        .clone();
        let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.sink.emit(&record);
        }));
        if let Err(panic) = observed {
            state.terminal = true;
            state.stopped |= state.active;
            state.active = 0;
            if !std::thread::panicking() {
                std::panic::resume_unwind(panic);
            }
        }
    }

    fn describe(&self, ordinal: usize, kind: TrackKind, resolution: Option<(u64, u64)>) -> bool {
        if ordinal >= self.total {
            return false;
        }
        let Some(description) = Description::new(kind, resolution) else {
            return false;
        };
        let bit = 1_u64 << ordinal;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.terminal
            || (state.completed | state.stopped) & bit != 0
            || state.descriptions[ordinal].is_some()
        {
            return false;
        }
        state.descriptions[ordinal] = Some(description);
        self.emit("running", &mut state);
        true
    }

    fn complete(&self, bit: u64, success: bool) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.terminal || state.active & bit == 0 {
            return;
        }
        state.active &= !bit;
        if success {
            state.completed |= bit;
        } else {
            state.stopped |= bit;
        }
        self.emit("running", &mut state);
    }

    fn close(&self, finish: bool) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.terminal {
            return;
        }
        if finish && state.completed.count_ones() as usize != self.total {
            return;
        }
        state.terminal = true;
        state.stopped |= state.active;
        state.active = 0;
        self.emit(if finish { "finished" } else { "aborted" }, &mut state);
    }
}

pub(in crate::conversion) struct Units(Arc<Inner>);
impl Units {
    pub(super) fn new(
        sink: Arc<dyn Sink>,
        operation: Operation,
        subject: Subject,
        purpose: Purpose,
        unit: Unit,
        total: usize,
    ) -> Option<Self> {
        if total == 0 || total > MAX_TRACKS {
            return None;
        }
        let inner = Inner {
            sink,
            id: super::next_batch()?,
            operation,
            subject,
            purpose,
            unit,
            total,
            state: Mutex::default(),
        };
        inner.emit("started", &mut State::default());
        Some(Self(Arc::new(inner)))
    }
    pub(in crate::conversion) fn start(&self, ordinal: usize) -> Option<UnitTask> {
        if ordinal >= self.0.total {
            return None;
        }
        let bit = 1_u64 << ordinal;
        let mut state = self.0.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.terminal || state.started & bit != 0 {
            return None;
        }
        state.started |= bit;
        state.active |= bit;
        self.0.emit("running", &mut state);
        Some(UnitTask {
            inner: self.0.clone(),
            bit,
            complete: false,
        })
    }
    pub(in crate::conversion) fn describe(
        &self,
        ordinal: usize,
        kind: TrackKind,
        resolution: Option<(u64, u64)>,
    ) -> bool {
        self.0.describe(ordinal, kind, resolution)
    }
    pub(in crate::conversion) fn finish(&self) {
        self.0.close(true);
    }
}
impl Drop for Units {
    fn drop(&mut self) {
        self.0.close(false);
    }
}

pub(in crate::conversion) struct UnitTask {
    inner: Arc<Inner>,
    bit: u64,
    complete: bool,
}
impl UnitTask {
    pub(in crate::conversion) fn describe(
        &self,
        kind: TrackKind,
        resolution: Option<(u64, u64)>,
    ) -> bool {
        self.inner
            .describe(self.bit.trailing_zeros() as usize, kind, resolution)
    }
    pub(in crate::conversion) fn finish(mut self) {
        self.complete = true;
        self.inner.complete(self.bit, true);
    }
}
impl Drop for UnitTask {
    fn drop(&mut self) {
        if !self.complete {
            self.inner.complete(self.bit, false);
        }
    }
}
