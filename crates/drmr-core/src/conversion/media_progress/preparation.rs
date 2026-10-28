//! An outer native operation owns completion; rendition readers report provisional units.
use super::{
    Arc, Batch, Mutex, Operation, PoisonError, Purpose, Sink, Subject, Unit, UnitTask, Units,
};

#[derive(Default)]
struct State {
    batch: Option<Arc<Batch>>,
    units: Option<Units>,
    closed: bool,
}

pub(in crate::conversion) struct PreparationContext {
    sink: Arc<dyn Sink>,
    subject: Subject,
    purpose: Purpose,
    state: Mutex<State>,
}
impl PreparationContext {
    pub(in crate::conversion) fn start(&self, ordinal: usize) -> Option<UnitTask> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.closed {
            return None;
        }
        if state.units.is_none() {
            let batch = state.batch.as_ref()?;
            let super::Task::Preparation(_, _, unit) = batch.task else {
                return None;
            };
            state.units = Batch::units(
                Some(self.sink.clone()),
                Operation::Preparation,
                self.subject,
                self.purpose,
                unit,
                batch.weights.len(),
            );
        }
        state.units.as_ref()?.start(ordinal)
    }
    pub(in crate::conversion) fn begin(&self, total: usize, unit: Unit) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if !state.closed && state.batch.is_none() {
            state.batch =
                Batch::preparation(self.sink.clone(), self.subject, self.purpose, unit, total)
                    .map(Arc::new);
        }
    }
    pub(in crate::conversion) fn completed(&self, ordinal: usize) {
        let batch = {
            let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            (!state.closed).then(|| state.batch.clone()).flatten()
        };
        if let Some(batch) = batch {
            batch.completed(ordinal);
        }
    }
    fn close(&self, finish: bool) {
        let (batch, units) = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.closed = true;
            (state.batch.clone(), state.units.take())
        };
        let units_closed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if let Some(units) = units {
                if finish {
                    units.finish();
                }
                drop(units);
            }
        }));
        if let Err(panic) = units_closed {
            // Retained owners must not call the failed observer during their later Drop.
            if let Some(batch) = &batch {
                batch.silence();
            }
            std::panic::resume_unwind(panic);
        }
        if let Some(batch) = batch {
            if std::thread::panicking() {
                batch.silence();
                return;
            }
            if finish {
                batch.finish();
            }
            // An incomplete explicit finish is an abortion, never a late
            // callback waiting for a retained backend clone to be dropped.
            batch.abort();
        }
    }
}

pub(in crate::conversion) struct Scope {
    context: Option<Arc<PreparationContext>>,
    finished: bool,
}
impl Scope {
    pub(in crate::conversion) fn finish(mut self) {
        self.finished = true;
        if let Some(context) = &self.context {
            context.close(true);
        }
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        if !self.finished
            && let Some(context) = &self.context
        {
            context.close(false);
        }
    }
}

pub(in crate::conversion) fn scoped(
    backend: &super::super::backend::Backend,
    subject: Subject,
    purpose: Purpose,
) -> (super::super::backend::Backend, Scope) {
    let context = backend.activity.as_ref().map(|sink| {
        Arc::new(PreparationContext {
            sink: sink.clone(),
            subject,
            purpose,
            state: Mutex::default(),
        })
    });
    let mut backend = backend.clone();
    backend.preparation.clone_from(&context);
    (
        backend,
        Scope {
            context,
            finished: false,
        },
    )
}

#[cfg(test)]
mod tests;
