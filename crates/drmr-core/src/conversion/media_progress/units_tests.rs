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

fn assert_unit_states(record: &Record, expected: &[&str]) {
    assert_eq!(
        record.get("unit_states"),
        Some(&serde_json::json!(expected))
    );
    for (field, token) in [
        ("active_units", "active"),
        ("waiting_units", "waiting"),
        ("completed_units", "completed"),
        ("stopped_units", "stopped"),
    ] {
        assert_eq!(
            record[field],
            expected.iter().filter(|state| **state == token).count()
        );
    }
    assert_eq!(record["total_units"], expected.len());
}

#[test]
fn unit_state_snapshots_identify_out_of_order_tasks_and_keep_aggregate_counts() {
    let sink = Arc::new(Collector::default());
    let batch = unit_batch(&sink, 4).unwrap();
    assert_unit_states(
        sink.records().last().unwrap(),
        &["waiting", "waiting", "waiting", "waiting"],
    );
    let later = batch.start(2).unwrap();
    assert_unit_states(
        sink.records().last().unwrap(),
        &["waiting", "waiting", "active", "waiting"],
    );
    let first = batch.start(0).unwrap();
    assert_unit_states(
        sink.records().last().unwrap(),
        &["active", "waiting", "active", "waiting"],
    );
    later.finish();
    assert_unit_states(
        sink.records().last().unwrap(),
        &["active", "waiting", "completed", "waiting"],
    );
    drop(first);
    assert_unit_states(
        sink.records().last().unwrap(),
        &["stopped", "waiting", "completed", "waiting"],
    );
    drop(batch);
    let records = sink.records();
    let last = records.last().unwrap();
    assert_eq!(last["state"], "aborted");
    assert_unit_states(last, &["stopped", "waiting", "completed", "waiting"]);
}

#[test]
fn unit_state_finished_snapshot_has_every_ordinal_completed() {
    let sink = Arc::new(Collector::default());
    let batch = unit_batch(&sink, 3).unwrap();
    let third = batch.start(2).unwrap();
    let first = batch.start(0).unwrap();
    batch.start(1).unwrap().finish();
    first.finish();
    third.finish();
    batch.finish();
    let before = sink.records();
    let last = before.last().unwrap();
    assert_eq!(last["state"], "finished");
    assert_unit_states(last, &["completed", "completed", "completed"]);
    drop(batch);
    assert_eq!(sink.records(), before);
}

#[test]
fn unit_state_maximum_ordinal_and_closed_task_keep_the_same_bounded_snapshot() {
    let sink = Arc::new(Collector::default());
    let batch = unit_batch(&sink, 64).unwrap();
    let mut expected = ["waiting"; 64];
    assert_unit_states(sink.records().last().unwrap(), &expected);
    let highest = batch.start(63).unwrap();
    expected[63] = "active";
    assert_unit_states(sink.records().last().unwrap(), &expected);
    highest.finish();
    expected[63] = "completed";
    assert_unit_states(sink.records().last().unwrap(), &expected);
    let retained = batch.start(0).unwrap();
    drop(batch);
    expected[0] = "stopped";
    let before = sink.records();
    assert_unit_states(before.last().unwrap(), &expected);
    assert_eq!(before.last().unwrap()["state"], "aborted");
    retained.finish();
    assert_eq!(sink.records(), before);
}

#[test]
fn unit_description_snapshots_are_aligned_and_immutable_without_starting_tasks() {
    use crate::inspection::TrackKind;
    let sink = Arc::new(Collector::default());
    let batch = unit_batch(&sink, 3).unwrap();
    assert_eq!(
        sink.records().last().unwrap().get("unit_descriptions"),
        Some(&serde_json::json!([null, null, null]))
    );
    assert!(batch.describe(1, TrackKind::Video, Some((1920, 1080))));
    let before = sink.records();
    let last = before.last().unwrap();
    assert_eq!(last["state"], "running");
    assert_eq!(last["started_units"], 0);
    assert_unit_states(last, &["waiting", "waiting", "waiting"]);
    assert_eq!(
        last.get("unit_descriptions"),
        Some(&serde_json::json!([
            null,
            {"kind":"video", "resolution":[1920,1080]},
            null
        ]))
    );
    assert!(!batch.describe(1, TrackKind::Audio, None));
    assert!(!batch.describe(1, TrackKind::Video, Some((1920, 1080))));
    assert!(!batch.describe(3, TrackKind::Audio, None));
    assert!(!batch.describe(0, TrackKind::Unknown, None));
    assert_eq!(sink.records(), before);
}

#[test]
fn unit_description_task_labels_preserve_completed_and_closed_ordinal_authority() {
    use crate::inspection::TrackKind;
    let sink = Arc::new(Collector::default());
    let batch = unit_batch(&sink, 3).unwrap();
    batch.start(0).unwrap().finish();
    assert!(!batch.describe(0, TrackKind::Video, Some((1920, 1080))));
    let active = batch.start(1).unwrap();
    assert!(active.describe(TrackKind::Audio, None));
    let stopped = batch.start(2).unwrap();
    drop(stopped);
    assert!(!batch.describe(2, TrackKind::Text, None));
    assert_eq!(
        sink.records().last().unwrap().get("unit_descriptions"),
        Some(&serde_json::json!([null, {"kind":"audio"}, null]))
    );
    drop(batch);
    let before = sink.records();
    assert!(!active.describe(TrackKind::Video, Some((1280, 720))));
    active.finish();
    assert_eq!(sink.records(), before);
}

#[test]
fn unit_description_resolution_is_optional_bounded_and_video_only() {
    use crate::inspection::TrackKind;
    for (kind, resolution, expected) in [
        (TrackKind::Video, None, serde_json::json!({"kind":"video"})),
        (
            TrackKind::Video,
            Some((0, 1080)),
            serde_json::json!({"kind":"video"}),
        ),
        (
            TrackKind::Video,
            Some((1920, 0)),
            serde_json::json!({"kind":"video"}),
        ),
        (
            TrackKind::Video,
            Some((u64::from(u32::MAX) + 1, 1080)),
            serde_json::json!({"kind":"video"}),
        ),
        (
            TrackKind::Video,
            Some((u64::from(u32::MAX), 1)),
            serde_json::json!({"kind":"video", "resolution":[u32::MAX,1]}),
        ),
        (
            TrackKind::Audio,
            Some((1920, 1080)),
            serde_json::json!({"kind":"audio"}),
        ),
        (
            TrackKind::Text,
            Some((1920, 1080)),
            serde_json::json!({"kind":"text"}),
        ),
    ] {
        let sink = Arc::new(Collector::default());
        let batch = unit_batch(&sink, 1).unwrap();
        assert!(batch.describe(0, kind, resolution));
        assert_eq!(
            sink.records().last().unwrap().get("unit_descriptions"),
            Some(&serde_json::json!([expected]))
        );
    }
}

#[test]
fn unit_description_observer_panic_cannot_repeat_from_task_or_owner_cleanup() {
    use crate::inspection::TrackKind;
    struct Failing {
        records: Mutex<Vec<Record>>,
    }
    impl Sink for Failing {
        fn emit(&self, record: &Record) {
            let mut records = self.records.lock().unwrap_or_else(PoisonError::into_inner);
            records.push(record.clone());
            assert_ne!(
                records.len(),
                3,
                "synthetic descriptor observer interruption"
            );
        }
    }
    let sink = Arc::new(Failing {
        records: Mutex::default(),
    });
    let batch = Batch::units(
        Some(sink.clone()),
        Operation::Snapshot,
        Subject::Source,
        Purpose::Conversion,
        Unit::Tracks,
        2,
    )
    .unwrap();
    let task = batch.start(0).unwrap();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        task.describe(TrackKind::Video, Some((1920, 1080)))
    }));
    assert!(panic.is_err());
    assert!(!batch.describe(1, TrackKind::Audio, None));
    assert!(batch.start(1).is_none());
    drop(task);
    drop(batch);
    assert_eq!(
        sink.records
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len(),
        3
    );
}
