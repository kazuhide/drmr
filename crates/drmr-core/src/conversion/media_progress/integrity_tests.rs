use super::*;
use crate::events::Collector;

#[test]
fn integrity_counts_actual_bytes_and_files_without_per_chunk_emissions() {
    let sink = Arc::new(Collector::default());
    let batch = Integrity::new(Some(sink.clone()), 10, 2).unwrap();
    let now = Instant::now();
    batch.advance(2, 0, now);
    batch.advance(1, 1, now);
    assert_eq!(sink.records().len(), 1);
    batch.advance(7, 0, now + Duration::from_millis(251));
    assert_eq!(sink.records().len(), 2);
    assert_eq!(sink.records()[1]["completed_bytes"], 10);
    assert_eq!(sink.records()[1]["completed_units"], 1);
    batch.completed_file();
    batch.finish();
    let records = sink.records();
    assert_eq!(records.last().unwrap()["state"], "finished");
    assert_eq!(records.last().unwrap()["completed_units"], 2);
    for record in records {
        assert_eq!(record["task"], "integrity");
        assert_eq!(record["unit"], "files");
        assert_eq!(record["total_bytes"], 10);
        assert_eq!(record["total_units"], 2);
        assert_eq!(record["estimated"], false);
        assert_eq!(record.len(), 11);
    }
}

#[test]
fn integrity_is_bounded_and_cannot_finish_incomplete_or_emit_after_completion() {
    let sink = Arc::new(Collector::default());
    assert!(Integrity::new(None, 10, 1).is_none());
    assert!(Integrity::new(Some(sink.clone()), 0, 1).is_none());
    assert!(Integrity::new(Some(sink.clone()), 10, 0).is_none());
    assert!(Integrity::new(Some(sink.clone()), 10, 100_131).is_none());
    let batch = Integrity::new(Some(sink.clone()), 10, 1).unwrap();
    batch.read(5);
    batch.finish();
    drop(batch);
    assert_eq!(sink.records().last().unwrap()["state"], "aborted");
    let batch = Integrity::new(Some(sink.clone()), 10, 1).unwrap();
    batch.read(10);
    batch.completed_file();
    batch.finish();
    let finished = sink.records();
    batch.read(10);
    batch.completed_file();
    batch.finish();
    drop(batch);
    assert_eq!(sink.records(), finished);
}

#[test]
fn concurrent_integrity_counters_are_monotone_and_share_the_global_batch_sequence() {
    let sink = Arc::new(Collector::default());
    let batch = Integrity::new(Some(sink.clone()), 2_000, 100).unwrap();
    std::thread::scope(|scope| {
        for _ in 0..100 {
            let batch = &batch;
            scope.spawn(move || {
                batch.read(20);
                batch.completed_file();
            });
        }
    });
    batch.finish();
    let records = sink.records();
    for pair in records.windows(2) {
        assert!(pair[0]["completed_bytes"].as_u64() <= pair[1]["completed_bytes"].as_u64());
        assert!(pair[0]["completed_units"].as_u64() <= pair[1]["completed_units"].as_u64());
    }
    assert_eq!(records.last().unwrap()["completed_bytes"], 2_000);
    assert_eq!(records.last().unwrap()["completed_units"], 100);
    let comparison = Batch::comparison(Some(sink.clone()), vec![1]).unwrap();
    assert_ne!(
        sink.records().last().unwrap()["batch_id"],
        records[0]["batch_id"]
    );
    drop(comparison);
}

#[test]
fn running_integrity_sink_panic_closes_observer_authority() {
    struct Failing(Mutex<Vec<Record>>);
    impl Sink for Failing {
        fn emit(&self, record: &Record) {
            self.0.lock().unwrap().push(record.clone());
            assert_eq!(record["state"], "started", "synthetic observer panic");
        }
    }
    let sink = Arc::new(Failing(Mutex::default()));
    let batch = Integrity::new(Some(sink.clone()), 10, 1).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        batch.advance(5, 0, Instant::now() + Duration::from_millis(251));
    }));
    assert!(result.is_err());
    batch.read(5);
    drop(batch);
    assert_eq!(sink.0.lock().unwrap().len(), 2);
}
