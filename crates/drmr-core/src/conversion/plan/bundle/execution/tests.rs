use super::*;
use crate::{
    concurrency::Stop,
    conversion::{
        SourceInventory,
        plan::bundle::{BundleOutputIntent, ManifestValidation},
    },
    events::NoSink,
    inspection::TargetProtection,
    resources::Limiter,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
mod memory_tests;

fn converter(cpus: usize, memory: u64, stop: Stop) -> Converter {
    Converter::new(
        Arc::new(
            ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
                .with_limiter(Arc::new(Limiter::new(cpus, memory))),
        ),
        "packager",
        "ffmpeg",
    )
    .with_stop(stop)
    .with_timeout(Duration::from_secs(3))
}

fn plan(outputs: usize) -> BundlePlan {
    BundlePlan {
        kind: "execution-plan".into(),
        schema_version: 6,
        plan_id: "a".repeat(64),
        source: SourceInventory {
            schema_version: 1,
            root: PathBuf::from("/source"),
            entrypoint: PathBuf::from("manifest.mpd"),
            files: Vec::new(),
            graph_digest: "b".repeat(64),
        },
        source_format: OutputFormat::Dash,
        output_root: PathBuf::from("/result"),
        required_depth: Some(crate::conversion::ValidationDepth::Native),
        processing: None,
        outputs: (0..outputs)
            .map(|ordinal| BundleOutputIntent {
                id: format!("output-{ordinal}"),
                format: OutputFormat::Hls,
                target: TargetProtection::Clear,
                drms: Vec::new(),
                conformance: ManifestValidation::None,
                processing: None,
            })
            .collect(),
    }
}

fn observed(converter: &Converter) -> Execution<'_> {
    Execution {
        converter,
        hardware: Some(Hardware::from_observations(
            8,
            Some(8 * 1024 * 1024 * 1024),
            None,
        )),
    }
}

#[test]
fn independent_output_pipelines_overlap_and_return_plan_order() {
    let converter = converter(2, 4 * 1024 * 1024 * 1024, Stop::new());
    let execution = observed(&converter);
    let (entered, observed) = mpsc::sync_channel(1);
    let observed = Mutex::new(observed);
    let (_retained, outputs) = execution
        .run(&plan(2), |ordinal| {
            if ordinal == 0 {
                observed
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| ConversionError("bundle-output-serialized"))?;
            } else {
                entered.send(()).unwrap();
            }
            Ok(ordinal)
        })
        .unwrap();
    assert_eq!(outputs, vec![0, 1]);
}

#[test]
fn two_gib_two_output_budget_keeps_metadata_charges_and_scales_workers() {
    let hardware = Hardware::from_observations(8, Some(2 * 1024 * 1024 * 1024), None);
    let workload = Workload {
        max_pixels: Some(1920 * 1080),
        input_buffer_bytes: Some(64 * 1024),
        tracks: 1,
    };
    let contexts = retained::<usize>(
        &plan(2),
        workload,
        crate::conversion::ValidationDepth::Native,
    )
    .unwrap();
    assert!(contexts.shared >= 2 * frozen::retained_inventory_bound(false).unwrap());
    assert_eq!(contexts.worker, 2 * TRACK_RESIDENCY_BYTES);
    for maximum in [Some(1), Some(2), Some(4), None] {
        let selected = budget(hardware, workload, maximum, (8, u64::MAX), 2, &contexts).unwrap();
        assert_eq!(selected.workers, maximum.unwrap_or(2).min(2));
    }
    let one = contexts.shared + contexts.worker + contexts.leaf;
    assert_eq!(
        budget(hardware, workload, Some(4), (8, one), 2, &contexts)
            .unwrap()
            .workers,
        1
    );
    assert!(budget(hardware, workload, Some(4), (8, one - 1), 2, &contexts).is_err());
    let larger = Workload {
        tracks: 8,
        input_buffer_bytes: Some(64 * 1024 * 1024),
        ..workload
    };
    let larger =
        retained::<usize>(&plan(2), larger, crate::conversion::ValidationDepth::Native).unwrap();
    assert!(
        larger.shared > contexts.shared
            && larger.worker > contexts.worker
            && larger.leaf > contexts.leaf
    );
}

#[test]
fn capacity_one_linter_uses_leaf_ceiling_and_retained_results_keep_their_lease() {
    let stop = Stop::new();
    let converter = converter(1, 768 * 1024 * 1024, stop.clone());
    let original = converter.backend.runner.native_capacity();
    let execution = observed(&converter);
    let (retained, outputs) = execution
        .run(&plan(2), |ordinal| {
            let ceiling = converter.backend.runner.native_capacity();
            assert_eq!(ceiling.0, 1);
            assert!(ceiling.1 < original.1);
            let _linter = converter
                .backend
                .runner
                .native_permit(
                    crate::resources::Claim {
                        cpu_threads: 1,
                        memory_bytes: ceiling.1,
                    },
                    &stop,
                    Duration::from_millis(200),
                )
                .map_err(|_| ConversionError("nested-linter-admission-failed"))?;
            Ok(ordinal)
        })
        .unwrap();
    assert_eq!(outputs, vec![0, 1]);
    assert!(converter.backend.runner.native_capacity().1 < original.1);
    drop(retained);
    assert_eq!(converter.backend.runner.native_capacity(), original);
}

#[test]
fn failures_drain_select_plan_order_and_release_context_without_poisoning_stop() {
    let stop = Stop::new();
    let converter = converter(2, 4 * 1024 * 1024 * 1024, stop.clone());
    let original = converter.backend.runner.native_capacity();
    let execution = observed(&converter);
    let (failed, observed) = mpsc::sync_channel(1);
    let observed = Mutex::new(observed);
    let drained = AtomicBool::new(false);
    let started = AtomicUsize::new(0);
    let result = execution.run::<()>(&plan(3), |ordinal| {
        started.fetch_add(1, Ordering::Relaxed);
        if ordinal == 0 {
            observed
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| ConversionError("sibling-not-started"))?;
            drained.store(true, Ordering::Release);
            Err(ConversionError("first-output-failed"))
        } else {
            failed.send(()).unwrap();
            Err(ConversionError("second-output-failed"))
        }
    });
    assert_eq!(result.unwrap_err(), ConversionError("first-output-failed"));
    assert!(drained.load(Ordering::Acquire));
    assert_eq!(started.load(Ordering::Relaxed), 2);
    assert!(!stop.is_set());
    assert_eq!(converter.backend.runner.native_capacity(), original);
    let (_retained, outputs) = execution.run(&plan(2), Ok).unwrap();
    assert_eq!(outputs, vec![0, 1]);
}

#[test]
fn cancellation_or_insufficient_retained_capacity_never_starts_extra_outputs() {
    let stop = Stop::new();
    let converter = converter(1, 2 * 1024 * 1024 * 1024, stop.clone());
    let original = converter.backend.runner.native_capacity();
    let execution = observed(&converter);
    let started = AtomicUsize::new(0);
    let result = execution.run(&plan(3), |ordinal| {
        started.fetch_add(1, Ordering::Relaxed);
        stop.set();
        Ok(ordinal)
    });
    assert_eq!(result.unwrap_err(), ConversionError("cancelled"));
    assert_eq!(started.load(Ordering::Relaxed), 1);
    assert_eq!(converter.backend.runner.native_capacity(), original);
    let small = self::converter(2, 256 * 1024 * 1024, Stop::new());
    assert_eq!(
        observed(&small)
            .run(&plan(2), |_| {
                started.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })
            .unwrap_err(),
        ConversionError("resource-budget")
    );
    assert_eq!(started.load(Ordering::Relaxed), 1);
}

#[test]
fn panic_drains_siblings_before_returning_every_retained_byte() {
    let stop = Stop::new();
    let converter = converter(2, 4 * 1024 * 1024 * 1024, stop.clone());
    let original = converter.backend.runner.native_capacity();
    let execution = observed(&converter);
    let (entered, observed) = mpsc::sync_channel(1);
    let observed = Mutex::new(observed);
    let drained = AtomicBool::new(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execution.run::<()>(&plan(2), |ordinal| {
            if ordinal == 0 {
                observed
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(2))
                    .unwrap();
                panic!("synthetic output panic");
            }
            entered.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            drained.store(true, Ordering::Release);
            Ok(())
        })
    }));
    assert!(result.is_err());
    assert!(drained.load(Ordering::Acquire));
    assert!(!stop.is_set());
    assert_eq!(converter.backend.runner.native_capacity(), original);
}

#[test]
fn nested_output_pools_share_the_commands_worker_upper_bound() {
    let converter = converter(4, 8 * 1024 * 1024 * 1024, Stop::new())
        .with_max_workers(Some(2))
        .unwrap();
    let converter = converter.bundle_cohort();
    let execution = observed(&converter);
    let release = Stop::new();
    let (entered, observed) = mpsc::sync_channel(4);
    std::thread::scope(|scope| {
        let worker = scope.spawn(|| {
            execution.run(&plan(2), |_| {
                let scheduler = crate::conversion::execution::Scheduler::observed(
                    &converter.backend,
                    Some(2),
                    Hardware::from_observations(8, Some(8 * 1024 * 1024 * 1024), None),
                );
                scheduler.run(2, 1, 0, |_| {
                    let _permit = converter
                        .backend
                        .runner
                        .native_permit(
                            crate::resources::Claim {
                                cpu_threads: 1,
                                memory_bytes: 128 * 1024 * 1024,
                            },
                            &converter.backend.stop,
                            Duration::from_secs(2),
                        )
                        .map_err(|_| ConversionError("nested-scheduling-failed"))?;
                    entered.send(()).unwrap();
                    if !release.wait(Some(Duration::from_secs(2))) {
                        return Err(ConversionError("finite-test-rescue"));
                    }
                    Ok(())
                })
            })
        });
        let first = observed.recv_timeout(Duration::from_secs(2));
        let second = observed.recv_timeout(Duration::from_secs(2));
        let extra = observed.recv_timeout(Duration::from_millis(30));
        release.set();
        let (_retained, results) = worker.join().unwrap().unwrap();
        assert!(first.is_ok() && second.is_ok());
        assert!(
            extra.is_err(),
            "nested pools exceeded the command worker maximum"
        );
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|result| result.len() == 2));
    });
}
