use super::*;
use crate::{
    concurrency::Stop,
    conversion::{Converter, InputKeys, workspace},
    resources::Limiter,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

fn source(root: &Path) -> PathBuf {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let created = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture)
        .args([
            "-map",
            "0:v:0",
            "-t",
            "4",
            "-c",
            "copy",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(root.join("segment-%02d.ts"))
        .arg(root.join("first.m3u8"))
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "short transport fixture creation failed"
    );
    fs::copy(root.join("first.m3u8"), root.join("second.m3u8")).unwrap();
    let master = root.join("master.m3u8");
    fs::write(&master, "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=100000,CODECS=\"avc1.64001f\"\nfirst.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=200000,CODECS=\"avc1.64001f\"\nsecond.m3u8\n").unwrap();
    master
}

#[test]
fn native_renditions_keep_every_sample_clock_and_metadata_across_worker_counts() {
    let owner = crate::native::private_tempdir().unwrap();
    let input = source(owner.path());
    let original = fs::read(owner.path().join("segment-00.ts")).unwrap();
    let keys = InputKeys::default();
    let hls_keys = super::super::super::HlsInputKeys::default();
    let input = VerificationSource {
        input: &input,
        input_keys: &keys,
        hls_keys: &hls_keys,
    };
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(crate::events::NoSink))
        .with_limiter(Arc::new(Limiter::new(4, 4 * 1024 * 1024 * 1024)));
    let converter = Converter::new(Arc::new(runner), "missing-packager", "missing-ffmpeg")
        .with_ffprobe("missing-ffprobe")
        .with_stop(Stop::new());
    let serial_stage = workspace().unwrap();
    let parallel_stage = workspace().unwrap();
    let serial = prepare(
        &input,
        serial_stage.path(),
        &converter.backend,
        Some(1),
        SourceValidationPurpose::Verification,
        false,
    )
    .unwrap();
    let parallel = prepare(
        &input,
        parallel_stage.path(),
        &converter.backend,
        Some(2),
        SourceValidationPurpose::Verification,
        false,
    )
    .unwrap();
    assert_eq!(serial.len(), 2);
    assert_eq!(serial.len(), parallel.len());
    for (a, b) in serial.iter().zip(&parallel) {
        same_evidence(a, b);
        assert_eq!(fs::read(&a.source).unwrap(), fs::read(&b.source).unwrap());
    }
    assert_eq!(
        fs::read(owner.path().join("segment-00.ts")).unwrap(),
        original
    );
}

fn same_evidence(a: &Prepared, b: &Prepared) {
    assert_eq!(a.metadata, b.metadata);
    assert_eq!(a.outer_protection, b.outer_protection);
    assert_eq!(a.compatibility, b.compatibility);
    assert!(a.clock == b.clock);
    assert_eq!(a.audio_presentation, b.audio_presentation);
    assert_eq!(a.transport_index, b.transport_index);
    assert_eq!(
        serde_json::to_value(&a.report).unwrap(),
        serde_json::to_value(&b.report).unwrap()
    );
    let a = a.transport_native.as_ref().unwrap();
    let b = b.transport_native.as_ref().unwrap();
    assert!(a.segment_timings().unwrap().len() >= 2);
    assert_eq!(a.metadata().unwrap(), b.metadata().unwrap());
    assert_eq!(a.timing().unwrap(), b.timing().unwrap());
    assert_eq!(a.evidence().unwrap(), b.evidence().unwrap());
    assert_eq!(a.segment_timings().unwrap(), b.segment_timings().unwrap());
}

#[test]
fn decoded_renditions_keep_original_evidence_and_observations_after_the_scope_exits() {
    let owner = crate::native::private_tempdir().unwrap();
    let input = source(owner.path());
    let original: Vec<_> = fs::read_dir(owner.path())
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    let keys = InputKeys::default();
    let hls_keys = super::super::super::HlsInputKeys::default();
    let input = VerificationSource {
        input: &input,
        input_keys: &keys,
        hls_keys: &hls_keys,
    };
    let observer = Arc::new(crate::events::Collector::default());
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(crate::events::NoSink))
        .with_limiter(Arc::new(Limiter::new(4, 4 * 1024 * 1024 * 1024)))
        .observed(observer.clone());
    let converter = Converter::new(Arc::new(runner), "packager", "ffmpeg").with_decode(true);
    let mut compared: Option<Vec<Prepared>> = None;
    let mut stages = Vec::new();
    for maximum in [1, 2] {
        let stage = workspace().unwrap();
        let prepared = prepare(
            &input,
            stage.path(),
            &converter.backend,
            Some(maximum),
            SourceValidationPurpose::Verification,
            false,
        )
        .unwrap();
        assert_eq!(prepared.len(), 2);
        if let Some(previous) = &compared {
            for (a, b) in previous.iter().zip(&prepared) {
                same_evidence(a, b);
            }
        }
        compared = Some(prepared);
        stages.push(stage);
        let workload = converter.backend.runner.registered_workload().unwrap();
        assert!(workload.max_pixels.unwrap() > 0 && workload.tracks >= 2);
    }
    assert!(
        observer
            .records()
            .iter()
            .any(|record| record["stage"] == "tool")
    );
    assert_eq!(converter.backend.runner.native_capacity().0, 4);
    for (path, bytes) in original {
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

struct ToolAdmissionGate {
    nested: AtomicBool,
    held: AtomicUsize,
    release: Stop,
}

impl crate::events::Sink for ToolAdmissionGate {
    fn emit(&self, record: &crate::events::Record) {
        if record.get("task").and_then(serde_json::Value::as_str) == Some("ts-preparation")
            && record.get("state").and_then(serde_json::Value::as_str) == Some("started")
        {
            self.nested.store(true, Ordering::Release);
        }
        if self.nested.load(Ordering::Acquire)
            && !self.release.is_set()
            && record.get("event").and_then(serde_json::Value::as_str) == Some("tool-output")
        {
            // ToolRunner emits this record while its actual subprocess reservation
            // is still held. Keep it live so a missing shared ceiling is observable.
            self.held.fetch_add(1, Ordering::Release);
            self.release.wait(Some(Duration::from_secs(5)));
        }
    }
}

#[test]
fn actual_decoded_preparation_caps_nested_tool_admission_across_renditions() {
    let owner = crate::native::private_tempdir().unwrap();
    let input = source(owner.path());
    let keys = InputKeys::default();
    let hls_keys = super::super::super::HlsInputKeys::default();
    let input = VerificationSource {
        input: &input,
        input_keys: &keys,
        hls_keys: &hls_keys,
    };
    let gate = Arc::new(ToolAdmissionGate {
        nested: AtomicBool::new(false),
        held: AtomicUsize::new(0),
        release: Stop::new(),
    });
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(crate::events::NoSink))
        .with_limiter(Arc::new(Limiter::new(4, 4 * 1024 * 1024 * 1024)))
        .observed(gate.clone());
    let mut converter = Converter::new(Arc::new(runner), "packager", "ffmpeg").with_decode(true);
    converter.backend.activity = Some(gate.clone());
    let stage = workspace().unwrap();
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            prepare(
                &input,
                stage.path(),
                &converter.backend,
                Some(2),
                SourceValidationPurpose::Verification,
                false,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        while gate.held.load(Ordering::Acquire) < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        std::thread::sleep(Duration::from_millis(350));
        let admitted = gate.held.load(Ordering::Acquire);
        gate.release.set();
        let prepared = worker.join().unwrap().unwrap();
        assert_eq!(
            admitted, 2,
            "actual nested HLS probes exceeded --max-workers=2"
        );
        assert_eq!(prepared.len(), 2);
        for track in &prepared {
            assert!(
                track
                    .transport_native
                    .as_ref()
                    .unwrap()
                    .segment_timings()
                    .unwrap()
                    .len()
                    >= 2
            );
        }
    });
    assert_eq!(converter.backend.runner.native_capacity().0, 4);
}
