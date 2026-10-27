//! Provisional work observations never establish media validity or publication.
use drmr_core::events::Record;
pub(super) mod media;
pub(super) mod rates;
pub(in crate::progress) use media::{Media, Task};
use media::{Purpose, Subject, Tracker};
pub(super) use rates::{Class, Rates};
use std::time::Duration;

pub(super) const SCALE: u32 = 1000;
pub(super) const SOURCE: u32 = 4 * SCALE;
pub(super) const OUTPUT: u32 = 16 * SCALE;

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

// Every entry corresponds to an actual ordered call on the qualified shared route.
// Preparation counts describe tracks/renditions, never fabricated media bytes.
const CONVERSION: [(Class, u32); 6] = [
    (Class::OutputPreparation, 1),
    (Class::Packaging, 1),
    (Class::ResultConversion, 1),
    (Class::Comparison, 2),
    (Class::ResultConversion, 1),
    (Class::Comparison, 2),
];
const VERIFICATION: [(Class, u32); 4] = [
    (Class::ResultAdmission, 1),
    (Class::SourceVerification, 1),
    (Class::ResultVerification, 1),
    (Class::Comparison, 2),
];

#[derive(Clone, Copy)]
pub(super) struct Work {
    pub(super) step: Option<Step>,
    pub(super) media: Option<Media>,
    pub(super) aborted: bool,
    tracker: Tracker,
    rates: Rates,
    began: Duration,
    cursor: usize,
    completed: u32,
    class: Option<Class>,
    covered: bool,
    captured: bool,
}
impl Default for Work {
    fn default() -> Self {
        Self {
            step: None,
            media: None,
            aborted: false,
            tracker: Tracker::default(),
            rates: Rates::default(),
            began: Duration::ZERO,
            cursor: 0,
            completed: 0,
            class: None,
            covered: true,
            captured: false,
        }
    }
}
impl Work {
    pub(super) fn step(&mut self, value: &str, now: Duration) -> bool {
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
        if self.step.is_some() && self.cursor != self.sequence().len() {
            self.covered = false;
        }
        self.step = Some(step);
        self.media = None;
        self.began = now;
        self.cursor = 0;
        self.class = None;
        self.captured = false;
        self.completed = self.completed.max(self.base());
        true
    }
    fn sequence(self) -> &'static [(Class, u32)] {
        match self.step {
            Some(Step::Conversion) => &CONVERSION,
            Some(Step::Verification) => &VERIFICATION,
            _ => &[],
        }
    }
    fn base(self) -> u32 {
        match self.step {
            Some(Step::Conversion) | None => 0,
            Some(Step::Verification) => 8 * SCALE,
            Some(Step::Finalization) => 14 * SCALE,
        }
    }
    fn class(media: Media) -> Option<Class> {
        match (media.task, media.subject, media.purpose) {
            (Task::Packaging, _, _) => Some(Class::Packaging),
            (Task::Comparison, _, _) => Some(Class::Comparison),
            (Task::Preparation, Some(Subject::Output), Some(Purpose::Conversion)) => {
                Some(Class::OutputPreparation)
            }
            (Task::Preparation, Some(Subject::Result), Some(Purpose::Conversion)) => {
                Some(Class::ResultConversion)
            }
            (Task::Preparation, Some(Subject::Result), Some(Purpose::Admission)) => {
                Some(Class::ResultAdmission)
            }
            (Task::Preparation, Some(Subject::Source), Some(Purpose::Verification)) => {
                Some(Class::SourceVerification)
            }
            (Task::Preparation, Some(Subject::Result), Some(Purpose::Verification)) => {
                Some(Class::ResultVerification)
            }
            _ => None,
        }
    }
    pub(super) fn observe(&mut self, record: &Record, now: Duration) -> bool {
        let Some(next) = Media::parse(record) else {
            return false;
        };
        if self.aborted {
            return false;
        }
        if next.task == Task::Integrity {
            return false;
        }
        let new = self.tracker.media.is_none_or(|old| old.id != next.id);
        if !self.tracker.observe(next, now) {
            return false;
        }
        self.aborted |= next.aborted();
        self.media = Some(next);
        if new {
            self.class = Self::class(next);
            if self.step == Some(Step::Verification) && !self.captured {
                self.rates
                    .sample(Class::Capture, now.saturating_sub(self.began), now);
                self.captured = true;
            }
            // An unexpected producer route remains observable, but does not acquire an ETA.
            let expected = self.sequence().get(self.cursor).map(|(class, _)| *class);
            if self.class != expected {
                self.covered = false;
            }
            if let Some(class) = self.class
                && let Some(relative) = self.sequence()[self.cursor..]
                    .iter()
                    .position(|(expected, _)| *expected == class)
            {
                self.cursor += relative;
            }
        }
        if let Some(class) = self.class
            && next.finished()
        {
            self.rates.observe(class, self.tracker, now);
        }
        let sequence = self.sequence();
        let prefix = sequence
            .iter()
            .take(self.cursor)
            .map(|(_, weight)| *weight * SCALE)
            .sum::<u32>();
        let current = sequence
            .get(self.cursor)
            .filter(|(class, _)| Some(*class) == self.class)
            .map_or(0, |(_, weight)| *weight * next.fraction());
        self.completed = self
            .completed
            .max(self.base() + u32::from(self.captured) * SCALE + prefix + current);
        if next.finished()
            && sequence
                .get(self.cursor)
                .is_some_and(|(class, _)| Some(*class) == self.class)
        {
            self.cursor += 1;
        }
        true
    }
    pub(super) fn rates(self, now: Duration) -> Rates {
        let mut rates = self.rates;
        if self
            .tracker
            .media
            .is_some_and(|media| media.active() && !media.checking())
            && let Some(class) = self.class
        {
            rates.observe(class, self.tracker, now);
        }
        rates
    }
    pub(super) fn observations(self) -> u64 {
        self.tracker.observations
    }
    pub(in crate::progress) fn step_remaining(self, now: Duration) -> Option<Duration> {
        if !self.media.is_some_and(Media::active)
            || self.tracker.observations < 2
            || self.tracker.elapsed(now) < Duration::from_secs(1)
            || self.tracker.elapsed(now) > Duration::from_hours(168)
        {
            return None;
        }
        let class = self.class?;
        self.rates(now).batch_remaining(class, self.tracker, now)
    }
    pub(super) fn remaining(self, rates: &Rates, now: Duration) -> Option<Duration> {
        if !self.covered || self.aborted || self.step.is_none() {
            return None;
        }
        // bundle_signaling verifies declarations against a fresh output capture before
        // the final integrity check. Native record parsing remains approximate overhead.
        let finalization = rates
            .duration(Class::Capture)?
            .checked_add(rates.duration(Class::Integrity)?)?;
        if self.step == Some(Step::Finalization) {
            return finalization
                .checked_sub(now.saturating_sub(self.began))
                .filter(|duration| !duration.is_zero());
        }
        let mut remaining = Duration::ZERO;
        if self.step == Some(Step::Verification) && !self.captured {
            remaining = rates
                .duration(Class::Capture)?
                .checked_sub(now.saturating_sub(self.began))?;
        }
        for (index, (class, _)) in self.sequence().iter().enumerate().skip(self.cursor) {
            let duration = if index == self.cursor && self.media.is_some_and(Media::active) {
                rates.batch_remaining(*class, self.tracker, now)?
            } else {
                rates.duration(*class)?
            };
            remaining = remaining.checked_add(duration)?;
        }
        if self.step == Some(Step::Conversion) {
            remaining = remaining.checked_add(rates.duration(Class::Capture)?)?;
            for (class, _) in VERIFICATION {
                remaining = remaining.checked_add(rates.duration(class)?)?;
            }
        }
        remaining.checked_add(finalization)
    }
    pub(super) fn completed(self) -> u32 {
        self.completed.min(OUTPUT - 1)
    }
    pub(super) fn detail(self) -> Option<&'static str> {
        if self.aborted {
            return Some("Stopping output");
        }
        if let Some(media) = self.media {
            if media.active() {
                return Some(match media.task {
                    Task::Packaging => "Writing media",
                    Task::Comparison => "Comparing media",
                    Task::Preparation if media.checking() => "Checking prepared track data",
                    Task::Preparation => match media.subject {
                        Some(Subject::Output) => "Preparing output tracks",
                        Some(Subject::Source) => "Preparing source media",
                        _ => "Preparing result media",
                    },
                    Task::Integrity => "Checking file integrity",
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

#[derive(Clone, Copy, Default)]
pub(super) struct Source {
    preparation: Tracker,
}
impl Source {
    pub(super) fn observe(&mut self, record: &Record, now: Duration) -> bool {
        let Some(media) = Media::parse(record) else {
            return false;
        };
        if (media.task, media.subject, media.purpose)
            != (
                Task::Preparation,
                Some(Subject::Source),
                Some(Purpose::Conversion),
            )
        {
            return false;
        }
        self.preparation.observe(media, now)
    }
    pub(super) fn media(self) -> Option<Media> {
        self.preparation.media
    }
    pub(super) fn fraction(self) -> u32 {
        self.preparation.media.map_or(0, Media::fraction)
    }
    pub(super) fn aborted(self) -> bool {
        self.preparation.aborted
    }
    pub(super) fn observations(self) -> u64 {
        self.preparation.observations
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
        serde_json::json!({"estimated":true,"basis":"logical-media-passes","eta_basis":"observed-step-rates","percent":self.percent,"remaining_ms":self.remaining.map(super::millis)})
    }
}
