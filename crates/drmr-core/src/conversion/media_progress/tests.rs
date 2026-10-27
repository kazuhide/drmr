use super::*;
use crate::events::{Collector, Record, Sink};
use std::{io::Write as _, sync::Arc};

fn events(collector: &Arc<Collector>) -> Vec<Record> {
    collector.records()
}

#[test]
fn weighted_comparison_counts_completed_pairs_once_and_finishes_explicitly() {
    let sink = Arc::new(Collector::default());
    let batch = Batch::comparison(Some(sink.clone()), vec![2, 8]).unwrap();
    batch.completed(1);
    batch.completed(1);
    batch.completed(usize::MAX);
    batch.completed(0);
    batch.finish();
    drop(batch);
    let records = events(&sink);
    assert_eq!(records.len(), 4);
    assert_eq!(records[0]["state"], "started");
    assert_eq!(records[1]["completed_bytes"], 8);
    assert_eq!(records[1]["completed_units"], 1);
    assert_eq!(records[2]["completed_bytes"], 10);
    assert_eq!(records[3]["state"], "finished");
    for record in &records {
        assert_eq!(record["stage"], "media-progress");
        assert_eq!(record["task"], "comparison");
        assert_eq!(record["total_bytes"], 10);
        assert_eq!(record["total_units"], 2);
        assert_eq!(record["estimated"], false);
        assert!(record["batch_id"].as_u64().unwrap() > 0);
        assert_eq!(record["batch_id"], records[0]["batch_id"]);
    }
}

#[test]
fn concurrent_comparison_snapshots_are_monotone_and_ids_are_distinct() {
    let sink = Arc::new(Collector::default());
    let batch = Batch::comparison(Some(sink.clone()), vec![1; 32]).unwrap();
    std::thread::scope(|scope| {
        for index in 0..32 {
            let batch = &batch;
            scope.spawn(move || batch.completed(index));
        }
    });
    batch.finish();
    let records = events(&sink);
    for pair in records.windows(2) {
        assert!(pair[0]["completed_bytes"].as_u64() <= pair[1]["completed_bytes"].as_u64());
    }
    let next = Batch::comparison(Some(sink.clone()), vec![1]).unwrap();
    assert_ne!(
        events(&sink).last().unwrap()["batch_id"],
        records[0]["batch_id"]
    );
    drop(next);
}

#[test]
fn incomplete_or_dropped_work_is_aborted_instead_of_finished() {
    let sink = Arc::new(Collector::default());
    let batch = Batch::comparison(Some(sink.clone()), vec![2, 8]).unwrap();
    batch.completed(0);
    batch.finish();
    drop(batch);
    let records = events(&sink);
    assert_eq!(records.last().unwrap()["state"], "aborted");
    assert!(!records.iter().any(|record| record["state"] == "finished"));
    assert!(Batch::comparison(Some(sink.clone()), vec![]).is_none());
    assert!(Batch::comparison(Some(sink.clone()), vec![0]).is_none());
    assert!(Batch::comparison(Some(sink.clone()), vec![u64::MAX, 1]).is_none());
    assert!(Batch::comparison(Some(sink), vec![1; 65]).is_none());
}

#[test]
fn packaging_keeps_raw_observations_even_when_the_estimate_is_exceeded() {
    let sink = Arc::new(Collector::default());
    let batch = Batch::packaging(Some(sink.clone()), 10).unwrap();
    batch.observed(15);
    batch.observed(3);
    batch.finish();
    drop(batch);
    let records = events(&sink);
    assert_eq!(records.len(), 3);
    assert_eq!(records[1]["completed_bytes"], 15);
    assert_eq!(records[2]["completed_bytes"], 15);
    assert_eq!(records[2]["state"], "finished");
    assert_eq!(records[2]["total_bytes"], 10);
    assert_eq!(records[2]["estimated"], true);
    assert!(!records[2].contains_key("completed_units"));
}

#[test]
fn terminal_observer_panic_never_causes_a_second_terminal_record() {
    struct Failing(std::sync::Mutex<Vec<Record>>);
    impl Sink for Failing {
        fn emit(&self, record: &Record) {
            self.0.lock().unwrap().push(record.clone());
            assert_ne!(record["state"], "finished", "synthetic observer panic");
        }
    }
    let sink = Arc::new(Failing(std::sync::Mutex::new(Vec::new())));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let batch = Batch::packaging(Some(sink.clone()), 10).unwrap();
        batch.finish();
    }));
    assert!(result.is_err());
    let records = sink.0.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1]["state"], "finished");
}

#[test]
fn a_running_observer_panic_cannot_emit_again_after_cleanup_reports_an_error() {
    struct Failing(std::sync::Mutex<Vec<Record>>);
    impl Sink for Failing {
        fn emit(&self, record: &Record) {
            self.0.lock().unwrap().push(record.clone());
            assert_eq!(record["state"], "started", "synthetic observer panic");
        }
    }
    let sink = Arc::new(Failing(std::sync::Mutex::new(Vec::new())));
    let batch = Batch::packaging(Some(sink.clone()), 10).unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        batch.observed(5);
    }));
    assert!(result.is_err());
    // The process-control path may return cleanup-unconfirmed instead of
    // resuming the panic. Ordinary Drop must not call that observer again.
    drop(batch);
    assert_eq!(sink.0.lock().unwrap().len(), 2);
}

#[test]
fn packaging_sampler_observes_growing_files_without_locking_the_writer() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("track-0000");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(directory.join("init.mp4"), [0; 3]).unwrap();
    let mut writer = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(directory.join("seg_1.m4s"))
        .unwrap();
    writer.try_lock().unwrap();
    writer.write_all(&[0; 5]).unwrap();
    writer.flush().unwrap();
    let mut sampler = Sampler::new(root.path(), 1).unwrap();
    assert_eq!(sampler.sample(), 8);
    writer.write_all(&[0; 7]).unwrap();
    writer.flush().unwrap();
    assert_eq!(sampler.sample(), 15);
    std::fs::write(directory.join("seg_2.m4s"), [0; 4]).unwrap();
    assert_eq!(sampler.sample(), 19);
    writer.write_all(&[0; 1]).unwrap();
}

#[test]
fn sampling_is_bounded_independent_of_how_many_segments_exist() {
    let mut sampler = Sampler::new(Path::new("unused"), 64).unwrap();
    let mut calls = 0;
    sampler.sample_with(|path| {
        calls += 1;
        if path.extension().is_none() {
            Entry::Directory
        } else {
            Entry::File(5)
        }
    });
    assert!(calls <= MAX_PROBES);
    assert!(calls > 0);
    assert!(Sampler::new(Path::new("unused"), 65).is_none());
}

#[test]
fn a_sampler_time_budget_stops_before_consuming_its_probe_allowance() {
    let mut sampler = Sampler::new(Path::new("unused"), 64).unwrap();
    let mut calls = 0;
    let mut ticks = 0;
    sampler.sample_with_clock(
        |path| {
            calls += 1;
            if path.extension().is_none() {
                Entry::Directory
            } else {
                Entry::File(5)
            }
        },
        || {
            ticks += 1;
            std::time::Duration::from_millis(ticks * 2)
        },
    );
    assert!(calls > 0 && calls < MAX_PROBES);
    assert!(calls <= 8);
}

#[test]
fn three_probe_allowances_cover_seven_hundred_completed_segments_without_a_walk() {
    let mut sampler = Sampler::new(Path::new("unused-staging"), 7).unwrap();
    let mut calls = 0;
    for _ in 0..3 {
        sampler.sample_with_clock(
            |path| {
                calls += 1;
                let name = path.file_name().unwrap().to_str().unwrap();
                if name.starts_with("track-") {
                    Entry::Directory
                } else if name == "init.mp4" {
                    Entry::File(3)
                } else if name
                    .strip_prefix("seg_")
                    .and_then(|value| value.strip_suffix(".m4s"))
                    .and_then(|value| value.parse::<u32>().ok())
                    .is_some_and(|number| number <= 100)
                {
                    Entry::File(5)
                } else {
                    Entry::Missing
                }
            },
            || Duration::ZERO,
        );
    }
    assert!(calls <= 3 * MAX_PROBES);
    assert_eq!(
        sampler.tracks.iter().map(Cursor::bytes).sum::<u64>(),
        7 * (3 + 100 * 5)
    );
}

fn observed_backend(sink: Arc<dyn Sink>) -> super::super::backend::Backend {
    super::super::Converter::new(
        Arc::new(crate::toolrun::ToolRunner::new(
            Arc::new(crate::toolrun::Secrets::new()),
            Arc::new(crate::events::NoSink),
        )),
        "private-unused-tool",
        "private-unused-decoder",
    )
    .with_activity_sink(sink)
    .backend
}

#[test]
fn preparation_counts_read_units_but_finishes_only_after_its_outer_native_tail() {
    let sink = Arc::new(Collector::default());
    let backend = observed_backend(sink.clone());
    let (backend, scope) = scoped(&backend, Subject::Result, Purpose::Admission);
    let context = backend.preparation().unwrap();
    context.begin(2, Unit::Renditions);
    context.completed(0);
    context.completed(1);
    context.completed(1);
    let records = sink.records();
    assert_eq!(records.len(), 3);
    assert_eq!(records[2]["state"], "running");
    assert_eq!(records[2]["completed_units"], 2);
    assert_eq!(records[2]["total_units"], 2);
    scope.finish();
    assert_eq!(sink.records().last().unwrap()["state"], "finished");
    for record in sink.records() {
        assert_eq!(record["task"], "preparation");
        assert_eq!(record["subject"], "result");
        assert_eq!(record["purpose"], "admission");
        assert_eq!(record["unit"], "renditions");
        assert_eq!(record["estimated"], false);
        assert!(!record.contains_key("completed_bytes"));
        assert!(!record.contains_key("total_bytes"));
    }
}

#[test]
fn quiet_backend_clones_cannot_reuse_the_preparation_observer_authority() {
    let sink = Arc::new(Collector::default());
    let backend = observed_backend(sink.clone());
    let (mut quiet, scope) = scoped(&backend, Subject::Source, Purpose::Conversion);
    quiet.activity = None;
    assert!(quiet.preparation().is_none());
    scope.finish();
    assert_eq!(sink.records().len(), 0);
}

#[test]
fn finished_and_aborted_preparation_scopes_reject_late_retained_contexts() {
    for finish in [true, false] {
        let sink = Arc::new(Collector::default());
        let backend = observed_backend(sink.clone());
        let (backend, scope) = scoped(&backend, Subject::Output, Purpose::Conversion);
        let context = backend.preparation().unwrap().clone();
        context.begin(1, Unit::Tracks);
        context.completed(0);
        if finish {
            scope.finish();
        } else {
            drop(scope);
        }
        let records = sink.records();
        assert_eq!(
            records.last().unwrap()["state"],
            if finish { "finished" } else { "aborted" }
        );
        context.begin(2, Unit::Tracks);
        context.completed(0);
        context.completed(1);
        assert_eq!(sink.records(), records);
        let text = serde_json::to_string(&records).unwrap();
        assert!(!text.contains("private-unused-tool") && !text.contains("private-unused-decoder"));
    }
}

#[test]
fn unwound_preparation_scopes_silence_started_and_unstarted_retained_contexts() {
    for started in [false, true] {
        let sink = Arc::new(Collector::default());
        let backend = observed_backend(sink.clone());
        let (backend, scope) = scoped(&backend, Subject::Result, Purpose::Verification);
        let context = backend.preparation().unwrap().clone();
        if started {
            context.begin(1, Unit::Tracks);
        }
        let before = sink.records();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _scope = scope;
            panic!("synthetic native preparation unwind");
        }));
        assert!(panic.is_err());
        assert_eq!(sink.records(), before);
        context.begin(1, Unit::Tracks);
        context.completed(0);
        assert_eq!(sink.records(), before);
    }
}

#[cfg(unix)]
#[test]
fn sampling_skips_symlinks_nonregular_files_and_unrelated_names() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("track-0000");
    std::fs::create_dir(&directory).unwrap();
    let outside = root.path().join("operator-private");
    std::fs::write(&outside, [0; 50]).unwrap();
    std::os::unix::fs::symlink(&outside, directory.join("init.mp4")).unwrap();
    std::fs::create_dir(directory.join("seg_1.m4s")).unwrap();
    std::fs::write(directory.join("unrelated.m4s"), [0; 100]).unwrap();
    let mut sampler = Sampler::new(root.path(), 1).unwrap();
    assert_eq!(sampler.sample(), 0);
    std::fs::remove_dir(directory.join("seg_1.m4s")).unwrap();
    std::os::unix::fs::symlink(&outside, directory.join("seg_1.m4s")).unwrap();
    assert_eq!(sampler.sample(), 0);
}
