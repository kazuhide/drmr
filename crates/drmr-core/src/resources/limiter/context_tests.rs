use super::*;
use std::sync::mpsc;

#[test]
fn retained_context_leaves_cpu_one_and_reduces_only_the_leaf_memory_ceiling() {
    let limiter = Arc::new(Limiter::new(1, 512));
    let stop = Stop::new();
    let retained = limiter
        .retain(128, 256, &stop, Duration::from_secs(1))
        .unwrap();
    assert_eq!(limiter.capacity(), (1, 384));
    // A linter can plan against this ceiling even while the parent retains state.
    let child = limiter
        .acquire(
            Claim {
                cpu_threads: 1,
                memory_bytes: limiter.capacity().1,
            },
            Some(&stop),
            Some(Duration::from_millis(100)),
        )
        .unwrap();
    assert_eq!(limiter.capacity(), (1, 384));
    drop(child);
    drop(retained);
    assert_eq!(limiter.capacity(), (1, 512));
    assert_eq!(
        limiter
            .acquire(
                Claim {
                    cpu_threads: 0,
                    memory_bytes: 1
                },
                None,
                None
            )
            .unwrap_err(),
        ReservationError::Insufficient
    );
}

#[test]
fn impossible_context_and_leaf_claims_refuse_without_waiting_for_the_parent() {
    let limiter = Arc::new(Limiter::new(1, 512));
    let stop = Stop::new();
    assert!(matches!(
        limiter.retain(257, 256, &stop, Duration::from_secs(1)),
        Err(ReservationError::Insufficient)
    ));
    assert!(matches!(
        limiter.retain(u64::MAX, 1, &stop, Duration::from_secs(1)),
        Err(ReservationError::Insufficient)
    ));
    let _retained = limiter
        .retain(128, 256, &stop, Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        limiter
            .acquire(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: 385
                },
                None,
                Some(Duration::from_secs(1))
            )
            .unwrap_err(),
        ReservationError::Insufficient
    );
}

#[test]
fn cancellation_and_panic_release_every_retained_byte_and_notify_waiters() {
    let limiter = Arc::new(Limiter::new(1, 512));
    let stopped = Stop::new();
    stopped.set();
    assert!(matches!(
        limiter.retain(128, 256, &stopped, Duration::from_secs(1)),
        Err(ReservationError::Cancelled)
    ));
    let stop = Stop::new();
    let retained = limiter
        .retain(128, 256, &stop, Duration::from_secs(1))
        .unwrap();
    let (entered, observed) = mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| {
            entered.send(()).unwrap();
            limiter
                .retain(200, 256, &stop, Duration::from_secs(1))
                .unwrap()
        });
        observed.recv_timeout(Duration::from_secs(1)).unwrap();
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _retained = retained;
            panic!("synthetic retained-context panic");
        }));
        assert!(unwind.is_err());
        drop(waiter.join().unwrap());
    });
    assert_eq!(limiter.capacity(), (1, 512));
    assert_eq!(lock(&limiter.used).leaf_floor, 0);
}

#[test]
fn concurrent_parents_preserve_the_greatest_leaf_floor() {
    let limiter = Arc::new(Limiter::new(1, 512));
    let stop = Stop::new();
    let first = limiter
        .retain(128, 256, &stop, Duration::from_secs(1))
        .unwrap();
    assert!(matches!(
        limiter.retain(129, 1, &stop, Duration::from_millis(30)),
        Err(ReservationError::Timeout)
    ));
    let second = limiter
        .retain(128, 1, &stop, Duration::from_secs(1))
        .unwrap();
    assert_eq!(limiter.capacity(), (1, 256));
    let leaf = limiter
        .acquire(
            Claim {
                cpu_threads: 1,
                memory_bytes: 256,
            },
            Some(&stop),
            Some(Duration::from_secs(1)),
        )
        .unwrap();
    drop(leaf);
    drop(first);
    assert_eq!(limiter.capacity(), (1, 384));
    drop(second);
    assert_eq!(limiter.capacity(), (1, 512));
    assert_eq!(lock(&limiter.used).leaf_floor, 0);
}
