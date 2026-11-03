use super::*;
use crate::concurrency::Pool;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Cancelled,
    Real(usize),
}
impl UnitError for Failure {
    fn cancelled() -> Self {
        Self::Cancelled
    }
    fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}
type TestUnit<'a> = Unit<'a, usize, Failure>;
const WAIT: Duration = Duration::from_secs(5);

#[test]
fn a_blocked_first_unit_does_not_prevent_refill_beyond_the_initial_window() {
    let reached = Stop::new();
    let results = std::thread::scope(|scope| {
        let pool = Pool::new(scope, 2);
        let units = (0..MAX_PENDING_TASKS + 3).map(|index| {
            let reached = &reached;
            Box::new(move || {
                if index == 0 {
                    assert!(
                        reached.wait(Some(WAIT)),
                        "the ordered prefix stopped refill"
                    );
                } else if index == MAX_PENDING_TASKS {
                    reached.set();
                }
                Ok(index)
            }) as TestUnit<'_>
        });
        run_completion_order(|unit| pool.submit(unit), units, None).unwrap()
    });
    assert_eq!(results, (0..MAX_PENDING_TASKS + 3).collect::<Vec<_>>());
}

#[test]
fn a_full_window_keeps_the_producer_lazy_when_every_worker_is_busy() {
    let constructed = AtomicUsize::new(0);
    let full = Stop::new();
    let release = Stop::new();
    let (observed, results) = std::thread::scope(|scope| {
        let producer = scope.spawn(|| {
            let pool = Pool::new(scope, 2);
            let units = (0..MAX_PENDING_TASKS * 3).map(|index| {
                constructed.fetch_add(1, Ordering::SeqCst);
                if index + 1 == MAX_PENDING_TASKS {
                    full.set();
                }
                let release = &release;
                Box::new(move || {
                    if index < 2 {
                        assert!(release.wait(Some(WAIT)));
                    }
                    Ok(index)
                }) as TestUnit<'_>
            });
            run_completion_order(|unit| pool.submit(unit), units, None).unwrap()
        });
        assert!(full.wait(Some(WAIT)));
        std::thread::sleep(Duration::from_millis(50));
        let observed = constructed.load(Ordering::SeqCst);
        release.set();
        (observed, producer.join().unwrap())
    });
    assert_eq!(observed, MAX_PENDING_TASKS);
    assert_eq!(results, (0..MAX_PENDING_TASKS * 3).collect::<Vec<_>>());
}

#[test]
fn a_real_failure_wins_over_an_earlier_observer_and_drains_the_active_work() {
    let caller = Stop::new();
    let abort = Stop::new();
    let linked = Stop::linked(&caller, &abort);
    let started = Stop::new();
    let active = AtomicUsize::new(0);
    let ran = AtomicUsize::new(0);
    let result = std::thread::scope(|scope| {
        let pool = Pool::new(scope, 2);
        let units = (0..MAX_PENDING_TASKS * 3).map(|index| {
            let (linked, started, active, ran) = (&linked, &started, &active, &ran);
            Box::new(move || {
                ran.fetch_add(1, Ordering::SeqCst);
                if index == 0 {
                    active.fetch_add(1, Ordering::SeqCst);
                    started.set();
                    assert!(linked.wait(Some(WAIT)));
                    active.fetch_sub(1, Ordering::SeqCst);
                    return Err(Failure::Cancelled);
                }
                assert!(started.wait(Some(WAIT)));
                Err(Failure::Real(index))
            }) as TestUnit<'_>
        });
        run_completion_order(|unit| pool.submit(unit), units, Some(&abort))
    });
    assert_eq!(result, Err(Failure::Real(1)));
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(ran.load(Ordering::SeqCst), 2);
    assert!(abort.is_set());
    assert!(!caller.is_set());
}

#[test]
fn earlier_real_failures_win_even_when_a_later_unit_fails_first() {
    let first_started = Stop::new();
    let later_failed = Stop::new();
    let result = std::thread::scope(|scope| {
        let pool = Pool::new(scope, 2);
        let units = (0..2).map(|index| {
            let (first_started, later_failed) = (&first_started, &later_failed);
            Box::new(move || {
                if index == 0 {
                    first_started.set();
                    assert!(later_failed.wait(Some(WAIT)));
                } else {
                    assert!(first_started.wait(Some(WAIT)));
                    later_failed.set();
                }
                Err(Failure::Real(index))
            }) as TestUnit<'_>
        });
        run_completion_order(|unit| pool.submit(unit), units, None)
    });
    assert_eq!(result, Err(Failure::Real(0)));
}

#[test]
fn a_selected_panic_is_resumed_only_after_active_units_have_drained() {
    let first_started = Stop::new();
    let later_failed = Stop::new();
    let later_drained = Stop::new();
    let outcome = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        std::thread::scope(|scope| {
            let pool = Pool::new(scope, 2);
            let units = (0..2).map(|index| {
                let (first_started, later_failed, later_drained) =
                    (&first_started, &later_failed, &later_drained);
                Box::new(move || {
                    if index == 0 {
                        first_started.set();
                        assert!(later_failed.wait(Some(WAIT)));
                        panic!("first acquisition panic");
                    }
                    assert!(first_started.wait(Some(WAIT)));
                    later_failed.set();
                    later_drained.set();
                    Err(Failure::Real(index))
                }) as TestUnit<'_>
            });
            run_completion_order(|unit| pool.submit(unit), units, None)
        })
    }));
    assert!(later_drained.is_set());
    let payload = outcome.expect_err("the earlier submitted panic must be resumed");
    assert_eq!(
        payload.downcast_ref::<&str>(),
        Some(&"first acquisition panic")
    );
}

#[test]
fn external_cancellation_joins_work_and_releases_its_shared_resource_claim() {
    use crate::resources::{Claim, Limiter};
    let caller = Stop::new();
    let abort = Stop::new();
    let linked = Stop::linked(&caller, &abort);
    let started = Stop::new();
    let limiter = Arc::new(Limiter::new(2, 2 * 1024 * 1024));
    let result = std::thread::scope(|scope| {
        let pool = Pool::new(scope, 2);
        let units = (0..2).map(|index| {
            let (caller, linked, started, limiter) = (&caller, &linked, &started, &limiter);
            Box::new(move || {
                if index == 0 {
                    let _permit = limiter
                        .acquire(
                            Claim {
                                cpu_threads: 1,
                                memory_bytes: 1024 * 1024,
                            },
                            None,
                            Some(WAIT),
                        )
                        .unwrap();
                    started.set();
                    assert!(linked.wait(Some(WAIT)));
                } else {
                    assert!(started.wait(Some(WAIT)));
                    caller.set();
                }
                Err(Failure::Cancelled)
            }) as TestUnit<'_>
        });
        run_completion_order(|unit| pool.submit(unit), units, Some(&abort))
    });
    assert_eq!(result, Err(Failure::Cancelled));
    drop(
        limiter
            .acquire(
                Claim {
                    cpu_threads: 2,
                    memory_bytes: 2 * 1024 * 1024,
                },
                None,
                Some(WAIT),
            )
            .unwrap(),
    );
}
