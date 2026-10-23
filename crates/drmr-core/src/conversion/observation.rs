//! Fixed task lifetimes supplement phase observations without retaining media data.
use crate::events::Sink;
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(super) enum Task {
    PacketTiming,
    EncodedContent,
    DecodedContent,
    ManifestLint,
}
impl Task {
    fn token(self) -> &'static str {
        match self {
            Self::PacketTiming => "packet-timing",
            Self::EncodedContent => "encoded-content",
            Self::DecodedContent => "decoded-content",
            Self::ManifestLint => "manifest-lint",
        }
    }
}
pub(super) struct Scope(Option<(Arc<dyn Sink>, Task)>);
impl Scope {
    pub fn new(sink: Option<&Arc<dyn Sink>>, task: Task) -> Self {
        if let Some(sink) = sink {
            emit(sink.as_ref(), task, "started");
        }
        Self(sink.map(|sink| (sink.clone(), task)))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        if let Some((sink, task)) = &self.0 {
            // Closure reports a lifetime, including failure and cancellation, not success.
            emit(sink.as_ref(), *task, "closed");
        }
    }
}
fn emit(sink: &dyn Sink, task: Task, state: &'static str) {
    sink.emit(serde_json::json!({"stage":"conversion", "event":"task", "task":task.token(), "state":state}).as_object().unwrap());
}
