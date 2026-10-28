//! Started tasks are observable; worker ceilings and CPU utilization are not.
use super::work::media::{Media, Purpose, Subject, Task, Unit};
use drmr_core::events::Record;
use std::fmt::Write as _;

mod items;
pub(in crate::progress) use items::ItemState;
mod descriptions;
pub(in crate::progress) use descriptions::Description;

#[cfg(test)]
mod item_tests;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Preparation,
    Snapshot,
    Normalization,
    SourceComparison,
    Comparison,
}
impl Operation {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "preparation" => Some(Self::Preparation),
            "snapshot" => Some(Self::Snapshot),
            "normalization" => Some(Self::Normalization),
            "source-comparison" => Some(Self::SourceComparison),
            "comparison" => Some(Self::Comparison),
            _ => None,
        }
    }
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
pub(in crate::progress) struct Work {
    id: u64,
    operation: Operation,
    status: Status,
    subject: Subject,
    purpose: Purpose,
    unit: Unit,
    total: u64,
    started: u64,
    active: u64,
    waiting: u64,
    completed: u64,
    stopped: u64,
    items: Option<items::Items>,
    descriptions: Option<descriptions::Descriptions>,
}
impl Work {
    fn parse(record: &Record) -> Option<Self> {
        let text = |key| record.get(key)?.as_str();
        let number = |key| record.get(key)?.as_u64();
        let mut work = Self {
            id: number("batch_id")?,
            operation: Operation::parse(text("operation")?)?,
            status: Status::parse(text("state")?)?,
            subject: Subject::parse(text("subject")?)?,
            purpose: Purpose::parse(text("purpose")?)?,
            unit: Unit::parse(text("unit")?)?,
            total: number("total_units")?,
            started: number("started_units")?,
            active: number("active_units")?,
            waiting: number("waiting_units")?,
            completed: number("completed_units")?,
            stopped: number("stopped_units")?,
            items: None,
            descriptions: None,
        };
        if work.id == 0
            || work.total == 0
            || work.total > 64
            || work.unit == Unit::Files
            || !match work.operation {
                Operation::Preparation => matches!(
                    (work.subject, work.purpose),
                    (
                        Subject::Source,
                        Purpose::Conversion | Purpose::Verification | Purpose::Admission,
                    ) | (Subject::Result, _)
                        | (Subject::Output, Purpose::Conversion)
                ),
                Operation::Comparison => {
                    (work.subject, work.purpose) == (Subject::Result, Purpose::Verification)
                }
                _ => (work.subject, work.purpose) == (Subject::Source, Purpose::Conversion),
            }
            || [
                work.started,
                work.active,
                work.waiting,
                work.completed,
                work.stopped,
            ]
            .into_iter()
            .any(|value| value > work.total)
            || work.started + work.waiting != work.total
            || work.active + work.completed + work.stopped != work.started
            || (work.status == Status::Started && work.started != 0)
            || (work.status == Status::Finished && work.completed != work.total)
            || (work.status.terminal() && work.active != 0)
        {
            return None;
        }
        work.items = items::Items::parse(
            record,
            work.total,
            [work.active, work.waiting, work.completed, work.stopped],
        )?;
        work.descriptions =
            descriptions::Descriptions::parse(record, work.total, work.items.is_some())?;
        Some(work)
    }
    pub(in crate::progress) fn items(&self) -> Option<&[ItemState]> {
        self.items.as_ref().map(|items| items.states(self.total))
    }
    pub(in crate::progress) fn description(&self, ordinal: usize) -> Option<Description> {
        self.descriptions.as_ref()?.get(ordinal, self.total)
    }
    pub(in crate::progress) fn active(self) -> bool {
        !self.status.terminal()
    }
    pub(in crate::progress) fn has_started(self) -> bool {
        self.started > 0
    }
    pub(super) fn matches_media(self, media: Media) -> bool {
        self.active()
            && self.operation == Operation::Preparation
            && media.active()
            && media.task == Task::Preparation
            && media.subject == Some(self.subject)
            && media.purpose == Some(self.purpose)
            && media.unit == Some(self.unit)
            && media.units.is_some_and(|[_, total]| total == self.total)
    }
    pub(in crate::progress) fn label(self, independent: bool) -> &'static str {
        match self.operation {
            Operation::Snapshot => "Copying source tracks",
            Operation::Normalization => "Preparing clear tracks",
            Operation::SourceComparison => "Checking source conversion",
            Operation::Comparison if independent => "Comparing original and result",
            Operation::Comparison => "Comparing converted media",
            Operation::Preparation => match (self.subject, self.purpose) {
                (Subject::Output, _) => "Copying prepared tracks",
                (Subject::Source, Purpose::Verification) => "Reading original media",
                (Subject::Source, Purpose::Admission) => "Checking source media",
                (Subject::Source, _) => "Preparing source media",
                (Subject::Result, Purpose::Conversion) => "Reading converted media",
                (Subject::Result, Purpose::Admission) => "Checking result metadata",
                (Subject::Result, Purpose::Verification) => "Reading result media",
            },
        }
    }
    pub(in crate::progress) fn compact_label(self) -> &'static str {
        match self.operation {
            Operation::Snapshot => "Copy",
            Operation::Normalization | Operation::Preparation => "Prep",
            Operation::SourceComparison | Operation::Comparison => "Check",
        }
    }
    pub(in crate::progress) fn counts(self, compact: bool) -> String {
        let mut value = if compact {
            format!(
                "{} active {} wait {}/{}",
                self.active, self.waiting, self.completed, self.total
            )
        } else {
            format!(
                "{} active, {} waiting, {}/{} done",
                self.active, self.waiting, self.completed, self.total
            )
        };
        if self.stopped > 0 {
            let _ = write!(value, ", {} stopped", self.stopped);
        }
        value
    }
    pub(super) fn json(self) -> serde_json::Value {
        let mut value = serde_json::json!({
            "batch_id":self.id, "operation":self.operation.token(),
            "state":self.status.token(), "subject":self.subject.token(),
            "purpose":self.purpose.token(), "unit":self.unit.token(),
            "total_units":self.total, "started_units":self.started,
            "active_units":self.active, "waiting_units":self.waiting,
            "completed_units":self.completed, "stopped_units":self.stopped,
        });
        if let Some(items) = &self.items {
            value["unit_states"] = items.json(self.total);
        }
        if let Some(descriptions) = &self.descriptions {
            value["unit_descriptions"] = descriptions.json(self.total);
        }
        value
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Tracker {
    work: Option<Work>,
    last_id: u64,
}
impl Tracker {
    pub(super) fn current(self) -> Option<Work> {
        self.work
    }
    pub(super) fn clear(&mut self) {
        self.work = None;
    }
    pub(super) fn observe(&mut self, record: &Record) -> bool {
        let Some(next) = Work::parse(record) else {
            return false;
        };
        if let Some(old) = self.work.filter(|old| old.id == next.id) {
            if old.status.terminal()
                || next.status == Status::Started
                || old.operation != next.operation
                || old.subject != next.subject
                || old.purpose != next.purpose
                || old.unit != next.unit
                || old.total != next.total
                || next.started < old.started
                || next.completed < old.completed
                || next.stopped < old.stopped
                || match (old.items, next.items) {
                    (Some(previous), Some(next)) => !next.follows(previous, old.total),
                    (Some(_), None) => true,
                    _ => false,
                }
                || match (&old.descriptions, &next.descriptions) {
                    (Some(_), None) => true,
                    (previous, Some(next)) => {
                        !next.follows(previous.as_ref(), old.items(), old.total)
                    }
                    _ => false,
                }
                || next == old
            {
                return false;
            }
        } else if next.status != Status::Started
            || next.id <= self.last_id
            || self
                .work
                .is_some_and(|old| old.active() && old.active + old.waiting > 0)
        {
            return false;
        }
        self.last_id = next.id;
        self.work = Some(next);
        true
    }
}
