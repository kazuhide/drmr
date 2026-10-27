//! Operation-local rates retain one latest sample per fixed work class.
use super::{SCALE, Tracker};
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::progress::pipeline) enum Class {
    OutputPreparation,
    Packaging,
    ResultConversion,
    ResultAdmission,
    SourceVerification,
    ResultVerification,
    Comparison,
    Integrity,
    Capture,
}
impl Class {
    const COUNT: usize = 9;
    fn index(self) -> usize {
        self as usize
    }
}
#[derive(Clone, Copy)]
struct Sample {
    duration: Duration,
    observed: Duration,
    tail: Option<Duration>,
}
#[derive(Clone, Copy, Default)]
pub(in crate::progress::pipeline) struct Rates {
    samples: [Option<Sample>; Class::COUNT],
}
impl Rates {
    pub(in crate::progress::pipeline) fn observe(
        &mut self,
        class: Class,
        tracker: Tracker,
        now: Duration,
    ) {
        let Some(media) = tracker.media.filter(|media| !media.aborted()) else {
            return;
        };
        let fraction = media.fraction();
        if fraction == 0 {
            return;
        }
        let Some(nanos) = tracker
            .elapsed(now)
            .as_nanos()
            .checked_mul(u128::from(SCALE))
            .map(|nanos| nanos / u128::from(fraction))
        else {
            return;
        };
        let Some(duration) = bounded(nanos) else {
            return;
        };
        if duration.is_zero() {
            return;
        }
        let at = tracker.ended.unwrap_or(now);
        let tail = tracker.ended.and_then(|end| {
            tracker
                .checking_began
                .map(|began| end.saturating_sub(began))
        });
        self.sample_with_tail(class, duration, at, tail);
    }
    pub(in crate::progress::pipeline) fn sample(
        &mut self,
        class: Class,
        duration: Duration,
        now: Duration,
    ) {
        self.sample_with_tail(class, duration, now, None);
    }
    fn sample_with_tail(
        &mut self,
        class: Class,
        duration: Duration,
        now: Duration,
        tail: Option<Duration>,
    ) {
        if duration.is_zero() || duration > Duration::from_hours(168) {
            return;
        }
        let slot = &mut self.samples[class.index()];
        if slot.is_none_or(|old| now >= old.observed) {
            *slot = Some(Sample {
                duration,
                observed: now,
                tail: tail.or_else(|| slot.and_then(|old| old.tail)),
            });
        }
    }
    pub(in crate::progress::pipeline) fn merge(&mut self, other: Self) {
        for (old, new) in self.samples.iter_mut().zip(other.samples) {
            if let Some(new) = new
                && old.is_none_or(|old| {
                    new.observed > old.observed
                        || new.observed == old.observed && new.duration > old.duration
                })
            {
                *old = Some(Sample {
                    tail: new.tail.or_else(|| old.and_then(|old| old.tail)),
                    ..new
                });
            }
        }
    }
    pub(in crate::progress::pipeline) fn duration(&self, class: Class) -> Option<Duration> {
        self.samples[class.index()].map(|sample| sample.duration)
    }
    pub(in crate::progress::pipeline) fn batch_remaining(
        &self,
        class: Class,
        tracker: Tracker,
        now: Duration,
    ) -> Option<Duration> {
        let media = tracker.media?;
        if media.checking() {
            let sample = self.samples[class.index()]?;
            return sample
                .tail?
                .checked_sub(now.saturating_sub(tracker.checking_began?))
                .filter(|duration| !duration.is_zero());
        }
        self.remaining(class, media.fraction())
    }
    pub(in crate::progress::pipeline) fn remaining(
        &self,
        class: Class,
        fraction: u32,
    ) -> Option<Duration> {
        bounded(
            self.duration(class)?
                .as_nanos()
                .checked_mul(u128::from(SCALE - fraction.min(SCALE)))?
                / u128::from(SCALE),
        )
    }
}
pub(in crate::progress::pipeline) fn bounded(nanos: u128) -> Option<Duration> {
    if nanos > Duration::from_hours(168).as_nanos() {
        return None;
    }
    Some(Duration::from_nanos(u64::try_from(nanos).ok()?))
}
