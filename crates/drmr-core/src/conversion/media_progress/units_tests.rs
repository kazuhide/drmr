use super::*;
use crate::events::Collector;

fn unit_batch(sink: &Arc<Collector>, total: usize) -> Option<Units> {
    Batch::units(
        Some(sink.clone()),
        Operation::Snapshot,
        Subject::Source,
        Purpose::Conversion,
        Unit::Tracks,
        total,
    )
}

#[test]
fn unit_counts_follow_observed_starts_and_never_infer_running_from_remaining() {
    let sink = Arc::new(Collector::default());
    let batch = unit_batch(&sink, 4).unwrap();
    let a = batch.start(2).unwrap();
    let b = batch.start(0).unwrap();
    a.finish();
    drop(b);
    let records = sink.records();
    let record = records.last().unwrap();
    assert_eq!(record["started_units"], 2);
    assert_eq!(record["active_units"], 0);
    assert_eq!(record["waiting_units"], 2);
    assert_eq!(record["completed_units"], 1);
    assert_eq!(record["stopped_units"], 1);
    batch.finish();
    drop(batch);
    let records = sink.records();
    assert_eq!(records.last().unwrap()["state"], "aborted");
    assert!(!records.iter().any(|record| record["state"] == "finished"));
    for record in records {
        assert_eq!(record["stage"], "rendition-progress");
        assert_eq!(record["operation"], "snapshot");
        assert_eq!(record["subject"], "source");
        assert_eq!(record["purpose"], "conversion");
        assert_eq!(record["unit"], "tracks");
    }
}

#[test]
fn duplicate_and_out_of_range_starts_cannot_inflate_unit_counts() {
    let sink = Arc::new(Collector::default());
    let batch = unit_batch(&sink, 2).unwrap();
    let first = batch.start(0).unwrap();
    assert!(batch.start(0).is_none());
    assert!(batch.start(usize::MAX).is_none());
    first.finish();
    assert!(batch.start(0).is_none());
    batch.start(1).unwrap().finish();
    batch.finish();
    assert!(batch.start(1).is_none());
    let last = sink.records().last().unwrap().clone();
    assert_eq!(last["state"], "finished");
    assert_eq!(last["active_units"], 0);
    assert_eq!(last["waiting_units"], 0);
    assert_eq!(last["completed_units"], 2);
    assert!(unit_batch(&sink, 0).is_none());
    assert!(unit_batch(&sink, 65).is_none());
}

#[test]
fn simultaneous_units_report_real_overlap_and_unwinding_releases_active_units() {
    let sink = Arc::new(Collector::default());
    let batch = unit_batch(&sink, 2).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let batch = &batch;
        scope.spawn(move || {
            let task = batch.start(0).unwrap();
            started_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            task.finish();
        });
        started_rx.recv().unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _task = batch.start(1).unwrap();
            assert_eq!(sink.records().last().unwrap()["active_units"], 2);
            panic!("synthetic unit interruption");
        }));
        assert!(result.is_err());
        assert_eq!(sink.records().last().unwrap()["active_units"], 1);
        release_tx.send(()).unwrap();
    });
    drop(batch);
    let records = sink.records();
    let last = records.last().unwrap();
    assert_eq!(last["state"], "aborted");
    assert_eq!(last["active_units"], 0);
    assert_eq!(last["completed_units"], 1);
    assert_eq!(last["stopped_units"], 1);
}

#[test]
fn closing_unit_batch_stops_live_units_and_rejects_late_guard_completion() {
    let sink = Arc::new(Collector::default());
    let batch = unit_batch(&sink, 2).unwrap();
    let task = batch.start(0).unwrap();
    drop(batch);
    let before = sink.records();
    assert_eq!(before.last().unwrap()["active_units"], 0);
    assert_eq!(before.last().unwrap()["stopped_units"], 1);
    task.finish();
    assert_eq!(sink.records(), before);
    assert!(!serde_json::to_string(&before).unwrap().contains("private"));
}

#[test]
fn observer_panics_cannot_repeat_from_task_or_owner_cleanup() {
    struct Failing {
        records: Mutex<Vec<Record>>,
        at: usize,
    }
    impl Sink for Failing {
        fn emit(&self, record: &Record) {
            let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
            records.push(record.clone());
            assert_ne!(records.len(), self.at, "synthetic observer interruption");
        }
    }
    for at in [2, 3] {
        let sink = Arc::new(Failing {
            records: Mutex::default(),
            at,
        });
        let batch = Batch::units(
            Some(sink.clone()),
            Operation::Snapshot,
            Subject::Source,
            Purpose::Conversion,
            Unit::Tracks,
            1,
        )
        .unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            batch.start(0).unwrap().finish();
        }));
        assert!(panic.is_err());
        drop(batch);
        assert_eq!(
            sink.records
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len(),
            at
        );
    }
}
