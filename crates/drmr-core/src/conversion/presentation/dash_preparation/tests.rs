use super::*;
use crate::{
    concurrency::Stop,
    conversion::{Converter, HlsInputKeys, InputKeys, media_progress, verify::StreamView},
    events::Collector,
    resources::{Hardware, Limiter},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

fn command(cpus: usize, memory: u64, stop: Stop, records: Arc<Collector>) -> Converter {
    let runner = ToolRunner::new(Arc::new(Secrets::new()), records.clone())
        .with_limiter(Arc::new(Limiter::new(cpus, memory)));
    Converter::new(Arc::new(runner.cohort(cpus)), "packager", "unused-decoder")
        .with_activity_sink(records)
        .with_max_workers(Some(cpus))
        .unwrap()
        .with_stop(stop)
        .with_timeout(Duration::from_secs(5))
}

fn hardware() -> Hardware {
    Hardware::from_observations(8, Some(8 * 1024 * 1024 * 1024), None)
}

fn source(root: &Path) -> PathBuf {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    for role in ["video", "audio"] {
        crate::conversion::files::create_dir(&root.join(role)).unwrap();
    }
    let manifest = root.join("manifest.mpd");
    let mut packaging = std::process::Command::new("packager");
    for role in ["video", "audio"] {
        packaging.arg(format!(
            "in={},stream={role},init_segment={}/init.mp4,segment_template={}/seg_$Number$.m4s",
            clip.display(),
            root.join(role).display(),
            root.join(role).display()
        ));
    }
    let result = packaging
        .args([
            "--segment_duration",
            "2",
            "--fragment_duration",
            "2",
            "--generate_static_live_mpd",
            "--mp4_reset_initial_composition_offset_to_zero=false",
            "--single_threaded",
            "--io_cache_size=0",
            "--mpd_output",
        ])
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(result.status.success(), "DASH fixture creation failed");
    manifest
}

fn captured(manifest: &Path, stage: &Path) -> (Vec<LocalTrack>, Vec<metadata::Metadata>) {
    let tracks = crate::conversion::dash::snapshot_dash(manifest, stage).unwrap();
    let descriptions = metadata::read_dash(&stage.join("dash-manifest.mpd")).unwrap();
    assert_eq!(tracks.len(), 2);
    (tracks, descriptions)
}

fn input<'a>(path: &'a Path, keys: &'a InputKeys, hls: &'a HlsInputKeys) -> VerificationSource<'a> {
    VerificationSource {
        input: path,
        input_keys: keys,
        hls_keys: hls,
    }
}

fn hashes(root: &Path) -> Vec<(PathBuf, [u8; 32])> {
    fn visit(root: &Path, path: &Path, values: &mut Vec<(PathBuf, [u8; 32])>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(root, &path, values);
            } else {
                let mut hash = crate::sha256::Sha256::new();
                hash.update(fs::read(&path).unwrap());
                values.push((
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    hash.finalize(),
                ));
            }
        }
    }
    let mut values = Vec::new();
    visit(root, root, &mut values);
    values.sort();
    values
}

#[test]
fn real_dash_metadata_tasks_overlap_and_keep_representation_order() {
    let root = crate::native::private_tempdir().unwrap();
    let source_root = root.path().join("source");
    crate::conversion::files::create_dir(&source_root).unwrap();
    let manifest = source(&source_root);
    let stage = crate::conversion::files::Stage::new(Some(root.path())).unwrap();
    let (tracks, descriptions) = captured(&manifest, stage.path());
    let command = command(
        2,
        4 * 1024 * 1024 * 1024,
        Stop::new(),
        Arc::new(Collector::default()),
    );
    let scheduler = Scheduler::observed(&command.backend, Some(2), hardware());
    let (entered, observed) = mpsc::sync_channel(1);
    let observed = Mutex::new(observed);
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    let input = input(&manifest, &keys, &hls);
    let prepared = collect(&scheduler, &tracks, &descriptions, |ordinal| {
        if ordinal == 0 {
            observed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| ConversionError("dash-metadata-preparation-serialized"))?;
        } else {
            entered.send(()).unwrap();
        }
        prepare_track(
            ordinal,
            &tracks[ordinal],
            descriptions[ordinal].clone(),
            &input,
            stage.path(),
            &command.backend,
            SourceValidationPurpose::Verification,
        )
    })
    .unwrap();
    assert_eq!(
        prepared
            .iter()
            .map(|track| track.report.tracks[0].kind)
            .collect::<Vec<_>>(),
        [TrackKind::Video, TrackKind::Audio]
    );
    for (actual, expected) in prepared.iter().zip(&tracks) {
        assert_eq!(actual.source, expected.source);
        assert!(actual.dash_reference);
    }
}

#[test]
fn native_dash_preparation_reports_actual_tasks_and_preserves_media_for_worker_counts() {
    let root = crate::native::private_tempdir().unwrap();
    let source_root = root.path().join("source");
    crate::conversion::files::create_dir(&source_root).unwrap();
    let manifest = source(&source_root);
    let original = hashes(&source_root);
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    let input = input(&manifest, &keys, &hls);
    let mut previous = None::<Vec<Prepared>>;
    let mut stages = Vec::new();
    for workers in [1, 2] {
        let records = Arc::new(Collector::default());
        let command = command(
            workers,
            4 * 1024 * 1024 * 1024,
            Stop::new(),
            records.clone(),
        );
        let stage = crate::conversion::files::Stage::new(Some(root.path())).unwrap();
        let (backend, scope) = media_progress::scoped(
            &command.backend,
            media_progress::Subject::Result,
            media_progress::Purpose::Verification,
        );
        let prepared = prepare(
            &input,
            stage.path(),
            &backend,
            Some(workers),
            SourceValidationPurpose::Verification,
        )
        .unwrap();
        scope.finish();
        let observed = records.records();
        let terminal = observed
            .iter()
            .rev()
            .find(|record| {
                record.get("stage").and_then(serde_json::Value::as_str)
                    == Some("rendition-progress")
                    && record.get("operation").and_then(serde_json::Value::as_str)
                        == Some("preparation")
            })
            .expect("actual DASH metadata starts must be observable");
        assert_eq!(terminal["state"], "finished");
        assert_eq!(terminal["total_units"], 2);
        assert_eq!(terminal["started_units"], 2);
        assert_eq!(terminal["completed_units"], 2);
        assert_eq!(terminal["active_units"], 0);
        assert_eq!(terminal["stopped_units"], 0);
        assert_eq!(
            terminal.get("unit_descriptions"),
            Some(&serde_json::json!([{"kind":"video"}, {"kind":"audio"}]))
        );
        assert!(observed.iter().all(|record| !matches!(
            record.get("tool").and_then(serde_json::Value::as_str),
            Some("ffmpeg" | "ffprobe")
        )));
        if let Some(previous) = &previous {
            for (before, after) in previous.iter().zip(&prepared) {
                assert_eq!(before.metadata, after.metadata);
                let compared = command
                    .backend
                    .verify_views(
                        StreamView::new(&before.media, before.index, None).with_clock(before.clock),
                        StreamView::new(&after.media, after.index, None).with_clock(after.clock),
                    )
                    .unwrap();
                assert!(compared.passed && compared.checks.is_pass());
                assert!(
                    compared.checks.packet_payloads_equal && compared.checks.packet_timing_equal
                );
            }
        }
        previous = Some(prepared);
        stages.push(stage);
    }
    assert_eq!(hashes(&source_root), original);
}

#[test]
fn mismatched_dash_descriptions_are_rejected_before_scheduling() {
    let command = command(
        2,
        4 * 1024 * 1024 * 1024,
        Stop::new(),
        Arc::new(Collector::default()),
    );
    let scheduler = Scheduler::observed(&command.backend, Some(2), hardware());
    let tracks = [LocalTrack {
        source: PathBuf::from("unused.mp4"),
        index: 0,
        kind: TrackKind::Video,
    }];
    let called = AtomicBool::new(false);
    let result = collect(&scheduler, &tracks, &[], |_| {
        called.store(true, Ordering::Relaxed);
        Ok(())
    });
    assert_eq!(
        result.err().unwrap(),
        ConversionError("presentation-track-count-mismatch")
    );
    assert!(!called.load(Ordering::Relaxed));
}

#[test]
fn later_invalid_metadata_drains_started_work_and_leaves_cancellation_usable() {
    let root = crate::native::private_tempdir().unwrap();
    let source_root = root.path().join("source");
    crate::conversion::files::create_dir(&source_root).unwrap();
    let manifest = source(&source_root);
    let stage = crate::conversion::files::Stage::new(Some(root.path())).unwrap();
    let (mut tracks, descriptions) = captured(&manifest, stage.path());
    tracks[1].index = 63;
    let stop = Stop::new();
    let command = command(
        2,
        4 * 1024 * 1024 * 1024,
        stop.clone(),
        Arc::new(Collector::default()),
    );
    let scheduler = Scheduler::observed(&command.backend, Some(2), hardware());
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    let input = input(&manifest, &keys, &hls);
    let (failed, observed) = mpsc::sync_channel(1);
    let observed = Mutex::new(observed);
    let drained = AtomicBool::new(false);
    let result = collect(&scheduler, &tracks, &descriptions, |ordinal| {
        if ordinal == 0 {
            observed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| ConversionError("second-dash-metadata-not-started"))?;
        }
        let prepared = prepare_track(
            ordinal,
            &tracks[ordinal],
            descriptions[ordinal].clone(),
            &input,
            stage.path(),
            &command.backend,
            SourceValidationPurpose::Verification,
        );
        if ordinal == 1 {
            assert!(prepared.is_err());
            failed.send(()).unwrap();
        } else {
            assert!(prepared.is_ok());
            drained.store(true, Ordering::Release);
        }
        prepared
    });
    assert_eq!(
        result.err().unwrap(),
        ConversionError("stream-selector-mismatch")
    );
    assert!(drained.load(Ordering::Acquire));
    assert!(!stop.is_set());
    assert_eq!(
        collect(&scheduler, &tracks[..1], &descriptions[..1], |_| Ok(7)).unwrap(),
        [7]
    );
}

#[test]
fn unknown_memory_and_cancellation_refuse_before_metadata_work_starts() {
    for memory in [128 * 1024 * 1024, 4 * 1024 * 1024 * 1024] {
        let stop = Stop::new();
        if memory > 128 * 1024 * 1024 {
            stop.set();
        }
        let command = command(1, memory, stop, Arc::new(Collector::default()));
        let scheduler = Scheduler::observed(&command.backend, Some(2), hardware());
        let tracks = [LocalTrack {
            source: PathBuf::from("unused.mp4"),
            index: 0,
            kind: TrackKind::Video,
        }];
        let called = AtomicUsize::new(0);
        let result = collect(
            &scheduler,
            &tracks,
            &[metadata::Metadata::default()],
            |_| {
                called.fetch_add(1, Ordering::Relaxed);
                Ok(())
            },
        );
        assert_eq!(
            result.err().unwrap(),
            ConversionError(if memory > 128 * 1024 * 1024 {
                "cancelled"
            } else {
                "resource-budget"
            })
        );
        assert_eq!(called.load(Ordering::Relaxed), 0);
    }
}

#[test]
fn finite_audio_bindings_keep_separate_private_views_with_one_cpu() {
    use crate::conversion::{audio_presentation, copy_private, ts};
    let root = crate::native::private_tempdir().unwrap();
    let stage = crate::conversion::files::Stage::new(Some(root.path())).unwrap();
    let command = command(
        1,
        4 * 1024 * 1024 * 1024,
        Stop::new(),
        Arc::new(Collector::default()),
    );
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let transport = stage.path().join("audio.ts");
    let created = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(clip)
        .args([
            "-map", "0:a:0", "-vn", "-c:a", "aac", "-ar", "44100", "-t", "1.05", "-f", "mpegts",
        ])
        .arg(&transport)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "finite audio fixture creation failed"
    );
    let mut scanner = ts::NativeScanner::new();
    scanner.segment(&transport, &command.backend.stop).unwrap();
    let (_, native) = scanner.finish().unwrap();
    let native = native.values().next().unwrap();
    let source = stage.path().join("claimed-audio.mp4");
    command
        .backend
        .demux_transport(&transport, TrackKind::Audio, &source, Some(native))
        .unwrap();
    let timing = native.timing().unwrap();
    let range = audio_presentation::AudioPresentation::new(
        audio_presentation::AudioEvidence {
            sample_rate: 44_100,
            coded_frames: timing.count,
            raw_first_pts: timing.first_pts,
            raw_coded_end: timing.end_pts,
        },
        timing.count * 1024 - 17,
    )
    .unwrap();
    audio_presentation::restore(&source, range, &command.backend.stop).unwrap();
    let second = stage.path().join("second-audio.mp4");
    copy_private(&source, &second).unwrap();
    let tracks = [
        LocalTrack {
            source: source.clone(),
            index: 0,
            kind: TrackKind::Audio,
        },
        LocalTrack {
            source: second,
            index: 0,
            kind: TrackKind::Audio,
        },
    ];
    let descriptions = [metadata::Metadata::default(), metadata::Metadata::default()];
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    let input = input(&source, &keys, &hls);
    let scheduler = Scheduler::observed(&command.backend, Some(2), hardware());
    let prepared = collect(&scheduler, &tracks, &descriptions, |ordinal| {
        prepare_track(
            ordinal,
            &tracks[ordinal],
            descriptions[ordinal].clone(),
            &input,
            stage.path(),
            &command.backend,
            SourceValidationPurpose::Verification,
        )
    })
    .unwrap();
    assert_ne!(prepared[0].media, prepared[1].media);
    for track in prepared {
        assert_eq!(track.audio_presentation, Some(range));
        assert!(
            audio_presentation::read_claim(&track.media)
                .unwrap()
                .is_none()
        );
        let compared = command.verify_audio_projection(&track, &keys).unwrap();
        assert!(compared.passed && compared.checks.is_pass());
        assert!(compared.checks.packet_payloads_equal && compared.checks.packet_timing_equal);
    }
}
