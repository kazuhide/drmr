//! Observe real metadata and comparison work without depending on elapsed-time speedups.
use super::*;
use crate::{
    concurrency::Stop,
    events::{Collector, Record, Sink},
    resources::{Claim, Hardware, Limiter},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

struct Overlap {
    tool: &'static str,
    started: AtomicUsize,
    second_entered: mpsc::SyncSender<()>,
    observed: Mutex<mpsc::Receiver<()>>,
    overlapped: AtomicBool,
    records: Collector,
}
impl Overlap {
    fn new(tool: &'static str) -> Self {
        let (second_entered, observed) = mpsc::sync_channel(1);
        Self {
            tool,
            started: AtomicUsize::new(0),
            second_entered,
            observed: Mutex::new(observed),
            overlapped: AtomicBool::new(false),
            records: Collector::default(),
        }
    }
}
impl Sink for Overlap {
    fn emit(&self, record: &Record) {
        self.records.emit(record);
        if record.get("event").and_then(serde_json::Value::as_str) != Some("command")
            || record.get("tool").and_then(serde_json::Value::as_str) != Some(self.tool)
        {
            return;
        }
        // Arguments remain redacted. One task cannot issue its next synchronous
        // tool until this call returns, so the second start proves an overlap.
        let ordinal = self.started.fetch_add(1, Ordering::AcqRel);
        if ordinal == 0 {
            let overlapped = self
                .observed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .is_ok();
            self.overlapped.store(overlapped, Ordering::Release);
        } else if ordinal == 1 {
            self.second_entered.send(()).unwrap();
        }
    }
}

fn converter(sink: Arc<dyn Sink>, cpus: usize) -> (Converter, Arc<Limiter>) {
    let limiter = Arc::new(Limiter::new(cpus, 4 * 1024 * 1024 * 1024));
    let runner = ToolRunner::new(Arc::new(Secrets::new()), sink).with_limiter(limiter.clone());
    let mut converter = Converter::new(Arc::new(runner), "packager", "ffmpeg")
        .with_decode(true)
        .with_max_workers(Some(cpus))
        .unwrap()
        .with_timeout(Duration::from_secs(10));
    converter.backend.scheduling_hardware = Some(Hardware::from_observations(
        8,
        Some(8 * 1024 * 1024 * 1024),
        None,
    ));
    (converter, limiter)
}

fn video(directory: &Path) -> PathBuf {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let source = directory.join("video.mp4");
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-copyts", "-i"])
        .arg(&fixture)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
            "-movflags",
            "+frag_keyframe+empty_moov",
            "-use_editlist",
            "0",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(output.status.success(), "video fixture creation failed");
    source
}

fn prepared(directory: &Path, fixture: &Path) -> Vec<Prepared> {
    (0..2)
        .map(|ordinal| {
            let directory = directory.join(format!("comparison-{ordinal:04}"));
            super::super::files::create_dir(&directory).unwrap();
            let source = directory.join("source.mp4");
            let media = directory.join("media.mp4");
            copy_private(fixture, &source).unwrap();
            copy_private(fixture, &media).unwrap();
            let mut source = prepare(&source, 0, TrackKind::Video, &InputKeys::default()).unwrap();
            source.media = media;
            source
        })
        .collect()
}

#[test]
fn snapshot_profiles_overlap_and_normalization_keeps_track_order_and_payloads() {
    let root = crate::native::private_tempdir().unwrap();
    let source = video(root.path());
    let original = fs::read(&source).unwrap();
    let tracks = (0..2)
        .map(|_| LocalTrack {
            source: source.clone(),
            kind: TrackKind::Video,
            index: 0,
        })
        .collect::<Vec<_>>();
    let observed = Arc::new(Overlap::new("ffprobe"));
    let (command, _) = converter(observed.clone(), 2);
    let keys = InputKeys::default();
    let (prepared, clear) = command
        .normalize_tracks(
            &TrackConversionRequest {
                tracks: &tracks,
                output: &root.path().join("unused"),
                format: OutputFormat::Dash,
                target: TargetProtection::Clear,
                input_keys: &keys,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            },
            root.path(),
            None,
        )
        .unwrap();
    assert!(
        observed.overlapped.load(Ordering::Acquire),
        "the second snapshot profile must start before the first profile continues"
    );
    for ordinal in 0..2 {
        assert_eq!(
            prepared[ordinal].source,
            root.path()
                .join(format!("normalize-{ordinal:04}/source.mp4"))
        );
        let verification = command
            .backend
            .verify_media(&source, &clear[ordinal], None, None)
            .unwrap();
        assert!(verification.passed && verification.checks.is_pass());
        assert!(verification.checks.packet_payloads_equal);
        assert!(verification.checks.packet_timing_equal);
    }
    assert_eq!(fs::read(source).unwrap(), original);
}

#[test]
fn original_to_intermediate_checks_overlap_and_keep_track_order() {
    let root = crate::native::private_tempdir().unwrap();
    let source = video(root.path());
    let prepared = prepared(root.path(), &source);
    let observed = Arc::new(Overlap::new("ffmpeg"));
    let (command, _) = converter(observed.clone(), 2);
    let mut tracks = Vec::new();
    command
        .verify_source_tracks(&prepared, &mut tracks, &InputKeys::default())
        .unwrap();
    assert!(
        observed.overlapped.load(Ordering::Acquire),
        "the second intermediate comparison must start before the first continues"
    );
    assert_eq!(tracks.len(), 2);
    for (track, expected) in tracks.iter().zip(&prepared) {
        assert_eq!(track.source, expected.media);
        assert_eq!(track.kind, TrackKind::Video);
        assert_eq!(track.index, 0);
    }
}

#[test]
fn vp9_review_distinct_source_views_allow_modeled_tail_without_relaxing_outputs() {
    let root = crate::native::private_tempdir().unwrap();
    let seed =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/vp9-p0-8.mp4");
    let source = root.path().join("tail.mp4");
    super::super::native_media::vp9_binding_tests::prepare_tail(&seed, &source);
    let original = fs::read(&source).unwrap();
    let media = root.path().join("source-view.mp4");
    fs::write(&media, &original).unwrap();
    let keys = InputKeys::new(vec![
        super::super::ContentKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap(),
    ])
    .unwrap();
    let mut prepared = prepare(&source, 0, TrackKind::Video, &keys).unwrap();
    prepared.media = media.clone();
    let observed = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), observed.clone()));
    let command = Converter::new(runner, "absent-packager", "absent-ffmpeg")
        .with_ffprobe("absent-ffprobe")
        .with_decode(false);
    let mut tracks = Vec::new();
    command
        .verify_source_tracks(&[prepared], &mut tracks, &keys)
        .unwrap();
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].source, media);
    assert_eq!(
        command
            .backend
            .verify_views(
                StreamView::new(&source, 0, None).with_all_keys(&keys),
                StreamView::new(&media, 0, None).with_all_keys(&keys)
            )
            .unwrap_err(),
        super::super::native_media::VP9_LAYOUT
    );
    assert!(
        observed
            .records()
            .iter()
            .all(|event| event.get("event").and_then(serde_json::Value::as_str) != Some("command"))
    );
    assert_eq!(fs::read(source).unwrap(), original);
    assert_eq!(fs::read(media).unwrap(), original);
}

#[test]
fn capacity_one_source_checks_release_admission_for_nested_media_work() {
    let root = crate::native::private_tempdir().unwrap();
    let source = video(root.path());
    let prepared = prepared(root.path(), &source);
    let (command, limiter) = converter(Arc::new(crate::events::NoSink), 1);
    let command = command.with_decode(false);
    let mut tracks = Vec::new();
    command
        .verify_source_tracks(&prepared, &mut tracks, &InputKeys::default())
        .unwrap();
    assert_eq!(tracks.len(), 2);
    drop(command);
    drop(
        limiter
            .acquire(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: 4 * 1024 * 1024 * 1024,
                },
                Some(&Stop::new()),
                Some(Duration::from_secs(1)),
            )
            .unwrap(),
    );
}

#[test]
fn failed_intermediate_check_drains_started_media_work_without_poisoning_cancellation() {
    let root = crate::native::private_tempdir().unwrap();
    let source = video(root.path());
    let prepared = prepared(root.path(), &source);
    let mut changed = fs::read(&prepared[1].media).unwrap();
    let kind = changed
        .windows(4)
        .position(|bytes| bytes == b"mdat")
        .unwrap();
    let start = kind.checked_sub(4).unwrap();
    let size =
        usize::try_from(u32::from_be_bytes(changed[start..kind].try_into().unwrap())).unwrap();
    assert!(size > 16 && start + size <= changed.len());
    let end = start + size;
    let mut offset = kind + 4;
    let mut mutated = false;
    while offset + 5 <= end {
        let length = usize::try_from(u32::from_be_bytes(
            changed[offset..offset + 4].try_into().unwrap(),
        ))
        .unwrap();
        assert!(length > 0 && offset + 4 + length <= end);
        if matches!(changed[offset + 4] & 0x1f, 1 | 5) && length > 8 {
            // Change coded picture data, rather than an ignored delimiter or padding byte.
            changed[offset + 4 + length / 2] ^= 1;
            mutated = true;
            break;
        }
        offset += 4 + length;
    }
    assert!(mutated);
    fs::write(&prepared[1].media, changed).unwrap();
    let observed = Arc::new(Overlap::new("ffmpeg"));
    let stop = Stop::new();
    let (command, _) = converter(observed.clone(), 2);
    let command = command.with_stop(stop.clone());
    let mut tracks = Vec::new();
    assert!(
        command
            .verify_source_tracks(&prepared, &mut tracks, &InputKeys::default())
            .is_err()
    );
    assert!(!stop.is_set());
    let records = observed.records.records();
    let started = records.iter().filter(|record| {
        record.get("event").and_then(serde_json::Value::as_str) == Some("command")
    });
    for command in started {
        assert!(
            records.iter().any(|record| {
                record.get("event").and_then(serde_json::Value::as_str) == Some("tool-output")
                    && record.get("call_id") == command.get("call_id")
            }),
            "a source check returned before its started child completed"
        );
    }
    let mut retry = Vec::new();
    command
        .verify_source_tracks(&prepared[..1], &mut retry, &InputKeys::default())
        .unwrap();
    assert_eq!(retry.len(), 1);
}

#[test]
fn native_normalization_preserves_payloads_and_clocks_with_one_or_two_workers() {
    let root = crate::native::private_tempdir().unwrap();
    let source = video(root.path());
    let original = fs::read(&source).unwrap();
    let tracks = (0..2)
        .map(|_| LocalTrack {
            source: source.clone(),
            kind: TrackKind::Video,
            index: 0,
        })
        .collect::<Vec<_>>();
    let keys = InputKeys::default();
    for workers in [1, 2] {
        let work = root.path().join(format!("workers-{workers}"));
        super::super::files::create_dir(&work).unwrap();
        let records = Arc::new(Collector::default());
        let (command, _) = converter(records.clone(), workers);
        let command = command.with_decode(false);
        let (prepared, clears) = command
            .normalize_tracks(
                &TrackConversionRequest {
                    tracks: &tracks,
                    output: &root.path().join("unused"),
                    format: OutputFormat::Dash,
                    target: TargetProtection::Clear,
                    input_keys: &keys,
                    output_key: None,
                    hls_key_uri: None,
                    hls_output_key: None,
                },
                &work,
                None,
            )
            .unwrap();
        for (ordinal, clear) in clears.iter().enumerate() {
            assert_eq!(
                prepared[ordinal].source,
                work.join(format!("normalize-{ordinal:04}/source.mp4"))
            );
            let verified = command
                .backend
                .verify_media(&source, clear, None, None)
                .unwrap();
            assert!(verified.passed && verified.checks.is_pass());
            assert!(verified.checks.packet_payloads_equal && verified.checks.packet_timing_equal);
        }
        assert!(records.records().iter().all(|record| {
            !matches!(
                record.get("tool").and_then(serde_json::Value::as_str),
                Some("ffmpeg" | "ffprobe")
            )
        }));
    }
    assert_eq!(fs::read(source).unwrap(), original);
}

#[test]
fn unknown_snapshot_profiles_keep_the_conservative_memory_refusal() {
    let root = crate::native::private_tempdir().unwrap();
    let source = video(root.path());
    let original = fs::read(&source).unwrap();
    let limiter = Arc::new(Limiter::new(2, 128 * 1024 * 1024));
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(crate::events::NoSink))
        .with_limiter(limiter);
    let command = Converter::new(Arc::new(runner), "packager", "unused-decoder")
        .with_max_workers(Some(2))
        .unwrap();
    let keys = InputKeys::default();
    let tracks = [LocalTrack {
        source: source.clone(),
        kind: TrackKind::Video,
        index: 0,
    }];
    let result = command.normalize_tracks(
        &TrackConversionRequest {
            tracks: &tracks,
            output: &root.path().join("unused"),
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        },
        root.path(),
        None,
    );
    assert_eq!(result.err().unwrap(), ConversionError("resource-budget"));
    assert!(!root.path().join("normalize-0000").exists());
    assert_eq!(fs::read(source).unwrap(), original);
}
