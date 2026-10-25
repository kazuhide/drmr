use super::*;
use std::sync::mpsc;

#[test]
fn a_partial_spawn_panic_closes_the_queue_and_drains_actual_started_workers() {
    for workers_started in [0, 1, 3] {
        let (entered, started) = mpsc::channel();
        let (finished, drained) = mpsc::channel();
        let mut attempts = 0;
        let mut rescue = None;
        let (outcome, closed, drained_without_rescue) = std::thread::scope(|scope| {
            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                Pool::spawn_with(4, |shared| {
                    attempts += 1;
                    rescue.get_or_insert_with(|| Arc::clone(&shared));
                    assert!(attempts <= workers_started, "synthetic spawn failure");
                    let entered = entered.clone();
                    let finished = finished.clone();
                    scope.spawn(move || {
                        entered.send(()).unwrap();
                        work(&shared);
                        finished.send(()).unwrap();
                    });
                    // The injected failure follows actual entry into every prior worker.
                    started.recv_timeout(Duration::from_secs(2)).unwrap();
                })
            }));
            let shared = rescue.as_ref().unwrap();
            let closed = lock(&shared.0).closed;
            let mut completed = 0;
            for _ in 0..workers_started {
                if drained.recv_timeout(Duration::from_secs(2)).is_err() {
                    break;
                }
                completed += 1;
            }
            // A regression must fail finitely rather than strand scope's joins.
            // Record both observations before rescuing the intentionally broken case.
            lock(&shared.0).closed = true;
            shared.1.notify_all();
            (outcome, closed, completed)
        });
        let payload = outcome.expect_err("the injected spawn panic propagates");
        assert_eq!(
            payload.downcast_ref::<&str>(),
            Some(&"synthetic spawn failure")
        );
        assert_eq!(attempts, workers_started + 1);
        assert!(closed, "a partially constructed pool must close its queue");
        assert_eq!(drained_without_rescue, workers_started);
    }
}
