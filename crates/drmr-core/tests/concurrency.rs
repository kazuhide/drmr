//! Ordered parallel results, bounded workers, cancellation, and deterministic failures.

use drmr_core::concurrency::{
    Cancelled, Pool, Stop, Task, Unit, UnitError, default_workers, run_ordered, workers_for,
};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread::ThreadId;
use std::time::Duration;

/// The exceptions the tests raise.
#[derive(Clone, Debug, PartialEq, Eq)]
enum TestError {
    Cancelled,
    Value(&'static str),
    Key(&'static str),
    Runtime(&'static str),
}

impl UnitError for TestError {
    fn cancelled() -> Self {
        TestError::Cancelled
    }

    fn is_cancelled(&self) -> bool {
        matches!(self, TestError::Cancelled)
    }
}

type TestUnit<'a, T> = Unit<'a, T, TestError>;

fn unit<'a, T>(run: impl FnOnce() -> Result<T, TestError> + Send + 'a) -> TestUnit<'a, T> {
    Box::new(run)
}

#[test]
fn results_keep_submission_order_whatever_finishes_first() {
    let out = std::thread::scope(|scope| {
        let pool = Pool::new(scope, 4);
        let units: Vec<TestUnit<'_, (u64, ThreadId)>> = [150, 50, 100, 0]
            .into_iter()
            .map(|delay| {
                unit(move || {
                    std::thread::sleep(Duration::from_millis(delay));
                    Ok((delay, std::thread::current().id()))
                })
            })
            .collect();
        run_ordered(|u| pool.submit(u), units, None)
    })
    .unwrap();
    assert_eq!(
        out.iter().map(|(delay, _)| *delay).collect::<Vec<_>>(),
        [150, 50, 100, 0]
    );
    let threads: HashSet<ThreadId> = out.iter().map(|(_, thread)| *thread).collect();
    assert!(threads.len() > 1, "the units really ran on several threads");
}

#[test]
fn fail_fast_cancels_the_queue_and_reports_the_earliest_failure_among_the_units_that_ran() {
    let ran = Arc::new(Mutex::new(Vec::new()));
    let stop = Stop::new();
    let later_failed = Stop::new();
    let unit0_started = Stop::new();
    let result = std::thread::scope(|scope| {
        let pool = Pool::new(scope, 2);
        let units: Vec<TestUnit<'_, usize>> = (0..12)
            .map(|index| {
                let (ran, stop, later_failed, unit0_started) = (
                    Arc::clone(&ran),
                    stop.clone(),
                    later_failed.clone(),
                    unit0_started.clone(),
                );
                unit(move || {
                    if stop.is_set() {
                        return Err(TestError::Cancelled);
                    }
                    ran.lock().unwrap().push(index);
                    if index == 0 {
                        // unit 2 fails first on the clock, once unit 0 is in flight (else the
                        // start gate could skip unit 0); unit 0 fails only afterwards and is
                        // still the one reimplemented, as the sequential loop would have raised it
                        unit0_started.set();
                        assert!(later_failed.wait(Some(Duration::from_secs(5))));
                        return Err(TestError::Value("unit-0"));
                    }
                    if index == 2 {
                        assert!(unit0_started.wait(Some(Duration::from_secs(5))));
                        later_failed.set();
                        return Err(TestError::Value("unit-2"));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                    Ok(index)
                })
            })
            .collect();
        run_ordered(|u| pool.submit(u), units, Some(&stop))
    });
    assert_eq!(result, Err(TestError::Value("unit-0")));
    assert!(stop.is_set());
    let ran = ran.lock().unwrap();
    assert!(
        ran.contains(&0) && ran.contains(&2) && ran.len() < 12,
        "the queue never drained: {ran:?}"
    );
}

#[test]
fn units_that_only_observed_the_stop_are_never_the_reported_failure() {
    let stop = Stop::new();
    let futures: Mutex<Vec<Task<Result<&'static str, TestError>>>> = Mutex::new(Vec::new());
    let result = std::thread::scope(|scope| {
        let pool = Pool::new(scope, 2);
        let late_stop = stop.clone();
        let late = unit(move || {
            if late_stop.wait(Some(Duration::from_secs(5))) {
                // released by the failure, never by a timer
                return Err(TestError::Cancelled);
            }
            Ok("ran")
        });
        let failing = unit(|| Err(TestError::Key("real")));
        run_ordered(
            |u| {
                let task = pool.submit(u);
                futures.lock().unwrap().push(task.clone());
                task
            },
            vec![late, failing],
            Some(&stop),
        )
    });
    assert_eq!(result, Err(TestError::Key("real")));
    let futures = futures.lock().unwrap();
    let first = futures[0]
        .take()
        .map(drmr_core::concurrency::Outcome::into_value);
    assert_eq!(first, Some(Err(TestError::Cancelled)));
}

#[test]
fn an_empty_batch_and_a_sequential_batch_behave_like_a_loop() {
    std::thread::scope(|scope| {
        let pool = Pool::new(scope, 1);
        let empty: Vec<TestUnit<'_, i32>> = Vec::new();
        assert_eq!(run_ordered(|u| pool.submit(u), empty, None), Ok(vec![]));
        let units = vec![unit(|| Ok(1)), unit(|| Ok(2))];
        assert_eq!(run_ordered(|u| pool.submit(u), units, None), Ok(vec![1, 2]));
    });
}

#[test]
fn default_workers_follow_the_cpu_count_up_to_thirty_two() {
    for (cpus, expected) in [
        (Some(64), 32),
        (Some(32), 32),
        (None, 4),
        (Some(3), 3),
        (Some(1), 1),
    ] {
        assert_eq!(workers_for(None, || cpus), expected, "cpu_count {cpus:?}");
    }
    assert_eq!(workers_for(None, || Some(0)), 4, "a zero count is `or 4`");
}

#[test]
fn default_workers_follow_the_affinity_mask_where_the_platform_has_one() {
    for (affinity, expected) in [(4, 4), (48, 32), (1, 1)] {
        assert_eq!(
            workers_for(Some(affinity), || panic!(
                "the CPU count is not asked when a mask exists"
            )),
            expected
        );
    }
    assert_eq!(
        workers_for(Some(0), || Some(64)),
        4,
        "an empty mask is `cpus or 4`"
    );
    let workers = default_workers();
    assert!((1..=32).contains(&workers), "{workers}");
}

#[test]
fn a_unit_the_pool_picked_after_a_failure_never_starts_even_without_a_caller_s_stop() {
    // 's start gate: with one worker the pool hands unit 1 to the thread the moment unit
    // 0 has failed, before the coordinator can cancel it; the failing unit itself has set the
    // flag.
    let ran = Arc::new(Mutex::new(Vec::new()));
    let futures: Mutex<Vec<Task<Result<usize, TestError>>>> = Mutex::new(Vec::new());
    let result = std::thread::scope(|scope| {
        let pool = Pool::new(scope, 1);
        let units: Vec<TestUnit<'_, usize>> = (0..3)
            .map(|index| {
                let ran = Arc::clone(&ran);
                unit(move || {
                    ran.lock().unwrap().push(index);
                    if index == 0 {
                        return Err(TestError::Value("unit-0"));
                    }
                    Ok(index)
                })
            })
            .collect();
        run_ordered(
            |u| {
                let task = pool.submit(u);
                futures.lock().unwrap().push(task.clone());
                task
            },
            units,
            None,
        )
    });
    assert_eq!(result, Err(TestError::Value("unit-0")));
    assert_eq!(*ran.lock().unwrap(), vec![0]);
    for task in &futures.lock().unwrap()[1..] {
        let cancelled = task.cancelled();
        let observed_stop = task.peek(|outcome| {
            matches!(
                outcome,
                Some(drmr_core::concurrency::Outcome::Returned(Err(
                    TestError::Cancelled
                )))
            )
        });
        assert!(cancelled || observed_stop, "{task:?}");
    }
}

#[test]
fn a_running_unit_waiting_on_the_callers_stop_flag_is_released_when_another_unit_fails() {
    // Both units pass a barrier, the observer at the top of its body: it is then past the start
    // gate and about to wait before the failing unit can set the flag. Without it the pool may
    // pop the observer after the flag is set and skip its body (`run_gated`), and nothing is
    // pushed.
    let stop = Stop::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let both_running = Arc::new(std::sync::Barrier::new(2));
    let result = std::thread::scope(|scope| {
        let pool = Pool::new(scope, 2);
        let (observer_stop, observer_seen, observer_barrier) =
            (stop.clone(), Arc::clone(&seen), Arc::clone(&both_running));
        let observer = unit(move || {
            observer_barrier.wait();
            observer_seen
                .lock()
                .unwrap()
                .push(observer_stop.wait(Some(Duration::from_secs(5))));
            Err(TestError::Cancelled)
        });
        let failing_barrier = Arc::clone(&both_running);
        let failing: TestUnit<'_, ()> = unit(move || {
            failing_barrier.wait();
            Err(TestError::Runtime("boom"))
        });
        run_ordered(|u| pool.submit(u), vec![observer, failing], Some(&stop))
    });
    assert_eq!(result, Err(TestError::Runtime("boom")));
    assert_eq!(*seen.lock().unwrap(), vec![true]);
}

#[test]
fn a_panicking_unit_is_a_failure_resumed_on_the_caller_in_submission_order() {
    // Python's BaseException: the unit sets the flag and is re-raised on the caller's thread.
    // Both units pass a barrier before failing: under load unit-1 could otherwise fail and set
    // the flag before unit-0 started, and the earliest failure would be unit-1's error.
    let stop = Stop::new();
    let both_started = std::sync::Barrier::new(2);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        std::thread::scope(|scope| {
            let pool = Pool::new(scope, 2);
            let units: Vec<TestUnit<'_, ()>> = vec![
                unit(|| {
                    both_started.wait();
                    panic!("unit-0 panicked")
                }),
                unit(|| {
                    both_started.wait();
                    Err(TestError::Value("unit-1"))
                }),
            ];
            run_ordered(|u| pool.submit(u), units, Some(&stop))
        })
    }));
    let payload = outcome.expect_err("the earliest failure is the panic");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"unit-0 panicked"));
    assert!(stop.is_set());
}

#[test]
fn cancelled_is_never_chosen_while_a_real_failure_exists_and_wins_only_alone() {
    let result = std::thread::scope(|scope| {
        let pool = Pool::new(scope, 1);
        let units: Vec<TestUnit<'_, ()>> = vec![unit(|| Err(TestError::Cancelled))];
        run_ordered(|u| pool.submit(u), units, None)
    });
    assert_eq!(
        result,
        Err(TestError::Cancelled),
        "only cancelled units: the first is raised"
    );
    let _ = Cancelled; // the crate's own `Cancelled` error, used by the source module's units
}

#[test]
fn a_stop_wait_times_out_unset_and_returns_at_once_once_set() {
    let stop = Stop::new();
    assert!(!stop.wait(Some(Duration::from_millis(20))));
    stop.set();
    assert!(stop.wait(None));
    assert!(stop.wait(Some(Duration::ZERO)));
}
