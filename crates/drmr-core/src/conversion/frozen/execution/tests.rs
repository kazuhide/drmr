use super::*;
use crate::resources::Limiter;
use std::sync::{
    Barrier,
    atomic::{AtomicUsize, Ordering},
};

#[test]
fn hardware_memory_and_user_maximum_all_bound_the_snapshot_pool() {
    for (maximum, expected) in [(Some(1), 1), (Some(2), 2), (Some(4), 4), (None, 4)] {
        let execution = Execution::for_test(maximum, 4, MEMORY_BYTES * 4);
        assert_eq!(execution.workers(16).unwrap(), expected);
        assert_eq!(execution.workers(1).unwrap(), 1);
    }
    let mut execution = Execution::for_test(None, 4, MEMORY_BYTES * 4);
    execution.hardware = Some(Hardware::from_observations(8, None, None));
    assert_eq!(execution.workers(16).unwrap(), 1);
    execution.hardware = Some(Hardware::from_observations(8, Some(0), None));
    assert_eq!(
        execution.workers(16).unwrap_err(),
        ConversionError("resource-budget")
    );
    for maximum in [Some(0), Some(65)] {
        let execution = Execution::for_test(maximum, 4, MEMORY_BYTES * 4);
        assert_eq!(
            execution.workers(16).unwrap_err(),
            ConversionError("resource-budget")
        );
    }
    assert_eq!(
        Execution::for_test(None, 4, MEMORY_BYTES - 1)
            .workers(16)
            .unwrap_err(),
        ConversionError("resource-budget")
    );
    assert_eq!(
        Execution::for_test(None, 4, MEMORY_BYTES)
            .workers(16)
            .unwrap(),
        1
    );
}

#[test]
fn real_parallel_work_is_bounded_and_keeps_submission_order_across_windows() {
    let execution = Execution::for_test(Some(2), 2, MEMORY_BYTES * 2);
    let start = Barrier::new(2);
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let results = execution
        .run(&Stop::new(), 300, |index, _| {
            let count = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(count, Ordering::SeqCst);
            if index < 2 {
                start.wait();
            }
            std::thread::yield_now();
            active.fetch_sub(1, Ordering::SeqCst);
            Ok(index)
        })
        .unwrap();
    assert_eq!(results, (0..300).collect::<Vec<_>>());
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[test]
fn failure_drains_started_work_without_poisoning_the_command_or_runner() {
    let execution = Execution::for_test(Some(2), 2, MEMORY_BYTES * 2);
    let stop = Stop::new();
    let start = Barrier::new(2);
    let started = AtomicUsize::new(0);
    let drained = AtomicUsize::new(0);
    let error = execution
        .run(&stop, 300, |index, control| {
            started.fetch_add(1, Ordering::SeqCst);
            assert!(index < 2, "no queued work starts after the failure");
            start.wait();
            if index == 0 {
                return Err(ConversionError("synthetic-copy-failure"));
            }
            assert!(control.failure.unwrap().wait(Some(Duration::from_secs(2))));
            drained.fetch_add(1, Ordering::SeqCst);
            control.check()
        })
        .unwrap_err();
    assert_eq!(error, ConversionError("synthetic-copy-failure"));
    assert_eq!(started.load(Ordering::SeqCst), 2);
    assert_eq!(drained.load(Ordering::SeqCst), 1);
    assert!(!stop.is_set());
    assert_eq!(
        execution.run(&stop, 3, |index, _| Ok(index)).unwrap(),
        [0, 1, 2]
    );
}

#[test]
fn failure_selection_uses_submission_order_instead_of_completion_order() {
    let execution = Execution::for_test(Some(2), 2, MEMORY_BYTES * 2);
    let start = Barrier::new(2);
    let error = execution
        .run::<()>(&Stop::new(), 200, |index, control| {
            start.wait();
            if index == 0 {
                assert!(control.failure.unwrap().wait(Some(Duration::from_secs(2))));
                Err(ConversionError("earlier-submission"))
            } else {
                Err(ConversionError("earlier-completion"))
            }
        })
        .unwrap_err();
    assert_eq!(error, ConversionError("earlier-submission"));
}

#[test]
fn panics_join_other_running_tasks_and_return_every_reservation() {
    let execution = Execution::for_test(Some(2), 2, MEMORY_BYTES * 2);
    let stop = Stop::new();
    let start = Barrier::new(2);
    let drained = AtomicUsize::new(0);
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execution.run(&stop, 200, |index, control| {
            start.wait();
            assert_ne!(index, 0, "synthetic worker panic");
            assert!(control.failure.unwrap().wait(Some(Duration::from_secs(2))));
            drained.fetch_add(1, Ordering::SeqCst);
            control.check()
        })
    }));
    assert!(panic.is_err());
    assert_eq!(drained.load(Ordering::SeqCst), 1);
    assert!(!stop.is_set());
    assert_eq!(
        execution.run(&stop, 2, |index, _| Ok(index)).unwrap(),
        [0, 1]
    );
}

#[test]
fn cancellation_drains_both_running_workers() {
    let execution = Execution::for_test(Some(2), 2, MEMORY_BYTES * 2);
    let stop = Stop::new();
    let start = Barrier::new(3);
    let drained = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            start.wait();
            stop.set();
        });
        let result = execution.run(&stop, 200, |_, control| {
            start.wait();
            assert!(stop.wait(Some(Duration::from_secs(2))));
            drained.fetch_add(1, Ordering::SeqCst);
            control.check()
        });
        assert_eq!(result.unwrap_err(), ConversionError("cancelled"));
    });
    assert_eq!(drained.load(Ordering::SeqCst), 2);
}

#[test]
fn queued_admission_obeys_deadlines_and_the_private_failure_gate() {
    let mut execution = Execution::for_test(Some(1), 1, MEMORY_BYTES);
    let limiter = Arc::new(Limiter::new(1, MEMORY_BYTES));
    let runner = execution.runner.take().unwrap();
    let runner = Arc::try_unwrap(runner)
        .ok()
        .unwrap()
        .with_limiter(limiter.clone());
    execution.runner = Some(Arc::new(runner));
    let held = limiter
        .acquire(
            Claim {
                cpu_threads: 1,
                memory_bytes: MEMORY_BYTES,
            },
            None,
            None,
        )
        .unwrap();
    execution.timeout = Duration::from_millis(40);
    let stop = Stop::new();
    let started = Instant::now();
    assert_eq!(
        execution.one(Control::new(&stop), || Ok(())).unwrap_err(),
        ConversionError("media-tool-timeout")
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    let failure = Stop::new();
    execution.timeout = Duration::from_secs(2);
    std::thread::scope(|scope| {
        scope.spawn(|| failure.set());
        let result = execution.one(
            Control {
                external: &stop,
                failure: Some(&failure),
            },
            || Ok(()),
        );
        assert_eq!(result.unwrap_err(), ConversionError("cancelled"));
    });
    assert!(!stop.is_set());
    drop(held);
    execution.one(Control::new(&stop), || Ok(())).unwrap();
}
