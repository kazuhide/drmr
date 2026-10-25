use super::*;
use crate::{
    concurrency::Stop,
    conversion::{Converter, execution::Scheduler},
    resources::{Hardware, Limiter},
    toolrun::{Secrets, ToolRunner},
};
use std::time::Duration;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
};

fn converter(cpus: usize, stop: Stop) -> Converter {
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(crate::events::NoSink))
        .with_limiter(Arc::new(Limiter::new(cpus, 4 * 1024 * 1024 * 1024)));
    Converter::new(Arc::new(runner), "packager", "ffmpeg").with_stop(stop)
}

fn hardware() -> Hardware {
    Hardware::from_observations(8, Some(8 * 1024 * 1024 * 1024), None)
}

fn paths(count: usize) -> Vec<MediaPath> {
    (0..count)
        .map(|ordinal| MediaPath {
            path: PathBuf::from(format!("rendition-{ordinal}.m3u8")),
            role: Some(TrackKind::Video),
            variants: count,
            metadata: metadata::Metadata::default(),
            audio_variant_alias: false,
        })
        .collect()
}

#[test]
fn independent_renditions_overlap_and_keep_manifest_order() {
    let converter = converter(2, Stop::new());
    let scheduler = Scheduler::observed(&converter.backend, Some(2), hardware());
    let (entered, observed) = mpsc::sync_channel(1);
    let observed = Mutex::new(observed);
    let tracks = collect(&scheduler, &paths(2), |ordinal| {
        if ordinal == 0 {
            observed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| ConversionError("rendition-preparation-serialized"))?;
        } else {
            entered.send(()).unwrap();
        }
        Ok(vec![ordinal])
    })
    .unwrap();
    assert_eq!(tracks, vec![0, 1]);
}

#[test]
fn capacity_one_leaves_the_cpu_token_for_nested_media_work() {
    let stop = Stop::new();
    let converter = converter(1, stop.clone());
    let scheduler = Scheduler::observed(&converter.backend, Some(4), hardware());
    let tracks = collect(&scheduler, &paths(3), |ordinal| {
        let _permit = converter
            .backend
            .runner
            .native_permit(
                crate::resources::Claim {
                    cpu_threads: 1,
                    memory_bytes: 128 * 1024 * 1024,
                },
                &stop,
                Duration::from_secs(1),
            )
            .map_err(|_| ConversionError("nested-admission-failed"))?;
        Ok(vec![ordinal])
    })
    .unwrap();
    assert_eq!(tracks, vec![0, 1, 2]);
    assert!(!stop.is_set());
}

#[test]
fn later_failure_waits_for_earlier_work_and_does_not_poison_the_caller() {
    let stop = Stop::new();
    let converter = converter(2, stop.clone());
    let scheduler = Scheduler::observed(&converter.backend, Some(2), hardware());
    let (failed, observed) = mpsc::sync_channel(1);
    let observed = Mutex::new(observed);
    let drained = AtomicBool::new(false);
    let result = collect::<()>(&scheduler, &paths(2), |ordinal| {
        if ordinal == 0 {
            observed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| ConversionError("second-rendition-not-started"))?;
            drained.store(true, Ordering::Release);
            Err(ConversionError("first-rendition-failed"))
        } else {
            failed.send(()).unwrap();
            Err(ConversionError("second-rendition-failed"))
        }
    });
    assert_eq!(
        result.unwrap_err(),
        ConversionError("first-rendition-failed")
    );
    assert!(drained.load(Ordering::Acquire));
    assert!(!stop.is_set());
    assert_eq!(
        collect(&scheduler, &paths(2), |ordinal| Ok(vec![ordinal])).unwrap(),
        vec![0, 1]
    );
}

#[test]
fn panic_drains_started_renditions_before_resuming_on_the_caller() {
    let stop = Stop::new();
    let converter = converter(2, stop.clone());
    let scheduler = Scheduler::observed(&converter.backend, Some(2), hardware());
    let (entered, observed) = mpsc::sync_channel(1);
    let observed = Mutex::new(observed);
    let drained = AtomicBool::new(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        collect::<()>(&scheduler, &paths(2), |ordinal| {
            if ordinal == 0 {
                observed
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                panic!("synthetic rendition panic");
            }
            entered.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            drained.store(true, Ordering::Release);
            Ok(vec![()])
        })
    }));
    assert!(result.is_err());
    assert!(drained.load(Ordering::Acquire));
    assert!(!stop.is_set());
}

#[test]
fn serial_renditions_observe_cancellation_before_starting_the_next_one() {
    let stop = Stop::new();
    let converter = converter(1, stop.clone());
    let scheduler = Scheduler::observed(&converter.backend, Some(4), hardware());
    let started = AtomicUsize::new(0);
    let result = collect(&scheduler, &paths(3), |ordinal| {
        started.fetch_add(1, Ordering::Relaxed);
        stop.set();
        Ok(vec![ordinal])
    });
    assert_eq!(result.unwrap_err(), ConversionError("cancelled"));
    assert_eq!(started.load(Ordering::Relaxed), 1);
}

#[test]
fn retained_rendition_metadata_and_results_have_explicit_charges() {
    let mut source = paths(2);
    let small = retained_metadata::<u64>(&source, 2).unwrap();
    source[0].metadata.audio_group = Some(String::with_capacity(4096));
    let large = retained_metadata::<u64>(&source, 2).unwrap();
    assert_eq!(large - small, 4096);
    assert!(retained_metadata::<[u64; 8]>(&source, 2).unwrap() > large);
    assert!(retained_metadata::<u64>(&source, usize::MAX).is_err());
}
