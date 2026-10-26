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

#[test]
fn temporary_context_memory_waits_without_holding_shared_cpu_and_cancels_cleanly() {
    let base = runner();
    let scoped = base.cohort(1);
    let stop = Stop::new();
    let context = base
        .retain_context(1024, 2048, &Stop::new(), Duration::from_secs(1))
        .unwrap();
    assert_eq!(scoped.native_capacity(), (1, 4096));
    let (entered, observed) = mpsc::sync_channel(1);
    let (finished, done) = mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            entered.send(()).unwrap();
            let result = scoped.native_permit(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: 3073,
                },
                &stop,
                Duration::from_secs(2),
            );
            finished.send(result).unwrap();
        });
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        let premature = done.recv_timeout(Duration::from_millis(30));
        let independent = base
            .native_permit(claim(4), &Stop::new(), Duration::from_secs(1))
            .unwrap();
        stop.set();
        worker.join().unwrap();
        assert!(
            premature.is_err(),
            "temporary context occupancy was refused"
        );
        assert_eq!(
            done.recv().unwrap().unwrap_err(),
            ReservationError::Cancelled
        );
        drop(independent);
    });
    drop(context);
    drop(
        scoped
            .native_permit(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: 4096,
                },
                &Stop::new(),
                Duration::from_secs(1),
            )
            .unwrap(),
    );
}

#[test]
fn a_commands_own_memory_ceiling_refuses_impossible_children_and_survives_every_fork() {
    let base = runner();
    let scoped = base.cohort(2).within_context(1024, 2048);
    let observed = scoped.observed(Arc::new(crate::events::NoSink));
    let (cleanup, _owner) = scoped.ensuring_cleanup_owner();
    let nested = scoped.cohort(1);
    assert_eq!(base.native_capacity(), (4, 4096));
    for runner in [&scoped, &observed, &cleanup, &nested] {
        assert_eq!(runner.native_capacity().1, 3072);
        assert_eq!(runner.manifest_capacity().1, 2048);
        assert_eq!(
            runner
                .native_permit(
                    Claim {
                        cpu_threads: 1,
                        memory_bytes: 3073
                    },
                    &Stop::new(),
                    Duration::from_secs(1),
                )
                .unwrap_err(),
            ReservationError::Insufficient
        );
        drop(
            runner
                .native_permit(
                    Claim {
                        cpu_threads: 1,
                        memory_bytes: 3072,
                    },
                    &Stop::new(),
                    Duration::from_secs(1),
                )
                .unwrap(),
        );
    }
}

#[test]
fn cohort_workload_updates_reach_the_command_without_joining_independent_forks() {
    let base = runner();
    let initial = Workload {
        max_pixels: Some(1280 * 720),
        input_buffer_bytes: Some(64 * 1024),
        tracks: 1,
    };
    base.register_workload(initial);
    let observed = base.observed(Arc::new(crate::events::NoSink));
    let (cleanup, owner) = base.ensuring_cleanup_owner();
    let scoped = base.cohort(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            scoped.register_workload(Workload {
                max_pixels: Some(1920 * 1080),
                input_buffer_bytes: Some(128 * 1024),
                tracks: 4,
            });
        });
        scope.spawn(|| {
            scoped.register_workload(Workload {
                max_pixels: Some(0),
                input_buffer_bytes: Some(32 * 1024),
                tracks: 1,
            });
        });
    });
    let merged = base.registered_workload().unwrap();
    assert_eq!(merged.max_pixels, Some(1920 * 1080));
    assert_eq!(merged.input_buffer_bytes, Some(128 * 1024));
    assert_eq!(merged.tracks, 4);
    for independent in [&observed, &cleanup] {
        let unchanged = independent.registered_workload().unwrap();
        assert_eq!(unchanged.max_pixels, initial.max_pixels);
        assert_eq!(unchanged.input_buffer_bytes, initial.input_buffer_bytes);
        assert_eq!(unchanged.tracks, initial.tracks);
    }
    let cleanup_scope = cleanup.cohort(1);
    cleanup_scope.register_workload(Workload {
        max_pixels: Some(3840 * 2160),
        ..initial
    });
    assert_eq!(
        cleanup.registered_workload().unwrap().max_pixels,
        Some(3840 * 2160)
    );
    assert_eq!(
        base.registered_workload().unwrap().max_pixels,
        Some(1920 * 1080)
    );
    assert!(Arc::ptr_eq(
        cleanup_scope.cleanup_owner.as_ref().unwrap(),
        &owner
    ));
}
