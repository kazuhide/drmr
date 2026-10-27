use super::*;
use crate::{
    events::{Collector, NoSink, Record, Sink},
    toolrun::{Secrets, ToolRunner},
};
use std::sync::atomic::{AtomicBool, Ordering};

fn source(root: &Path) -> PathBuf {
    std::fs::write(root.join("segment.ts"), vec![7; 128 * 1024]).unwrap();
    let path = root.join("input.m3u8");
    std::fs::write(
        &path,
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    path
}

fn command(sink: Arc<dyn Sink>) -> super::super::Converter {
    super::super::Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "unused-packager",
        "unused-decoder",
    )
    .with_activity_sink(sink)
}

#[test]
fn retained_guard_reports_actual_inventory_reads_and_quiet_clones_emit_nothing() {
    let root = tempfile::tempdir().unwrap();
    let input = source(root.path());
    let sink = Arc::new(Collector::default());
    let frozen = command(sink.clone()).capture_source(&input).unwrap();
    let before = sink.records().len();
    frozen.check_original().unwrap();
    let records = sink.records()[before..].to_vec();
    assert_eq!(records.first().unwrap()["task"], "integrity");
    assert_eq!(records.first().unwrap()["state"], "started");
    assert_eq!(records.last().unwrap()["state"], "finished");
    let expected: u64 = frozen.inventory().files.iter().map(|file| file.bytes).sum();
    assert_eq!(records.last().unwrap()["completed_bytes"], expected);
    assert_eq!(
        records.last().unwrap()["completed_units"],
        frozen.inventory().files.len()
    );
    assert_eq!(
        records.last().unwrap()["total_units"],
        frozen.inventory().files.len()
    );
    let text = serde_json::to_string(&records).unwrap();
    assert!(!text.contains(&root.path().to_string_lossy().to_string()));
    assert!(!text.contains("segment.ts") && !text.contains("input.m3u8"));
    let before_quiet = sink.records();
    frozen.check_original_quiet().unwrap();
    assert_eq!(sink.records(), before_quiet);
    frozen.check_original().unwrap();
    assert!(sink.records().len() > before_quiet.len());
}

#[test]
fn integrity_observer_cancellation_cannot_produce_a_finished_guard_batch() {
    struct Cancelling {
        stop: Stop,
        records: Collector,
        armed: AtomicBool,
    }
    impl Sink for Cancelling {
        fn emit(&self, record: &Record) {
            self.records.emit(record);
            if self.armed.load(Ordering::Acquire)
                && record.get("task").and_then(|value| value.as_str()) == Some("integrity")
                && record["state"] == "started"
            {
                self.stop.set();
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let input = source(root.path());
    let stop = Stop::new();
    let sink = Arc::new(Cancelling {
        stop: stop.clone(),
        records: Collector::default(),
        armed: AtomicBool::new(false),
    });
    let frozen = command(sink.clone())
        .with_stop(stop)
        .capture_source(&input)
        .unwrap();
    let before = sink.records.records().len();
    sink.armed.store(true, Ordering::Release);
    assert_eq!(
        frozen.check_original().unwrap_err(),
        ConversionError("cancelled")
    );
    let records = sink.records.records()[before..].to_vec();
    let records: Vec<_> = records
        .iter()
        .filter(|record| record.get("task").is_some_and(|task| task == "integrity"))
        .collect();
    assert_eq!(records.last().unwrap()["state"], "aborted");
    assert!(!records.iter().any(|record| record["state"] == "finished"));
}

#[test]
fn integrity_observer_mutation_is_still_rejected_by_actual_source_hashes() {
    struct Mutating {
        path: PathBuf,
        records: Collector,
        armed: AtomicBool,
    }
    impl Sink for Mutating {
        fn emit(&self, record: &Record) {
            self.records.emit(record);
            if self.armed.load(Ordering::Acquire)
                && record.get("task").and_then(|value| value.as_str()) == Some("integrity")
                && record["state"] == "started"
            {
                std::fs::write(&self.path, vec![8; 128 * 1024]).unwrap();
            }
        }
    }
    let root = tempfile::tempdir().unwrap();
    let input = source(root.path());
    let sink = Arc::new(Mutating {
        path: root.path().join("segment.ts"),
        records: Collector::default(),
        armed: AtomicBool::new(false),
    });
    let frozen = command(sink.clone()).capture_source(&input).unwrap();
    let before = sink.records.records().len();
    sink.armed.store(true, Ordering::Release);
    assert_eq!(frozen.check_original().unwrap_err(), CHANGED);
    let records = sink.records.records()[before..].to_vec();
    let records: Vec<_> = records
        .iter()
        .filter(|record| record.get("task").is_some_and(|task| task == "integrity"))
        .collect();
    assert_eq!(records.last().unwrap()["state"], "aborted");
    assert!(!records.iter().any(|record| record["state"] == "finished"));
}
