//! One bounded leaf pool shares admission with all media work in the command.
use super::{ConversionError, Stop, cancelled};
use crate::{
    concurrency::{Pool, Unit, run_ordered},
    resources::{Budget, Claim, Hardware, Permit, ReservationError, Workload},
    toolrun::ToolRunner,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

pub(in crate::conversion) const MEMORY_BYTES: u64 = 128 * 1024 * 1024;
const WAIT_SLICE: Duration = Duration::from_millis(20);

/// The standalone source API remains sequential and owns no runner.
#[derive(Clone, Default)]
pub(super) struct Execution {
    runner: Option<Arc<ToolRunner>>,
    maximum_workers: Option<usize>,
    timeout: Duration,
    quiet: bool,
    activity: Option<Arc<dyn crate::events::Sink>>,
    #[cfg(test)]
    hardware: Option<Hardware>,
}

#[derive(Clone, Copy)]
pub(super) struct Control<'a> {
    external: &'a Stop,
    failure: Option<&'a Stop>,
}

impl<'a> Control<'a> {
    pub fn new(external: &'a Stop) -> Self {
        Self {
            external,
            failure: None,
        }
    }
    pub fn check(self) -> Result<(), ConversionError> {
        cancelled(self.external)?;
        if let Some(failure) = self.failure {
            cancelled(failure)?;
        }
        Ok(())
    }
}

impl Execution {
    pub fn for_converter(converter: &super::super::Converter) -> Self {
        Self {
            runner: Some(converter.backend.runner.clone()),
            maximum_workers: converter.maximum_workers,
            timeout: converter.backend.timeout,
            quiet: false,
            activity: converter.backend.activity.clone(),
            #[cfg(test)]
            hardware: None,
        }
    }

    fn workers(&self, count: usize) -> Result<usize, ConversionError> {
        let Some(runner) = &self.runner else {
            return Ok(1);
        };
        let hardware = Hardware::detect();
        #[cfg(test)]
        let hardware = self.hardware.unwrap_or(hardware);
        let mut budget = Budget::choose(
            hardware,
            Workload {
                max_pixels: Some(0),
                input_buffer_bytes: Some(64 * 1024),
                tracks: 1,
            },
            self.maximum_workers,
        )
        .map_err(|_| ConversionError("resource-budget"))?;
        let (cpus, memory) = runner.native_capacity();
        budget.cpu_tokens = budget.cpu_tokens.min(cpus);
        budget.memory_tokens_bytes = budget.memory_tokens_bytes.min(memory);
        // Includes bounded graph/directory metadata as well as the 64 KiB
        // stream. Admission estimates are not operating-system RSS limits.
        budget.estimated_child_bytes = MEMORY_BYTES;
        budget.workers = budget
            .workers
            .min(budget.cpu_tokens)
            .min(usize::try_from(budget.memory_tokens_bytes / MEMORY_BYTES).unwrap_or(usize::MAX))
            .min(count.max(1));
        if budget.workers == 0 {
            return Err(ConversionError("resource-budget"));
        }
        if !self.quiet {
            runner.note_resource_budget(&budget);
        }
        Ok(budget.workers)
    }

    pub fn check_budget(&self) -> Result<(), ConversionError> {
        self.workers(1).map(|_| ())
    }

    pub fn without_observers(&self) -> Self {
        let mut execution = self.clone();
        execution.quiet = true;
        execution.activity = None;
        execution
    }

    pub fn integrity(
        &self,
        bytes: u64,
        files: usize,
    ) -> Option<super::super::media_progress::Integrity> {
        if self.quiet {
            return None;
        }
        super::super::media_progress::Integrity::new(self.activity.clone(), bytes, files)
    }

    pub fn one<T>(
        &self,
        control: Control<'_>,
        work: impl FnOnce() -> Result<T, ConversionError>,
    ) -> Result<T, ConversionError> {
        control.check()?;
        let _permit = self.reserve(control)?;
        control.check()?;
        let result = work()?;
        control.check()?;
        Ok(result)
    }

    pub fn run<T: Send>(
        &self,
        stop: &Stop,
        count: usize,
        work: impl Fn(usize, Control<'_>) -> Result<T, ConversionError> + Sync,
    ) -> Result<Vec<T>, ConversionError> {
        cancelled(stop)?;
        if count == 0 {
            return Ok(Vec::new());
        }
        let workers = self.workers(count)?;
        let failure = Stop::new();
        let control = Control {
            external: stop,
            failure: Some(&failure),
        };
        let invoke = |ordinal| self.one(control, || work(ordinal, control));
        if workers == 1 {
            return (0..count).map(invoke).collect();
        }
        std::thread::scope(|scope| {
            let pool = Pool::new(scope, workers);
            let units = (0..count).map(|ordinal| {
                let invoke = &invoke;
                Box::new(move || invoke(ordinal)) as Unit<'_, T, ConversionError>
            });
            run_ordered(|unit| pool.submit(unit), units, Some(&failure))
        })
    }

    fn reserve(&self, control: Control<'_>) -> Result<Option<Permit>, ConversionError> {
        let Some(runner) = &self.runner else {
            return Ok(None);
        };
        let started = Instant::now();
        loop {
            control.check()?;
            let remaining = self.timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(ConversionError("media-tool-timeout"));
            }
            match runner.native_permit(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: MEMORY_BYTES,
                },
                control.external,
                remaining.min(WAIT_SLICE),
            ) {
                Ok(permit) => return Ok(Some(permit)),
                Err(ReservationError::Cancelled) => return Err(ConversionError("cancelled")),
                Err(ReservationError::Insufficient) => {
                    return Err(ConversionError("resource-budget"));
                }
                Err(ReservationError::Timeout) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
impl Execution {
    pub fn for_test(workers: Option<usize>, cpus: usize, memory: u64) -> Self {
        use crate::{events::NoSink, resources::Limiter, toolrun::Secrets};
        Self {
            runner: Some(Arc::new(
                ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
                    .with_limiter(Arc::new(Limiter::new(cpus, memory))),
            )),
            maximum_workers: workers,
            timeout: Duration::from_secs(5),
            quiet: false,
            activity: None,
            hardware: Some(Hardware::from_observations(
                8,
                Some(8 * 1024 * 1024 * 1024),
                None,
            )),
        }
    }
}
