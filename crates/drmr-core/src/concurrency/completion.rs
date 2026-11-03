//! Acquisition can refill its bounded window without waiting for a slow early unit.
use super::{
    MAX_PENDING_TASKS, Outcome, Stop, Task, Unit, UnitError, lock, raised, raised_for_real,
    run_gated,
};
use std::{
    panic,
    sync::{Arc, Condvar, Mutex, PoisonError},
    time::Duration,
};

/// Preserve submission-order values and failures, but consume any finished task.
/// This avoids stopping downloads when an earlier acquisition unit is inspecting
/// a complete rendition. Only pending tasks are bounded here; returned values
/// remain owned by the caller just as with the ordinary ordered coordinator.
pub(crate) fn run_completion_order<'a, T, E, S>(
    mut submit: S,
    units: impl IntoIterator<Item = Unit<'a, T, E>>,
    stop: Option<&Stop>,
) -> Result<Vec<T>, E>
where
    T: Send + 'a,
    E: UnitError + Send + 'a,
    S: FnMut(Unit<'a, T, E>) -> Task<Result<T, E>>,
{
    let gate = stop.cloned().unwrap_or_default();
    let mut units = units.into_iter().enumerate();
    let mut tasks: Vec<(usize, Task<Result<T, E>>)> = Vec::new();
    let mut results = Vec::new();
    let signal = Arc::new((Mutex::new(()), Condvar::new()));
    loop {
        while tasks.len() < MAX_PENDING_TASKS && (!gate.is_set() || tasks.is_empty()) {
            let Some((index, unit)) = units.next() else {
                break;
            };
            let gate = gate.clone();
            let gated: Unit<'a, T, E> = Box::new(move || run_gated(&gate, unit));
            let task = submit(gated);
            let signal = Arc::clone(&signal);
            task.on_done(move || {
                let _guard = lock(&signal.0);
                signal.1.notify_one();
            });
            tasks.push((index, task));
        }
        if tasks.is_empty() {
            results.sort_unstable_by_key(|(index, _)| *index);
            return Ok(results.into_iter().map(|(_, value)| value).collect());
        }
        {
            let mut guard = lock(&signal.0);
            while !tasks.iter().any(|(_, task)| task.done()) {
                guard = signal
                    .1
                    .wait_timeout(guard, Duration::from_millis(50))
                    .unwrap_or_else(PoisonError::into_inner)
                    .0;
            }
        }
        // Finished states are immutable. Select only those known to be done
        // before checking for failure, so a new failure cannot be consumed as
        // a successful completion during the following removal pass.
        let ready = tasks
            .iter()
            .enumerate()
            .filter_map(|(position, (_, task))| task.done().then_some(position))
            .collect::<Vec<_>>();
        if tasks.iter().any(|(_, task)| raised(task)) {
            gate.set();
            for (_, task) in &tasks {
                task.cancel();
            }
            for (_, task) in &tasks {
                task.wait();
            }
            // Removal preserves task order, and previous removals were successes.
            let chosen = tasks
                .iter()
                .map(|(_, task)| task)
                .find(|task| raised_for_real(task))
                .or_else(|| tasks.iter().map(|(_, task)| task).find(|task| raised(task)))
                .expect("a task raised");
            return match chosen.take() {
                Some(Outcome::Returned(Err(error))) => Err(error),
                Some(Outcome::Panicked(payload)) => panic::resume_unwind(payload),
                _ => unreachable!("the chosen task raised"),
            };
        }
        for position in ready.into_iter().rev() {
            let (index, task) = tasks.remove(position);
            match task.take().map(Outcome::into_value) {
                Some(Ok(value)) => results.push((index, value)),
                _ => unreachable!("the completed task returned a value"),
            }
        }
    }
}

#[cfg(test)]
#[path = "completion_tests.rs"]
mod tests;
