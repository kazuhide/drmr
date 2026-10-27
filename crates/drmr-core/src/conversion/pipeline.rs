//! Fixed, bounded pipeline milestones supplement nested activity lifetimes.
use super::Converter;
use crate::events::{Record, Sink};
use crate::inspection::OutputFormat;
use std::sync::Arc;

/// Semantic command stages. Nested scopes never imply that a stage succeeded.
#[derive(Clone, Copy)]
pub(super) enum Phase {
    Source,
    Outputs,
    FinalCheck,
    Publication,
}
impl Phase {
    const fn token(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Outputs => "outputs",
            Self::FinalCheck => "final-check",
            Self::Publication => "publication",
        }
    }
}

/// Explicit output stages distinguish repeated internal comparison batches.
#[derive(Clone, Copy)]
pub(super) enum Step {
    Conversion,
    Verification,
    Finalization,
}
impl Step {
    const fn token(self) -> &'static str {
        match self {
            Self::Conversion => "conversion",
            Self::Verification => "verification",
            Self::Finalization => "finalization",
        }
    }
}

#[derive(Clone, Copy)]
struct Output {
    index: usize,
    total: usize,
    format: OutputFormat,
}
impl Output {
    fn new(index: usize, total: usize, format: OutputFormat) -> Option<Self> {
        (total > 0 && total <= 8 && index < total).then_some(Self {
            index,
            total,
            format,
        })
    }
    fn annotate(self, record: &mut Record) {
        record.insert("output_index".into(), self.index.into());
        record.insert("output_total".into(), self.total.into());
        record.insert("output_format".into(), self.format.as_str().into());
    }
}

#[derive(Clone, Copy)]
enum Target {
    Phase(Phase),
    Output(Output),
}

/// `finish` is the only success path. An ordinary early drop reports abortion.
pub(super) struct Scope {
    sink: Option<Arc<dyn Sink>>,
    target: Target,
    finished: bool,
}
impl Scope {
    fn new(sink: Option<Arc<dyn Sink>>, target: Target) -> Self {
        let scope = Self {
            sink,
            target,
            finished: false,
        };
        scope.emit("started");
        scope
    }
    pub fn finish(mut self) {
        // A panicking observer must not make Drop emit a second terminal record.
        self.finished = true;
        self.emit("finished");
    }
    fn emit(&self, state: &'static str) {
        let Some(sink) = &self.sink else { return };
        let mut record = serde_json::json!({
            "stage":"pipeline", "event":"progress", "state":state,
            "scope":match self.target { Target::Phase(phase) => phase.token(), Target::Output(_) => "output" }
        }).as_object().expect("fixed pipeline record").clone();
        if let Target::Output(output) = self.target {
            output.annotate(&mut record);
        }
        sink.emit(&record);
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        if !self.finished && !std::thread::panicking() {
            // Error/cancellation/panic are not a successful stage completion.
            self.finished = true;
            self.emit("aborted");
        }
    }
}

struct OutputSink {
    parent: Arc<dyn Sink>,
    output: Output,
}
impl Sink for OutputSink {
    fn emit(&self, record: &Record) {
        let mut scoped = record.clone();
        self.output.annotate(&mut scoped);
        self.parent.emit(&scoped);
    }
}

impl Converter {
    /// Describe only bounded logical work, without filenames or operator labels.
    pub(super) fn pipeline_work_plan(&self, outputs: usize, estimates_enabled: bool) {
        if !(1..=8).contains(&outputs) {
            return;
        }
        if let Some(sink) = &self.backend.activity {
            sink.emit(
                serde_json::json!({
                    "stage":"pipeline", "event":"progress", "scope":"work-plan",
                    "state":"ready", "planned_outputs":outputs,
                    "estimates_enabled":estimates_enabled, "source_completed":2,
                })
                .as_object()
                .expect("fixed work-plan record"),
            );
        }
    }

    pub(super) fn pipeline_step(&self, step: Step) {
        if let Some(sink) = &self.backend.activity {
            // The child OutputSink supplies its fixed ordinal and format.
            sink.emit(
                serde_json::json!({
                    "stage":"pipeline", "event":"progress", "scope":"output-step",
                    "state":"started", "step":step.token(),
                })
                .as_object()
                .expect("fixed output-step record"),
            );
        }
    }

    pub(super) fn pipeline_phase(&self, phase: Phase) -> Scope {
        Scope::new(self.backend.activity.clone(), Target::Phase(phase))
    }

    /// Attach only a bounded ordinal and a fixed format, never operator labels.
    pub(super) fn pipeline_output(
        &self,
        index: usize,
        total: usize,
        format: OutputFormat,
    ) -> (Self, Scope) {
        let mut child = self.with_processing_clone(self.processing_mode);
        let output = Output::new(index, total, format);
        let scope = if let Some(output) = output {
            let scope = Scope::new(self.backend.activity.clone(), Target::Output(output));
            child.backend.activity = self.backend.activity.as_ref().map(|parent| {
                Arc::new(OutputSink {
                    parent: parent.clone(),
                    output,
                }) as Arc<dyn Sink>
            });
            scope
        } else {
            // Invalid telemetry never creates an unbounded observer identity.
            // The operation's actual output list is validated separately.
            Scope::new(None, Target::Phase(Phase::Outputs))
        };
        (child, scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        conversion::Converter,
        events::{Collector, NoSink, Sink},
        inspection::OutputFormat,
        toolrun::{Secrets, ToolRunner},
    };
    use std::sync::Arc;

    fn command(sink: Arc<dyn Sink>) -> Converter {
        Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
            "unused-packager",
            "unused-ffmpeg",
        )
        .with_activity_sink(sink)
    }

    #[test]
    fn milestones_complete_only_explicitly_and_an_early_drop_is_aborted() {
        let sink = Arc::new(Collector::default());
        let command = command(sink.clone());
        command.pipeline_phase(Phase::Source).finish();
        drop(command.pipeline_phase(Phase::FinalCheck));
        let records = sink.records();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0]["scope"], "source");
        assert_eq!(records[0]["state"], "started");
        assert_eq!(records[1]["state"], "finished");
        assert_eq!(records[2]["scope"], "final-check");
        assert_eq!(records[3]["state"], "aborted");
        assert!(
            records
                .iter()
                .all(|r| r["stage"] == "pipeline" && r["event"] == "progress")
        );
    }

    #[test]
    fn parallel_output_contexts_keep_fixed_identity_without_contaminating_parent() {
        let sink = Arc::new(Collector::default());
        let command = command(sink.clone());
        let (dash, dash_scope) = command.pipeline_output(0, 2, OutputFormat::Dash);
        let (hls, hls_scope) = command.pipeline_output(1, 2, OutputFormat::Hls);
        std::thread::scope(|threads| {
            threads.spawn(|| {
                let _scope = dash
                    .backend
                    .scope(crate::conversion::measurement::Phase::Verification);
                dash.backend.completed_work_unit(0, 1);
            });
            threads.spawn(|| {
                let _scope = hls
                    .backend
                    .scope(crate::conversion::measurement::Phase::Packaging);
                hls.backend.completed_work_unit(0, 1);
            });
        });
        dash_scope.finish();
        hls_scope.finish();
        command.backend.completed_work_unit(0, 1);
        let records = sink.records();
        assert_eq!(records[0]["output_index"], 0);
        assert_eq!(records[0]["output_total"], 2);
        assert_eq!(records[0]["output_format"], "dash");
        assert_eq!(records[1]["output_index"], 1);
        assert_eq!(records[1]["output_format"], "hls");
        for record in &records[2..records.len() - 1] {
            let index = record["output_index"].as_u64().unwrap();
            assert_eq!(record["output_total"], 2);
            assert_eq!(
                record["output_format"],
                if index == 0 { "dash" } else { "hls" }
            );
            assert!(index < 2);
        }
        assert!(!records.last().unwrap().contains_key("output_index"));
        let text = serde_json::to_string(&records).unwrap();
        assert!(!text.contains("unused-packager") && !text.contains("unused-ffmpeg"));
    }

    #[test]
    fn an_observer_panic_never_fabricates_a_terminal_success_or_double_emits() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        struct Failing(AtomicUsize);
        impl Sink for Failing {
            fn emit(&self, _: &crate::events::Record) {
                assert_ne!(
                    self.0.fetch_add(1, Ordering::SeqCst),
                    0,
                    "synthetic observer failure"
                );
            }
        }
        let sink = Arc::new(Failing(AtomicUsize::new(0)));
        let command = command(sink.clone());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _scope = command.pipeline_phase(Phase::Publication);
        }));
        assert!(result.is_err());
        assert_eq!(sink.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn invalid_output_bounds_never_emit_unbounded_or_misattributed_context() {
        let sink = Arc::new(Collector::default());
        let command = command(sink.clone());
        for (index, total) in [(0, 0), (8, 8), (0, 9), (usize::MAX, usize::MAX)] {
            let (child, scope) = command.pipeline_output(index, total, OutputFormat::HlsTs);
            child.backend.completed_work_unit(0, 1);
            scope.finish();
        }
        assert_eq!(sink.records().len(), 4);
        assert!(
            sink.records()
                .iter()
                .all(|r| r["stage"] == "conversion" && !r.contains_key("output_index"))
        );
    }
}
