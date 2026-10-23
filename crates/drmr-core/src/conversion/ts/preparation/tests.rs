use super::*;
use crate::events::Collector;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

fn batch(stop: &Stop, workers: usize, total: usize, sink: Arc<Collector>) -> Batch<'_> {
    Batch {
        stop,
        workers,
        progress: Progress::new(Some(sink), total),
    }
}

#[test]
fn collects_in_parallel_with_ordered_results_across_bounded_batches() {
    let stop = Stop::new();
    let sink = Arc::new(Collector::default());
    let total = BATCH + 5;
    let batch = batch(&stop, 3, total, sink.clone());
    let active = AtomicUsize::new(0);
    let maximum = AtomicUsize::new(0);
    let first_started = Stop::new();
    let first_peer = Stop::new();
    let completed = Mutex::new(Vec::new());
    let mut result = Vec::new();
    for start in (0..total).step_by(BATCH) {
        let count = BATCH.min(total - start);
        result.extend(
            batch
                .collect(count, |offset| {
                    let ordinal = start + offset;
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(current, Ordering::SeqCst);
                    if ordinal == 0 {
                        first_started.set();
                        if !first_peer.wait(Some(Duration::from_secs(2))) {
                            active.fetch_sub(1, Ordering::SeqCst);
                            return Err(ConversionError("test-parallel-start-timeout"));
                        }
                    } else {
                        if !first_started.wait(Some(Duration::from_secs(2))) {
                            active.fetch_sub(1, Ordering::SeqCst);
                            return Err(ConversionError("test-parallel-start-timeout"));
                        }
                        completed.lock().unwrap().push(ordinal);
                        first_peer.set();
                    }
                    if ordinal == 0 {
                        completed.lock().unwrap().push(ordinal);
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                    Ok(ordinal)
                })
                .unwrap(),
        );
    }
    assert_eq!(result, (0..total).collect::<Vec<_>>());
    assert_ne!(completed.lock().unwrap()[0], 0);
    assert!((2..=3).contains(&maximum.load(Ordering::SeqCst)));
    assert_eq!(active.load(Ordering::SeqCst), 0);
    batch.finish().unwrap();
    drop(batch);
    let records = sink.records();
    assert_eq!(records.first().unwrap()["state"], "started");
    assert_eq!(records.last().unwrap()["state"], "finished");
    assert_eq!(records.last().unwrap()["completed"], total);
}

#[test]
fn one_worker_runs_on_the_calling_thread() {
    let stop = Stop::new();
    let sink = Arc::new(Collector::default());
    let batch = batch(&stop, 1, 3, sink);
    let caller = std::thread::current().id();
    assert_eq!(
        batch
            .collect(3, |ordinal| {
                assert_eq!(std::thread::current().id(), caller);
                Ok(ordinal)
            })
            .unwrap(),
        [0, 1, 2],
    );
    batch.finish().unwrap();
}

#[test]
fn failure_stops_queued_work_drains_running_work_and_preserves_external_stop() {
    let stop = Stop::new();
    let sink = Arc::new(Collector::default());
    let release = Stop::new();
    let calls = AtomicUsize::new(0);
    let active = AtomicUsize::new(0);
    let batch = batch(&stop, 2, BATCH, sink.clone());
    let result = batch.collect(BATCH, |ordinal| {
        calls.fetch_add(1, Ordering::SeqCst);
        active.fetch_add(1, Ordering::SeqCst);
        if ordinal == 0 {
            release.wait(Some(Duration::from_secs(2)));
            active.fetch_sub(1, Ordering::SeqCst);
            return Ok(ordinal);
        }
        release.set();
        active.fetch_sub(1, Ordering::SeqCst);
        Err(ConversionError("test-segment-evidence-failed"))
    });
    assert_eq!(result.unwrap_err().0, "test-segment-evidence-failed");
    assert!(calls.load(Ordering::SeqCst) < BATCH);
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert!(!stop.is_set());
    drop(batch);
    let records = sink.records();
    assert_eq!(records.last().unwrap()["state"], "aborted");
    assert!(records.last().unwrap()["completed"].as_u64().unwrap() < BATCH as u64);
    assert!(records.iter().all(|record| record["state"] != "finished"));
}

#[test]
fn cancellation_drains_active_units_without_starting_queued_inspection() {
    let stop = Stop::new();
    let sink = Arc::new(Collector::default());
    let calls = AtomicUsize::new(0);
    let batch = batch(&stop, 2, BATCH, sink.clone());
    let (started, received) = mpsc::channel();
    std::thread::scope(|scope| {
        let running = scope.spawn(|| {
            batch.collect(BATCH, |_| {
                calls.fetch_add(1, Ordering::SeqCst);
                started.send(()).unwrap();
                stop.wait(Some(Duration::from_secs(2)));
                Err::<(), _>(ConversionError("cancelled"))
            })
        });
        let first = received.recv_timeout(Duration::from_secs(2));
        let second = received.recv_timeout(Duration::from_secs(2));
        stop.set();
        let result = running.join().unwrap();
        assert!(first.is_ok() && second.is_ok());
        assert_eq!(result.unwrap_err().0, "cancelled");
    });
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(batch.finish().unwrap_err().0, "cancelled");
    drop(batch);
    let last = sink.records().pop().unwrap();
    assert_eq!(last["state"], "aborted");
    assert_eq!(last["completed"], 0);
}

#[test]
fn a_preexisting_cancellation_does_not_inspect_or_complete_any_segment() {
    let stop = Stop::new();
    stop.set();
    let sink = Arc::new(Collector::default());
    let batch = batch(&stop, 4, 3, sink.clone());
    let calls = AtomicUsize::new(0);
    let result = batch.collect(3, |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert_eq!(result.unwrap_err().0, "cancelled");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    drop(batch);
    assert_eq!(sink.records().last().unwrap()["state"], "aborted");
}

#[test]
fn collection_total_is_not_success_before_ordered_boundary_validation() {
    let stop = Stop::new();
    let sink = Arc::new(Collector::default());
    let batch = batch(&stop, 2, 2, sink.clone());
    batch.collect(2, Ok).unwrap();
    // An error in the caller's ordered boundary checks drops the batch here.
    drop(batch);
    let records = sink.records();
    assert_eq!(records.last().unwrap()["state"], "aborted");
    assert_eq!(records.last().unwrap()["completed"], 2);
    assert!(records.iter().all(|record| record["state"] != "finished"));
}

#[test]
fn incomplete_collection_cannot_be_reported_as_finished() {
    let sink = Arc::new(Collector::default());
    let progress = Progress::new(Some(sink.clone()), 2);
    progress.completed();
    assert_eq!(
        progress.finish().unwrap_err().0,
        "ts-segment-evidence-incomplete"
    );
    drop(progress);
    assert_eq!(sink.records().last().unwrap()["state"], "aborted");
}

#[test]
fn parallel_progress_is_monotonic_numeric_and_source_free() {
    let sink = Arc::new(Collector::default());
    let progress = Progress::new(Some(sink.clone()), 200);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let progress = &progress;
            scope.spawn(move || {
                for _ in 0..50 {
                    progress.completed();
                }
            });
        }
    });
    progress.finish().unwrap();
    drop(progress);
    let records = sink.records();
    assert_eq!(records.len(), 202);
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record.len(), 6);
        assert_eq!(record["stage"], "analysis");
        assert_eq!(record["event"], "progress");
        assert_eq!(record["task"], "ts-preparation");
        assert_eq!(record["total"], 200);
        assert_eq!(record["completed"], index.min(200));
        assert_eq!(
            record["state"],
            match index {
                0 => "started",
                201 => "finished",
                _ => "running",
            }
        );
    }
}
