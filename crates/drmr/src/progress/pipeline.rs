//! Bounded logical progress, independent of interleaved internal activity scopes.
use super::{Analysis, TASKS, display::Units};
use drmr_core::events::Record;
use std::time::Duration;

mod integrity;
mod work;
pub(super) use work::{Estimate, Media, Task};

const MAX_OUTPUTS: usize = 8;
const ACTIVITIES: [&str; 6] = [
    "input-preparation",
    "inspection",
    "normalization",
    "packaging",
    "verification",
    "publication",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Source,
    Outputs,
    FinalCheck,
    Publication,
}
impl Phase {
    pub(super) const ALL: [Self; 4] = [
        Self::Source,
        Self::Outputs,
        Self::FinalCheck,
        Self::Publication,
    ];
    fn parse(value: &str) -> Option<Self> {
        match value {
            "source" => Some(Self::Source),
            "outputs" => Some(Self::Outputs),
            "final-check" => Some(Self::FinalCheck),
            "publication" => Some(Self::Publication),
            _ => None,
        }
    }
    pub(super) fn index(self) -> usize {
        match self {
            Self::Source => 0,
            Self::Outputs => 1,
            Self::FinalCheck => 2,
            Self::Publication => 3,
        }
    }
    pub(super) fn token(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Outputs => "outputs",
            Self::FinalCheck => "final-check",
            Self::Publication => "publication",
        }
    }
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Source => "Checking source",
            Self::Outputs => "Creating outputs",
            Self::FinalCheck => "Checking final result",
            Self::Publication => "Saving result",
        }
    }
    pub(super) fn completed(self) -> &'static str {
        match self {
            Self::Source => "Source checked",
            Self::Outputs => "Outputs checked",
            Self::FinalCheck => "Final result checked",
            Self::Publication => "Result saved",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Status {
    #[default]
    Pending,
    Running,
    Finished,
    Aborted,
}
impl Status {
    pub(super) fn token(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Finished => "finished",
            Self::Aborted => "aborted",
        }
    }
    pub(super) fn terminal(self) -> bool {
        matches!(self, Self::Finished | Self::Aborted)
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Lifetime {
    pub(super) status: Status,
    pub(super) began: Option<Duration>,
    pub(super) ended: Option<Duration>,
}
impl Lifetime {
    fn update(&mut self, state: &str, now: Duration) -> bool {
        match (state, self.status) {
            ("started", Status::Pending) => {
                self.status = Status::Running;
                self.began = Some(now);
            }
            ("finished", Status::Running) => {
                self.status = Status::Finished;
                self.ended = Some(now);
            }
            ("aborted", Status::Running) => {
                self.status = Status::Aborted;
                self.ended = Some(now);
            }
            _ => return false,
        }
        true
    }
    pub(super) fn elapsed(self, now: Duration) -> Option<Duration> {
        self.began
            .map(|began| self.ended.unwrap_or(now).saturating_sub(began))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Format {
    Mp4,
    Dash,
    Hls,
    HlsTs,
}
impl Format {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "mp4" => Some(Self::Mp4),
            "dash" => Some(Self::Dash),
            "hls" => Some(Self::Hls),
            "hls-ts" => Some(Self::HlsTs),
            _ => None,
        }
    }
    fn token(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Dash => "dash",
            Self::Hls => "hls",
            Self::HlsTs => "hls-ts",
        }
    }
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4",
            Self::Dash => "DASH fMP4",
            Self::Hls => "HLS fMP4",
            Self::HlsTs => "HLS TS",
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(super) struct Output {
    pub(super) format: Option<Format>,
    pub(super) lifetime: Lifetime,
    activities: [u64; 6],
    tasks: [u64; 4],
    analysis: Option<Analysis>,
    work: work::Work,
}
impl Output {
    fn activity(&self) -> Option<&'static str> {
        if self.lifetime.status != Status::Running {
            return None;
        }
        if self.activities[4] > 0 || self.tasks.iter().any(|count| *count > 0) {
            Some("verification")
        } else if self.activities[5] > 0 {
            Some("publication")
        } else if self.activities[2] > 0 || self.activities[3] > 0 {
            Some("packaging")
        } else if self.analysis.is_some() || self.activities[0] > 0 {
            Some("input-preparation")
        } else if self.activities[1] > 0 {
            Some("inspection")
        } else {
            None
        }
    }
    pub(super) fn detail(&self) -> &'static str {
        match self.lifetime.status {
            Status::Pending => "Waiting",
            Status::Finished => "Checked",
            Status::Aborted => "Stopped",
            Status::Running => self.work.detail().unwrap_or_else(|| match self.activity() {
                Some("verification") => "Checking output",
                Some("publication") => "Finalizing output",
                Some("packaging") => "Converting media",
                Some("input-preparation") => "Preparing media",
                Some("inspection") => "Reading metadata",
                _ => "Working",
            }),
        }
    }
    pub(super) fn units(self) -> Option<Units> {
        self.analysis
            .filter(|a| self.lifetime.status == Status::Running && a.state != "aborted")
            .map(|a| Units {
                completed: a.completed,
                total: a.total,
                initial: 0,
                began: a.began,
                segments: true,
                checking: a.completed == a.total && a.state != "finished",
            })
    }
    pub(super) fn progress_label(&self) -> &'static str {
        match self.lifetime.status {
            Status::Pending => return "Waiting",
            Status::Finished => return "Checked",
            Status::Aborted => return "Stopped",
            Status::Running => {}
        }
        if let Some(media) = self.media().filter(|media| media.active()) {
            return match media.task {
                Task::Preparation => "Preparing media",
                Task::Packaging => "Writing media",
                Task::Comparison => "Comparing media",
                Task::Integrity => "Checking files",
            };
        }
        if self.tasks[3] > 0 {
            return "Checking manifests";
        }
        match self.activity() {
            Some("input-preparation" | "normalization") => "Preparing media",
            Some("inspection") => "Reading metadata",
            Some("verification") => "Checking media",
            Some("publication") => "Saving result",
            _ => match self.work.step {
                Some(work::Step::Verification) => "Checking media",
                Some(work::Step::Finalization) => "Checking result",
                _ => "Converting media",
            },
        }
    }
    pub(super) fn media(self) -> Option<Media> {
        self.work.media
    }
    pub(super) fn step_remaining(self, now: Duration) -> Option<Duration> {
        self.work.step_remaining(now)
    }
    pub(super) fn estimated_percent(self) -> u32 {
        if self.lifetime.status == Status::Finished {
            100
        } else {
            self.work.completed() * 100 / work::OUTPUT
        }
    }
}

struct Context {
    index: usize,
    total: usize,
    format: Format,
}
impl Context {
    fn parse(record: &Record) -> Option<Self> {
        let total = usize::try_from(record.get("output_total")?.as_u64()?).ok()?;
        let index = usize::try_from(record.get("output_index")?.as_u64()?).ok()?;
        let format = Format::parse(record.get("output_format")?.as_str()?)?;
        (total > 0 && total <= MAX_OUTPUTS && index < total).then_some(Self {
            index,
            total,
            format,
        })
    }
}

pub(super) fn has_context(record: &Record) -> bool {
    ["output_index", "output_total", "output_format"]
        .iter()
        .any(|key| record.contains_key(*key))
}

#[derive(Default)]
pub(super) struct State {
    pub(super) phase: Option<Phase>,
    phases: [Lifetime; 4],
    outputs: [Output; MAX_OUTPUTS],
    total: Option<usize>,
    plan: Option<work::Plan>,
    failed: bool,
    source_work: work::Source,
    integrity: integrity::Integrity,
}
impl State {
    pub(super) fn outputs(&self) -> &[Output] {
        &self.outputs[..self.total.unwrap_or(0)]
    }
    pub(super) fn lifetime(&self, phase: Phase) -> Lifetime {
        self.phases[phase.index()]
    }
    pub(super) fn active_outputs(&self) -> usize {
        self.outputs()
            .iter()
            .filter(|output| output.lifetime.status == Status::Running)
            .count()
    }
    pub(super) fn checked_outputs(&self) -> usize {
        self.outputs()
            .iter()
            .filter(|output| output.lifetime.status == Status::Finished)
            .count()
    }

    pub(super) fn estimates_enabled(&self) -> bool {
        self.plan.is_some_and(|plan| plan.enabled)
    }

    pub(super) fn stop_estimates(&mut self) {
        self.failed = true;
    }

    pub(super) fn source_media(&self) -> Option<Media> {
        self.source_work.media()
    }

    pub(super) fn observe_media(&mut self, record: &Record, now: Duration) -> bool {
        if record.get("task").and_then(serde_json::Value::as_str) == Some("integrity") {
            return self.integrity.observe(record, now);
        }
        self.lifetime(Phase::Source).status == Status::Running
            && self.source_work.observe(record, now)
    }

    pub(super) fn estimate(&self, now: Duration) -> Option<Estimate> {
        let plan = self.plan.filter(|plan| plan.enabled && !self.failed)?;
        let source = self.lifetime(Phase::Source);
        let source_work = if source.status == Status::Finished {
            work::SOURCE
        } else {
            (plan.source_completed + 2 * self.source_work.fraction()).min(work::SOURCE - 1)
        };
        let output_work = self
            .outputs()
            .iter()
            .map(|output| {
                if output.lifetime.status == Status::Finished {
                    work::OUTPUT
                } else {
                    output.work.completed()
                }
            })
            .sum::<u32>();
        let aborted = self.source_work.aborted()
            || self.integrity.aborted
            || self
                .phases
                .iter()
                .any(|life| life.status == Status::Aborted)
            || self
                .outputs()
                .iter()
                .any(|output| output.lifetime.status == Status::Aborted || output.work.aborted);
        let committed = !aborted
            && source.status == Status::Finished
            && self.outputs().len() == plan.outputs
            && self.checked_outputs() == plan.outputs
            && self.lifetime(Phase::Outputs).status == Status::Finished
            && self.lifetime(Phase::FinalCheck).status == Status::Finished
            && self.lifetime(Phase::Publication).status == Status::Finished;
        let observations = self.outputs().iter().fold(
            self.source_work
                .observations()
                .saturating_add(self.integrity.observations),
            |count, output| count.saturating_add(output.work.observations()),
        );
        let remaining = if committed {
            Some(Duration::ZERO)
        } else if !aborted
            && !self.integrity.unknown
            && observations >= 2
            && source.elapsed(now).is_some_and(|elapsed| {
                elapsed >= Duration::from_secs(1) && elapsed <= Duration::from_hours(168)
            })
        {
            self.remaining(plan, now)
        } else {
            None
        };
        Some(Estimate {
            percent: if committed {
                100
            } else {
                ((source_work + output_work) * 100 / plan.total()).min(99)
            },
            remaining,
        })
    }

    fn remaining(&self, plan: work::Plan, now: Duration) -> Option<Duration> {
        if self.lifetime(Phase::Source).status != Status::Finished
            || self.outputs().len() != plan.outputs
            || self
                .outputs()
                .iter()
                .any(|output| output.lifetime.status == Status::Pending)
        {
            return None;
        }
        let mut rates = self.integrity.rates();
        for output in self.outputs() {
            rates.merge(output.work.rates(now));
        }
        let mut longest = Duration::ZERO;
        for output in self
            .outputs()
            .iter()
            .filter(|output| output.lifetime.status == Status::Running)
        {
            longest = longest.max(output.work.remaining(&rates, now)?);
        }
        let passes = u32::try_from(plan.outputs + 1).ok()?;
        let mut guards = rates
            .duration(work::Class::Integrity)?
            .checked_mul(passes)?;
        if let Some(began) = self.lifetime(Phase::FinalCheck).began {
            guards = guards.checked_sub(now.saturating_sub(began))?;
        }
        work::rates::bounded(longest.checked_add(guards)?.as_nanos())
    }

    pub(super) fn observe(&mut self, record: &Record, now: Duration) -> bool {
        let (Some(scope), Some(state)) = (
            record.get("scope").and_then(serde_json::Value::as_str),
            record.get("state").and_then(serde_json::Value::as_str),
        ) else {
            return false;
        };
        if scope == "work-plan" {
            let Some(plan) = work::Plan::parse(record) else {
                return false;
            };
            if state != "ready"
                || self.phase != Some(Phase::Source)
                || self.lifetime(Phase::Source).status != Status::Running
                || self.plan.is_some()
                || self.total.is_some_and(|total| total != plan.outputs)
            {
                return false;
            }
            self.plan = Some(plan);
            return true;
        }
        if scope == "output-step" {
            if state != "started" || !self.valid_context(record) {
                return false;
            }
            let Some(step) = record.get("step").and_then(serde_json::Value::as_str) else {
                return false;
            };
            let context = Context::parse(record).expect("validated output context");
            return self.outputs[context.index].work.step(step, now);
        }
        if scope == "output" {
            let Some(context) = Context::parse(record) else {
                return false;
            };
            if self.total.is_some_and(|total| total != context.total)
                || self.plan.is_some_and(|plan| plan.outputs != context.total)
                || self.phase != Some(Phase::Outputs)
            {
                return false;
            }
            let output = &mut self.outputs[context.index];
            if output.format.is_some_and(|format| format != context.format)
                || !output.lifetime.update(state, now)
            {
                return false;
            }
            self.total = Some(context.total);
            output.format = Some(context.format);
            if output.lifetime.status.terminal() {
                output.activities = [0; 6];
                output.tasks = [0; 4];
                output.analysis = None;
            }
            return true;
        }
        let Some(phase) = Phase::parse(scope) else {
            return false;
        };
        if state == "started"
            && self
                .phase
                .is_some_and(|current| phase.index() < current.index())
        {
            return false;
        }
        if !self.phases[phase.index()].update(state, now) {
            return false;
        }
        if self
            .phase
            .is_none_or(|current| phase.index() >= current.index())
        {
            self.phase = Some(phase);
        }
        true
    }

    pub(super) fn valid_context(&self, record: &Record) -> bool {
        Context::parse(record).is_some_and(|context| {
            self.total == Some(context.total)
                && self.outputs[context.index].format == Some(context.format)
                && self.outputs[context.index].lifetime.status == Status::Running
        })
    }

    pub(super) fn observe_nested(&mut self, record: &Record, now: Duration) -> bool {
        if !self.valid_context(record) {
            return !has_context(record)
                && record.get("stage").and_then(serde_json::Value::as_str)
                    == Some("media-progress")
                && record.get("event").and_then(serde_json::Value::as_str) == Some("progress")
                && self.observe_media(record, now);
        }
        let context = Context::parse(record).expect("validated context");
        if record.get("stage").and_then(serde_json::Value::as_str) == Some("media-progress")
            && record.get("task").and_then(serde_json::Value::as_str) == Some("integrity")
        {
            return self.integrity.observe(record, now);
        }
        let output = &mut self.outputs[context.index];
        let text = |key| record.get(key).and_then(serde_json::Value::as_str);
        match (text("stage"), text("event")) {
            (Some("conversion"), Some("activity")) => {
                let Some(index) = text("activity")
                    .and_then(|name| ACTIVITIES.iter().position(|value| *value == name))
                else {
                    return false;
                };
                if text("state") == Some("started") && index != 0 {
                    output.analysis = None;
                }
                observe_count(&mut output.activities[index], text("state"))
            }
            (Some("conversion"), Some("task")) => {
                let Some(index) = text("task")
                    .and_then(|name| TASKS.iter().position(|(value, _)| *value == name))
                else {
                    return false;
                };
                observe_count(&mut output.tasks[index], text("state"))
            }
            (Some("analysis"), Some("progress")) => {
                let Some((analysis, _)) = super::parse_analysis(output.analysis, record, now)
                else {
                    return false;
                };
                output.analysis = Some(analysis);
                true
            }
            (Some("media-progress"), Some("progress")) => output.work.observe(record, now),
            _ => false,
        }
    }

    pub(super) fn clear_finished_analysis(&mut self) -> bool {
        let mut changed = false;
        for output in &mut self.outputs {
            if output
                .analysis
                .is_some_and(|analysis| analysis.state == "finished")
            {
                output.analysis = None;
                changed = true;
            }
        }
        changed
    }

    pub(super) fn json(&self, now: Duration) -> serde_json::Value {
        let phases: Vec<_> = Phase::ALL.into_iter().filter_map(|phase| {
            let life = self.lifetime(phase);
            (life.status != Status::Pending).then(|| serde_json::json!({"scope":phase.token(),"state":life.status.token(),"elapsed_ms":life.elapsed(now).map(millis)}))
        }).collect();
        let outputs: Vec<_> = self.outputs().iter().enumerate().map(|(index, output)| {
            let mut value = serde_json::json!({"index":index,"format":output.format.map(Format::token),"state":output.lifetime.status.token(),"activity":output.activity(),"elapsed_ms":output.lifetime.elapsed(now).map(millis)});
            if let Some(units) = output.units() {
                value["analysis"] = serde_json::json!({"task":"ts-preparation","completed":units.completed,"total":units.total,"checking":units.checking});
            }
            if let Some(step) = output.work.step {
                value["step"] = step.token().into();
            }
            if let Some(media) = output.media() {
                value["media"] = media.json();
            }
            if self.estimates_enabled() {
                value["estimated_work_percent"] = output.estimated_percent().into();
                if let Some(remaining)=output.step_remaining(now) {value["estimated_task_remaining_ms"]=millis(remaining).into();}
            }
            value
        }).collect();
        let mut value = serde_json::json!({"phase":self.phase.map(Phase::token),"phases":phases,"outputs":outputs,"active_outputs":self.active_outputs(),"checked_outputs":self.checked_outputs()});
        if let Some(sensor) = self.integrity.json(now) {
            value["integrity_batches"] = sensor;
        }
        if let Some(media) = self.source_media() {
            value["source_media"] = media.json();
        }
        if let Some(estimate) = self.estimate(now) {
            value["estimate"] = estimate.json();
        }
        value
    }
}

fn observe_count(count: &mut u64, state: Option<&str>) -> bool {
    match state {
        Some("started") => *count = count.saturating_add(1),
        Some("closed") if *count > 0 => *count -= 1,
        _ => return false,
    }
    true
}
fn millis(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod estimate_tests;

#[cfg(test)]
mod rate_tests;

#[cfg(test)]
mod tests {
    use super::*;
    fn record(value: serde_json::Value) -> Record {
        let serde_json::Value::Object(record) = value else {
            panic!("expected a pipeline event object");
        };
        record
    }
    fn began() -> State {
        let mut state = State::default();
        assert!(state.observe(
            &record(serde_json::json!({"scope":"outputs","state":"started"})),
            Duration::ZERO
        ));
        for (index, format) in [(0, "hls"), (1, "dash")] {
            assert!(state.observe(&output(index, format, "started"), Duration::ZERO));
        }
        state
    }
    fn output(index: usize, format: &str, state: &str) -> Record {
        record(
            serde_json::json!({"scope":"output","state":state,"output_index":index,"output_total":2,"output_format":format}),
        )
    }
    fn nested(index: usize, format: &str, activity: &str, state: &str) -> Record {
        record(
            serde_json::json!({"stage":"conversion","event":"activity","activity":activity,"state":state,"output_index":index,"output_total":2,"output_format":format}),
        )
    }

    #[test]
    fn interleaved_scope_closures_and_private_publication_do_not_complete_outputs() {
        let mut state = began();
        for (index, format, activity) in [(0, "hls", "inspection"), (1, "dash", "verification")] {
            assert!(state.observe_nested(
                &nested(index, format, activity, "started"),
                Duration::from_secs(1)
            ));
        }
        assert_eq!(state.outputs()[0].detail(), "Reading metadata");
        assert_eq!(state.outputs()[1].detail(), "Checking output");
        assert!(state.observe_nested(
            &nested(0, "hls", "inspection", "closed"),
            Duration::from_secs(2)
        ));
        assert_eq!(state.outputs()[1].detail(), "Checking output");
        assert_eq!(state.active_outputs(), 2);
        assert_eq!(state.checked_outputs(), 0);
        assert!(state.observe_nested(
            &nested(0, "hls", "publication", "started"),
            Duration::from_secs(3)
        ));
        assert_eq!(state.phase, Some(Phase::Outputs));
        assert_eq!(state.outputs()[0].detail(), "Finalizing output");
        assert!(state.observe(&output(1, "dash", "finished"), Duration::from_secs(4)));
        assert_eq!(state.active_outputs(), 1);
        assert_eq!(state.checked_outputs(), 1);
        assert_eq!(state.phase, Some(Phase::Outputs));
        assert!(!state.observe(&output(1, "dash", "finished"), Duration::from_secs(5)));
        assert!(state.observe(&output(0, "hls", "aborted"), Duration::from_secs(6)));
        assert_eq!(state.checked_outputs(), 1);
        assert_eq!(state.outputs()[0].detail(), "Stopped");
    }

    #[test]
    fn output_context_is_bounded_and_cannot_change_identity() {
        let mut state = State::default();
        assert!(state.observe(
            &record(serde_json::json!({"scope":"outputs","state":"started"})),
            Duration::ZERO
        ));
        for invalid in [
            serde_json::json!({"scope":"output","state":"started","output_index":8,"output_total":9,"output_format":"hls"}),
            serde_json::json!({"scope":"output","state":"started","output_index":0,"output_total":0,"output_format":"hls"}),
            serde_json::json!({"scope":"output","state":"started","output_index":0,"output_total":2,"output_format":"private-sentinel"}),
        ] {
            assert!(!state.observe(&record(invalid), Duration::ZERO));
            assert!(state.outputs().is_empty());
        }
        assert!(!state.observe(&output(0, "hls", "finished"), Duration::ZERO));
        assert!(state.observe(&output(0, "hls", "started"), Duration::ZERO));
        assert!(!state.observe(&output(0, "dash", "finished"), Duration::ZERO));
        let mut changed = output(1, "dash", "started");
        changed.insert("output_total".into(), 3.into());
        assert!(!state.observe(&changed, Duration::ZERO));
        assert_eq!(state.outputs().len(), 2);
        assert_eq!(state.outputs()[1].lifetime.status, Status::Pending);
        assert!(!state.json(Duration::ZERO).to_string().contains("sentinel"));
    }

    #[test]
    fn known_analysis_units_belong_only_to_their_output() {
        let mut state = began();
        let analysis = |index, format, state, done, total| {
            record(
                serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":state,"completed":done,"total":total,"output_index":index,"output_total":2,"output_format":format}),
            )
        };
        for (index, format, total) in [(0, "hls", 5), (1, "dash", 7)] {
            assert!(state.observe_nested(
                &analysis(index, format, "started", 0, total),
                Duration::from_secs(index + 1)
            ));
        }
        assert!(state.observe_nested(&analysis(0, "hls", "running", 2, 5), Duration::from_secs(3)));
        assert!(state.observe_nested(
            &analysis(1, "dash", "running", 4, 7),
            Duration::from_secs(4)
        ));
        let first = state.outputs()[0].units().unwrap();
        let second = state.outputs()[1].units().unwrap();
        assert_eq!(
            (first.completed, first.total, first.began),
            (2, 5, Duration::from_secs(1))
        );
        assert_eq!(
            (second.completed, second.total, second.began),
            (4, 7, Duration::from_secs(2))
        );
        assert!(
            !state.observe_nested(&analysis(0, "hls", "running", 1, 5), Duration::from_secs(5))
        );
        assert!(state.observe_nested(
            &analysis(0, "hls", "finished", 5, 5),
            Duration::from_secs(6)
        ));
        assert!(state.clear_finished_analysis());
        assert!(state.outputs()[0].units().is_none());
        assert!(state.outputs()[1].units().is_some());
    }

    #[test]
    fn final_check_can_finish_after_publication_starts_without_rewinding_the_phase() {
        let mut state = State::default();
        let phase = |scope, status| record(serde_json::json!({"scope":scope,"state":status}));
        assert!(state.observe(&phase("final-check", "started"), Duration::from_secs(1)));
        assert!(state.observe(&phase("publication", "started"), Duration::from_secs(2)));
        assert_eq!(state.phase, Some(Phase::Publication));
        assert!(state.observe(&phase("final-check", "finished"), Duration::from_secs(3)));
        assert_eq!(state.phase, Some(Phase::Publication));
        assert_eq!(state.lifetime(Phase::FinalCheck).status, Status::Finished);
        assert_eq!(state.lifetime(Phase::Publication).status, Status::Running);
        assert!(!state.observe(&phase("source", "started"), Duration::from_secs(4)));
        assert!(!state.observe(&phase("final-check", "finished"), Duration::from_secs(4)));
        assert!(state.observe(&phase("publication", "finished"), Duration::from_secs(5)));
        assert_eq!(state.phase, Some(Phase::Publication));
        assert_eq!(state.lifetime(Phase::Publication).status, Status::Finished);
    }
}
