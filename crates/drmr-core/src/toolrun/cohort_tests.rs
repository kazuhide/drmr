use super::*;
use crate::resources::ReservationError;
use std::sync::mpsc;

fn runner() -> ToolRunner {
    ToolRunner::new(Arc::new(Secrets::new()), Arc::new(crate::events::NoSink))
        .with_limiter(Arc::new(Limiter::new(4, 4096)))
}

fn claim(threads: usize) -> Claim {
    Claim {
        cpu_threads: threads,
        memory_bytes: 128,
    }
}

#[test]
fn independent_one_and_three_cpu_cohorts_use_all_four_shared_tokens() {
    let base = runner();
    let one = base.cohort(1);
    let three = base.cohort(3);
    let held = one
        .native_permit(claim(1), &Stop::new(), Duration::from_secs(1))
        .unwrap();
    let stopped = Stop::new();
    let (entered, observed) = mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            entered.send(()).unwrap();
            one.native_permit(claim(1), &stopped, Duration::from_secs(1))
        });
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        let other = three
            .native_permit(claim(3), &Stop::new(), Duration::from_secs(1))
            .unwrap();
        assert_eq!(base.native_capacity(), (4, 4096));
        assert_eq!(one.native_capacity(), (1, 4096));
        assert_eq!(three.native_capacity(), (3, 4096));
        stopped.set();
        assert_eq!(
            waiter.join().unwrap().unwrap_err(),
            ReservationError::Cancelled
        );
        drop(other);
    });
    drop(held);
    drop(
        one.native_permit(claim(1), &Stop::new(), Duration::from_secs(1))
            .unwrap(),
    );
    drop(
        base.native_permit(claim(4), &Stop::new(), Duration::from_secs(1))
            .unwrap(),
    );
}

#[test]
fn runner_forks_inherit_scope_and_narrower_nested_scope_also_charges_its_parent() {
    let base = runner();
    let parent = base.cohort(3);
    let observed = parent.observed(Arc::new(crate::events::NoSink));
    let (cleanup, _owner) = parent.ensuring_cleanup_owner();
    let wider = parent.cohort(4);
    for runner in [&observed, &cleanup, &wider] {
        assert!(Arc::ptr_eq(
            parent.cpu_scope.as_ref().unwrap(),
            runner.cpu_scope.as_ref().unwrap()
        ));
    }
    let narrow = parent.cohort(1);
    let narrow_held = narrow
        .native_permit(claim(1), &Stop::new(), Duration::from_secs(1))
        .unwrap();
    let parent_held = parent
        .native_permit(claim(2), &Stop::new(), Duration::from_secs(1))
        .unwrap();
    let independent = base.cohort(1);
    let other = independent
        .native_permit(claim(1), &Stop::new(), Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        parent
            .native_permit(claim(1), &Stop::new(), Duration::from_millis(20))
            .unwrap_err(),
        ReservationError::Timeout
    );
    assert_eq!(
        narrow
            .native_permit(claim(2), &Stop::new(), Duration::from_millis(20))
            .unwrap_err(),
        ReservationError::Insufficient
    );
    drop((other, parent_held, narrow_held));
    drop(
        parent
            .native_permit(claim(3), &Stop::new(), Duration::from_secs(1))
            .unwrap(),
    );
}

#[test]
fn impossible_memory_refuses_before_cpu_wait_and_unwinding_returns_both_tickets() {
    let base = runner();
    let scoped = base.cohort(1);
    let stop = Stop::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = scoped
            .native_permit(claim(1), &stop, Duration::from_secs(1))
            .unwrap();
        assert_eq!(
            scoped
                .native_permit(
                    Claim {
                        cpu_threads: 1,
                        memory_bytes: 4097
                    },
                    &stop,
                    Duration::from_secs(1)
                )
                .unwrap_err(),
            ReservationError::Insufficient
        );
        panic!("synthetic cohort panic");
    }));
    assert!(result.is_err());
    drop(
        scoped
            .native_permit(claim(1), &stop, Duration::from_secs(1))
            .unwrap(),
    );
    drop(
        base.native_permit(claim(4), &stop, Duration::from_secs(1))
            .unwrap(),
    );
}

#[test]
fn a_shared_admission_timeout_does_not_retain_a_cohort_ticket() {
    let base = runner();
    let scoped = base.cohort(1);
    let stop = Stop::new();
    let held = base
        .native_permit(claim(4), &stop, Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        scoped
            .native_permit(claim(1), &stop, Duration::from_millis(20))
            .unwrap_err(),
        ReservationError::Timeout
    );
    drop(held);
    drop(
        scoped
            .native_permit(claim(1), &stop, Duration::from_secs(1))
            .unwrap(),
    );
}
