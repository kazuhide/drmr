//! Bounded independent hash observations may overlap preparation in several children.
use super::work::media::{Task, Tracker};
use super::work::{Class, Media, Rates};
use drmr_core::events::Record;
use std::time::Duration;

#[derive(Default)]
pub(super) struct Integrity {
    active: [Option<Tracker>; 16],
    completed_ids: Vec<u64>,
    completed_batches: u64,
    last_terminal: Option<Tracker>,
    baseline: Option<u64>,
    observed_bytes: u128,
    observed_nanos: u128,
    observed_at: Duration,
    pub(super) observations: u64,
    pub(super) unknown: bool,
    pub(super) aborted: bool,
}
impl Integrity {
    pub(super) fn observe(&mut self, record: &Record, now: Duration) -> bool {
        let Some(media) = Media::parse(record).filter(|media| media.task == Task::Integrity) else {
            return false;
        };
        let existing = self.active.iter().position(|slot| {
            slot.is_some_and(|tracker| tracker.media.is_some_and(|old| old.id == media.id))
        });
        let index = if let Some(index) = existing {
            index
        } else {
            if self.completed_ids.contains(&media.id) {
                return false;
            }
            if self.completed_ids.len() >= 256 {
                self.unknown = true;
                return false;
            }
            let Some(index) = self.active.iter().position(Option::is_none) else {
                self.unknown = true;
                return true;
            };
            index
        };
        let mut tracker = self.active[index].unwrap_or_default();
        let previous = tracker.observations;
        if !tracker.observe(media, now) {
            return false;
        }
        self.baseline
            .get_or_insert(media.bytes.expect("integrity byte budget")[1]);
        self.observations = self
            .observations
            .saturating_add(tracker.observations.saturating_sub(previous));
        self.aborted |= media.aborted();
        self.active[index] = Some(tracker);
        if media.finished() {
            self.remember(media.id);
            self.completed_batches = self.completed_batches.saturating_add(1);
            self.last_terminal = Some(tracker);
            let bytes = u128::from(media.bytes.expect("integrity byte budget")[1]);
            match (
                self.observed_bytes.checked_add(bytes),
                self.observed_nanos
                    .checked_add(tracker.elapsed(now).as_nanos()),
            ) {
                (Some(bytes), Some(nanos)) => {
                    self.observed_bytes = bytes;
                    self.observed_nanos = nanos;
                    self.observed_at = now;
                }
                _ => self.unknown = true,
            }
            self.active[index] = None;
        } else if media.aborted() {
            self.remember(media.id);
            self.last_terminal = Some(tracker);
            self.active[index] = None;
        }
        true
    }
    fn remember(&mut self, id: u64) {
        if self.completed_ids.len() < 256 {
            self.completed_ids.push(id);
        } else {
            self.unknown = true;
        }
    }
    pub(super) fn json(&self, now: Duration) -> Option<serde_json::Value> {
        self.baseline?;
        let snapshot = |tracker: Tracker| {
            tracker.media.map(|media| {
                let mut value = media.json();
                value["elapsed_ms"] = super::millis(tracker.elapsed(now)).into();
                value
            })
        };
        let active: Vec<_> = self
            .active
            .iter()
            .filter_map(|slot| slot.and_then(snapshot))
            .collect();
        Some(serde_json::json!({
            "active":active,"last_terminal":self.last_terminal.and_then(snapshot),
            "completed_batches":self.completed_batches,"reference_bytes":self.baseline,
            "observed_bytes":u64::try_from(self.observed_bytes).ok(),
            "observed_batch_ms":u64::try_from(self.observed_nanos/1_000_000).ok(),
            "calibrated":self.rates().duration(Class::Integrity).is_some(),
            "overflow":self.unknown,"aborted":self.aborted,
        }))
    }
    pub(super) fn rates(&self) -> Rates {
        let mut rates = Rates::default();
        if self.unknown
            || self.aborted
            || self.completed_batches < 2
            || self.observed_nanos == 0
            || self.observed_bytes == 0
        {
            return rates;
        }
        let duration = self
            .observed_nanos
            .checked_mul(u128::from(self.baseline.unwrap_or(0)))
            .and_then(|nanos| super::work::rates::bounded(nanos / self.observed_bytes));
        if let Some(duration) = duration {
            rates.sample(Class::Integrity, duration, self.observed_at);
        }
        rates
    }
}
