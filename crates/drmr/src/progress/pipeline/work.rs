//! Provisional work observations never establish media validity or publication.
use drmr_core::events::Record;
use std::time::Duration;

pub(super) const SCALE: u32 = 1000;
pub(super) const SOURCE: u32 = 4 * SCALE;
pub(super) const OUTPUT: u32 = 8 * SCALE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    Conversion,
    Verification,
    Finalization,
}
impl Step {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "conversion" => Some(Self::Conversion),
            "verification" => Some(Self::Verification),
            "finalization" => Some(Self::Finalization),
            _ => None,
        }
    }
    pub(super) fn token(self) -> &'static str {
        match self {
            Self::Conversion => "conversion",
            Self::Verification => "verification",
            Self::Finalization => "finalization",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::progress) enum Task {
    Packaging,
    Comparison,
}
impl Task {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "packaging" => Some(Self::Packaging),
            "comparison" => Some(Self::Comparison),
            _ => None,
        }
    }
    fn token(self) -> &'static str {
        match self {
            Self::Packaging => "packaging",
            Self::Comparison => "comparison",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Started,
    Running,
    Finished,
    Aborted,
}
impl Status {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "started" => Some(Self::Started),
            "running" => Some(Self::Running),
            "finished" => Some(Self::Finished),
            "aborted" => Some(Self::Aborted),
            _ => None,
        }
    }
    fn token(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Running => "running",
            Self::Finished => "finished",
            Self::Aborted => "aborted",
        }
    }
    fn terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Aborted)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::progress) struct Media {
    pub(in crate::progress) task: Task,
    status: Status,
    id: u64,
    pub(in crate::progress) completed: u64,
    pub(in crate::progress) total: u64,
    pub(in crate::progress) units: Option<[u64; 2]>,
    estimated: bool,
}
impl Media {
    fn parse(record: &Record) -> Option<Self> {
        let text = |key| record.get(key).and_then(serde_json::Value::as_str);
        let number = |key| record.get(key).and_then(serde_json::Value::as_u64);
        let task = Task::parse(text("task")?)?;
        let status = Status::parse(text("state")?)?;
        let id = number("batch_id")?;
        let completed = number("completed_bytes")?;
        let total = number("total_bytes")?;
        let estimated = record.get("estimated")?.as_bool()?;
        let units = match (number("completed_units"), number("total_units")) {
            (None, None)
                if !record.contains_key("completed_units")
                    && !record.contains_key("total_units") =>
            {
                None
            }
            (Some(done), Some(total)) if total > 0 && total <= 64 && done <= total => {
                Some([done, total])
            }
            _ => return None,
        };
        if id == 0
            || total == 0
            || estimated != (task == Task::Packaging)
            || (task == Task::Comparison && completed > total)
            || (status == Status::Started
                && (completed != 0 || units.is_some_and(|[done, _]| done != 0)))
            || (status == Status::Finished
                && task == Task::Comparison
                && (completed != total || units.is_some_and(|[done, total]| done != total)))
        {
            return None;
        }
        Some(Self {
            task,
            status,
            id,
            completed,
            total,
            units,
            estimated,
        })
    }
    fn fraction(self) -> u32 {
        if self.status == Status::Finished {
            SCALE
        } else {
            let fraction = (u128::from(self.completed.min(self.total)) * u128::from(SCALE))
                / u128::from(self.total);
            u32::try_from(fraction).unwrap_or(SCALE).min(SCALE - 1)
        }
    }
    pub(super) fn json(self) -> serde_json::Value {
        let mut value = serde_json::json!({"task":self.task.token(),"state":self.status.token(),"batch_id":self.id,"completed_bytes":self.completed,"total_bytes":self.total,"estimated":self.estimated});
        if let Some([done, total]) = self.units {
            value["completed_units"] = done.into();
            value["total_units"] = total.into();
        }
        value
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Work {
    pub(super) step: Option<Step>,
    pub(super) media: Option<Media>,
    pub(super) observations: u64,
    pub(super) aborted: bool,
    last_id: u64,
    completed: u32,
}
impl Work {
    pub(super) fn step(&mut self, value: &str) -> bool {
        let Some(step) = Step::parse(value) else {
            return false;
        };
        if !matches!(
            (self.step, step),
            (None, Step::Conversion)
                | (Some(Step::Conversion), Step::Verification)
                | (Some(Step::Verification), Step::Finalization)
        ) {
            return false;
        }
        self.step = Some(step);
        self.media = None;
        self.completed = self.completed.max(match step {
            Step::Conversion => 0,
            Step::Verification => 4 * SCALE,
            Step::Finalization => 7 * SCALE,
        });
        true
    }
    pub(super) fn observe(&mut self, record: &Record) -> bool {
        let Some(next) = Media::parse(record) else {
            return false;
        };
        if self.aborted {
            return false;
        }
        let positive = if let Some(previous) = self.media.filter(|old| old.id == next.id) {
            if previous.status.terminal()
                || next.status == Status::Started
                || next.task != previous.task
                || next.total != previous.total
                || next.estimated != previous.estimated
                || next.completed < previous.completed
                || next.units.map(|[_, total]| total) != previous.units.map(|[_, total]| total)
                || matches!((previous.units, next.units), (Some([old, _]), Some([new, _])) if new < old)
                || next == previous
            {
                return false;
            }
            next.completed > previous.completed
                || matches!((previous.units, next.units), (Some([old, _]), Some([new, _])) if new > old)
        } else {
            if next.status != Status::Started
                || next.id <= self.last_id
                || self.media.is_some_and(|old| !old.status.terminal())
            {
                return false;
            }
            false
        };
        self.last_id = next.id;
        self.aborted |= next.status == Status::Aborted;
        self.observations = self.observations.saturating_add(u64::from(positive));
        self.media = Some(next);
        let work = match (self.step, next.task) {
            (Some(Step::Conversion), Task::Packaging) => SCALE + next.fraction(),
            (Some(Step::Conversion), Task::Comparison) => 2 * SCALE + 2 * next.fraction(),
            (Some(Step::Verification), Task::Comparison) => 5 * SCALE + 2 * next.fraction(),
            _ => self.completed,
        };
        self.completed = self.completed.max(work);
        true
    }
    pub(super) fn analysis(&mut self, completed: u64, total: u64, previous: u64) {
        if total == 0 {
            return;
        }
        self.observations = self
            .observations
            .saturating_add(u64::from(completed > previous));
        let fraction = u32::try_from(u128::from(completed) * u128::from(SCALE) / u128::from(total))
            .unwrap_or(SCALE)
            .min(SCALE - 1);
        let work = match self.step {
            Some(Step::Conversion) => fraction,
            Some(Step::Verification) => 4 * SCALE + fraction,
            _ => self.completed,
        };
        self.completed = self.completed.max(work);
    }
    pub(super) fn completed(self) -> u32 {
        self.completed.min(OUTPUT - 1)
    }
    pub(super) fn detail(self) -> Option<&'static str> {
        if self.aborted {
            return Some("Stopping output");
        }
        if let Some(media) = self.media {
            if !media.status.terminal() {
                return Some(match media.task {
                    Task::Packaging => "Writing media",
                    Task::Comparison => "Comparing media",
                });
            }
            if media.task == Task::Packaging {
                return Some("Checking converted media");
            }
        }
        self.step.map(|step| match step {
            Step::Conversion if self.media.is_some() => "Completing conversion checks",
            Step::Conversion => "Preparing output",
            Step::Verification => "Checking source and result",
            Step::Finalization => "Checking manifests and DRM",
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct Plan {
    pub(super) outputs: usize,
    pub(super) enabled: bool,
    pub(super) source_completed: u32,
}
impl Plan {
    pub(super) fn parse(record: &Record) -> Option<Self> {
        let outputs = usize::try_from(record.get("planned_outputs")?.as_u64()?).ok()?;
        let source_completed = u32::try_from(record.get("source_completed")?.as_u64()?).ok()?;
        (outputs > 0 && outputs <= 8 && source_completed <= 4).then_some(Self {
            outputs,
            enabled: record.get("estimates_enabled")?.as_bool()?,
            source_completed: source_completed * SCALE,
        })
    }
    pub(super) fn total(self) -> u32 {
        // Every output preserves the same original content, so the common byte scale
        // cancels. These input-sized logical passes are work estimates, not IO counters.
        let outputs = u32::try_from(self.outputs).expect("validated output count");
        SOURCE + OUTPUT * outputs + SCALE * (1 + outputs)
    }
}

#[derive(Clone, Copy)]
pub(in crate::progress) struct Estimate {
    pub(in crate::progress) percent: u32,
    pub(in crate::progress) remaining: Option<Duration>,
}
impl Estimate {
    pub(super) fn json(self) -> serde_json::Value {
        serde_json::json!({"estimated":true,"basis":"logical-media-passes","percent":self.percent,"remaining_ms":self.remaining.map(super::millis)})
    }
}
