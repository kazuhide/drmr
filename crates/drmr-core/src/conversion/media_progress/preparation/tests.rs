use super::*;
use crate::events::{Collector, Record};

const OBSERVER_PANIC: &str = "synthetic rendition terminal observer panic";

struct TerminalPanic {
    records: Collector,
    state: &'static str,
}
impl Sink for TerminalPanic {
    fn emit(&self, record: &Record) {
        self.records.emit(record);
        if record.get("stage").and_then(serde_json::Value::as_str) == Some("rendition-progress")
            && record.get("state").and_then(serde_json::Value::as_str) == Some(self.state)
        {
            std::panic::panic_any(OBSERVER_PANIC);
        }
    }
}

fn backend(sink: Arc<dyn Sink>) -> crate::conversion::backend::Backend {
    crate::conversion::Converter::new(
        Arc::new(crate::toolrun::ToolRunner::new(
            Arc::new(crate::toolrun::Secrets::new()),
            Arc::new(crate::events::NoSink),
        )),
        "unused-private-tool",
        "unused-private-decoder",
    )
    .with_activity_sink(sink)
    .backend
}

fn assert_terminal_observer_panic_silences_retained_media(finish: bool) {
    let sink = Arc::new(TerminalPanic {
        records: Collector::default(),
        state: if finish { "finished" } else { "aborted" },
    });
    let (backend, scope) = scoped(&backend(sink.clone()), Subject::Output, Purpose::Conversion);
    let context = backend.preparation().unwrap().clone();
    context.begin(1, Unit::Tracks);
    let task = context.start(0).unwrap();
    context.completed(0);
    task.finish();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if finish {
            scope.finish();
        } else {
            drop(scope);
        }
    }))
    .expect_err("the original observer panic must reach the caller");
    assert_eq!(panic.downcast_ref::<&str>(), Some(&OBSERVER_PANIC));
    let media_terminal = {
        let state = context.state.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(state.closed);
        let batch = state.batch.as_ref().unwrap();
        batch
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .terminal
    };
    let before = sink.records.records();
    context.begin(2, Unit::Tracks);
    context.completed(1);
    assert!(context.start(0).is_none());
    assert_eq!(sink.records.records(), before);
    drop(context);
    let final_drop = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(backend)));
    assert!(
        final_drop.is_ok(),
        "final owner drop must not repeat the observer panic"
    );
    assert_eq!(
        sink.records.records(),
        before,
        "a rendition observer panic must silence the retained media batch before its final owner drops"
    );
    assert!(
        media_terminal,
        "the media batch must already be terminal before retained owners drop"
    );
    assert_eq!(
        before
            .iter()
            .filter(|record| {
                record.get("stage").and_then(serde_json::Value::as_str)
                    == Some("rendition-progress")
                    && record.get("state").and_then(serde_json::Value::as_str) == Some(sink.state)
            })
            .count(),
        1
    );
}

#[test]
fn finished_unit_observer_panic_silences_retained_media_batch() {
    assert_terminal_observer_panic_silences_retained_media(true);
}

#[test]
fn aborted_unit_observer_panic_silences_retained_media_batch() {
    assert_terminal_observer_panic_silences_retained_media(false);
}

#[test]
fn healthy_preparation_closes_unit_and_media_batches_once_in_order() {
    for finish in [false, true] {
        let sink = Arc::new(Collector::default());
        let (backend, scope) = scoped(&backend(sink.clone()), Subject::Output, Purpose::Conversion);
        let context = backend.preparation().unwrap().clone();
        context.begin(1, Unit::Tracks);
        context.completed(0);
        context.start(0).unwrap().finish();
        if finish {
            scope.finish();
        } else {
            drop(scope);
        }
        let records = sink.records();
        let state = if finish { "finished" } else { "aborted" };
        let terminal_stages = records
            .iter()
            .filter(|record| record.get("state").and_then(serde_json::Value::as_str) == Some(state))
            .map(|record| record["stage"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(terminal_stages, ["rendition-progress", "media-progress"]);
        context.completed(0);
        assert!(context.start(0).is_none());
        drop(context);
        drop(backend);
        assert_eq!(sink.records(), records);
    }
}
