//! Only bounded, typed observations cross the private progress boundary.
use super::SCALE;
use drmr_core::events::Record;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::progress) enum Task {
    Packaging,
    Comparison,
    Preparation,
    Integrity,
}
impl Task {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "packaging" => Some(Self::Packaging),
            "comparison" => Some(Self::Comparison),
            "preparation" => Some(Self::Preparation),
            "integrity" => Some(Self::Integrity),
            _ => None,
        }
    }
    fn token(self) -> &'static str {
        match self {
            Self::Packaging => "packaging",
            Self::Comparison => "comparison",
            Self::Preparation => "preparation",
            Self::Integrity => "integrity",
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::progress::pipeline) enum Subject {
    Source,
    Result,
    Output,
}
impl Subject {
    pub(in crate::progress::pipeline) fn parse(value: &str) -> Option<Self> {
        match value {
            "source" => Some(Self::Source),
            "result" => Some(Self::Result),
            "output" => Some(Self::Output),
            _ => None,
        }
    }
    pub(in crate::progress::pipeline) fn token(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Result => "result",
            Self::Output => "output",
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::progress::pipeline) enum Purpose {
    Conversion,
    Verification,
    Admission,
}
impl Purpose {
    pub(in crate::progress::pipeline) fn parse(value: &str) -> Option<Self> {
        match value {
            "conversion" => Some(Self::Conversion),
            "verification" => Some(Self::Verification),
            "admission" => Some(Self::Admission),
            _ => None,
        }
    }
    pub(in crate::progress::pipeline) fn token(self) -> &'static str {
        match self {
            Self::Conversion => "conversion",
            Self::Verification => "verification",
            Self::Admission => "admission",
        }
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::progress) enum Unit {
    Tracks,
    Renditions,
    Files,
}
impl Unit {
    pub(in crate::progress::pipeline) fn parse(value: &str) -> Option<Self> {
        match value {
            "tracks" => Some(Self::Tracks),
            "renditions" => Some(Self::Renditions),
            "files" => Some(Self::Files),
            _ => None,
        }
    }
    pub(in crate::progress::pipeline) fn token(self) -> &'static str {
        match self {
            Self::Tracks => "tracks",
            Self::Renditions => "renditions",
            Self::Files => "files",
        }
    }
    pub(in crate::progress) fn label(self) -> &'static str {
        self.token()
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
    pub(in crate::progress::pipeline) id: u64,
    pub(in crate::progress::pipeline) subject: Option<Subject>,
    pub(in crate::progress::pipeline) purpose: Option<Purpose>,
    pub(in crate::progress) bytes: Option<[u64; 2]>,
    pub(in crate::progress) units: Option<[u64; 2]>,
    pub(in crate::progress) unit: Option<Unit>,
    estimated: bool,
}
impl Media {
    pub(in crate::progress::pipeline) fn parse(record: &Record) -> Option<Self> {
        let text = |key| record.get(key).and_then(serde_json::Value::as_str);
        let number = |key| record.get(key).and_then(serde_json::Value::as_u64);
        let task = Task::parse(text("task")?)?;
        let status = Status::parse(text("state")?)?;
        let id = number("batch_id")?;
        let estimated = record.get("estimated")?.as_bool()?;
        let bytes = pair(record, "completed_bytes", "total_bytes", false).ok()?;
        let units = pair(record, "completed_units", "total_units", true).ok()?;
        let unit = match text("unit") {
            Some(value) => Some(Unit::parse(value)?),
            None if !record.contains_key("unit") => None,
            _ => return None,
        };
        let (subject, purpose) = if task == Task::Preparation {
            (
                Some(Subject::parse(text("subject")?)?),
                Some(Purpose::parse(text("purpose")?)?),
            )
        } else {
            (None, None)
        };
        if id == 0
            || estimated != (task == Task::Packaging)
            || (task == Task::Preparation
                && (bytes.is_some()
                    || units.is_none()
                    || !matches!(unit, Some(Unit::Tracks | Unit::Renditions))))
            || (task != Task::Preparation && bytes.is_none())
            || (task == Task::Integrity
                && (unit != Some(Unit::Files)
                    || units.is_none()
                    || units.is_some_and(|[_, total]| total > 100_130)))
            || (task == Task::Comparison && units.is_some_and(|[_, total]| total > 64))
            || (task == Task::Preparation && units.is_some_and(|[_, total]| total > 64))
            || (task != Task::Packaging && bytes.is_some_and(|[done, total]| done > total))
            || (status == Status::Started
                && (bytes.is_some_and(|[done, _]| done != 0)
                    || units.is_some_and(|[done, _]| done != 0)))
            || (status == Status::Finished
                && task != Task::Packaging
                && (bytes.is_some_and(|[done, total]| done != total)
                    || units.is_some_and(|[done, total]| done != total)))
        {
            return None;
        }
        Some(Self {
            task,
            status,
            id,
            subject,
            purpose,
            bytes,
            units,
            unit,
            estimated,
        })
    }
    pub(in crate::progress::pipeline) fn fraction(self) -> u32 {
        if self.finished() {
            return SCALE;
        }
        let [done, total] = self.bytes.or(self.units).expect("validated media counters");
        u32::try_from(u128::from(done.min(total)) * u128::from(SCALE) / u128::from(total))
            .unwrap_or(SCALE)
            .min(SCALE - 1)
    }
    pub(in crate::progress::pipeline) fn finished(self) -> bool {
        self.status == Status::Finished
    }
    pub(in crate::progress::pipeline) fn aborted(self) -> bool {
        self.status == Status::Aborted
    }
    pub(in crate::progress) fn checking(self) -> bool {
        self.task == Task::Preparation
            && !self.finished()
            && self.units.is_some_and(|[done, total]| done == total)
    }
    pub(in crate::progress) fn active(self) -> bool {
        !self.status.terminal()
    }
    pub(in crate::progress::pipeline) fn json(self) -> serde_json::Value {
        let mut value = serde_json::json!({"task":self.task.token(),"state":self.status.token(),"batch_id":self.id,"estimated":self.estimated});
        if let Some([done, total]) = self.bytes {
            value["completed_bytes"] = done.into();
            value["total_bytes"] = total.into();
        }
        if let Some([done, total]) = self.units {
            value["completed_units"] = done.into();
            value["total_units"] = total.into();
        }
        if let Some(unit) = self.unit {
            value["unit"] = unit.token().into();
        }
        if let Some(subject) = self.subject {
            value["subject"] = subject.token().into();
        }
        if let Some(purpose) = self.purpose {
            value["purpose"] = purpose.token().into();
        }
        value
    }
}
fn pair(record: &Record, done: &str, total: &str, bounded: bool) -> Result<Option<[u64; 2]>, ()> {
    match (record.get(done), record.get(total)) {
        (None, None) => Ok(None),
        (Some(done), Some(total)) => {
            let done = done.as_u64().ok_or(())?;
            let total = total.as_u64().ok_or(())?;
            (total > 0 && (!bounded || done <= total))
                .then_some(Some([done, total]))
                .ok_or(())
        }
        _ => Err(()),
    }
}
#[derive(Clone, Copy, Default)]
pub(in crate::progress::pipeline) struct Tracker {
    pub(in crate::progress::pipeline) media: Option<Media>,
    pub(in crate::progress::pipeline) began: Duration,
    pub(in crate::progress::pipeline) ended: Option<Duration>,
    pub(in crate::progress::pipeline) checking_began: Option<Duration>,
    last_id: u64,
    pub(in crate::progress::pipeline) observations: u64,
    pub(in crate::progress::pipeline) aborted: bool,
}
impl Tracker {
    pub(in crate::progress::pipeline) fn elapsed(self, now: Duration) -> Duration {
        self.ended.unwrap_or(now).saturating_sub(self.began)
    }
    pub(in crate::progress::pipeline) fn observe(&mut self, next: Media, now: Duration) -> bool {
        if self.aborted {
            return false;
        }
        let positive = if let Some(old) = self.media.filter(|old| old.id == next.id) {
            if old.status.terminal()
                || next.status == Status::Started
                || old.task != next.task
                || old.subject != next.subject
                || old.purpose != next.purpose
                || old.unit != next.unit
                || old.estimated != next.estimated
                || !monotone(old.bytes, next.bytes)
                || !monotone(old.units, next.units)
                || next == old
            {
                return false;
            }
            counters_advance(old.bytes, next.bytes) || counters_advance(old.units, next.units)
        } else {
            if next.status != Status::Started
                || next.id <= self.last_id
                || self.media.is_some_and(Media::active)
            {
                return false;
            }
            self.began = now;
            self.ended = None;
            self.checking_began = None;
            false
        };
        self.last_id = next.id;
        self.observations = self.observations.saturating_add(u64::from(positive));
        self.aborted |= next.aborted();
        if next.checking() && self.checking_began.is_none() {
            self.checking_began = Some(now);
        }
        if next.status.terminal() {
            self.ended = Some(now);
        }
        self.media = Some(next);
        true
    }
}
fn monotone(old: Option<[u64; 2]>, new: Option<[u64; 2]>) -> bool {
    match (old, new) {
        (None, None) => true,
        (Some([old, total]), Some([new, new_total])) => total == new_total && new >= old,
        _ => false,
    }
}
fn counters_advance(old: Option<[u64; 2]>, new: Option<[u64; 2]>) -> bool {
    matches!((old,new),(Some([old,_]),Some([new,_])) if new>old)
}
