use super::*;
use std::sync::mpsc;

fn workload() -> Workload {
    Workload {
        max_pixels: Some(0),
        input_buffer_bytes: Some(0),
        tracks: 1,
    }
}

fn child_bytes() -> u64 {
    workload().estimated_child_bytes(DEFAULT_STDOUT_BYTES, DEFAULT_STDERR_BYTES)
}

fn runner(cpus: usize, children: u64) -> ToolRunner {
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(crate::events::NoSink))
        .with_limiter(Arc::new(Limiter::new(cpus, children * child_bytes())));
    runner.register_workload(workload());
    runner
}

#[test]
fn independent_packaging_calls_claim_one_cpu_and_one_child_without_shrinking_the_cohort() {
    for (cpus, children, maximum) in [(4, 4, 4), (2, 4, 4), (4, 1, 4), (4, 4, 1), (64, 64, 64)] {
        let base = runner(cpus, children).cohort(maximum);
        let scoped = base.for_packaging();
        let claim = scoped.call_claim(&RunOptions::default());
        assert_eq!(claim.cpu_threads, 1);
        assert_eq!(claim.memory_bytes, child_bytes());
        assert_eq!(scoped.native_capacity(), base.native_capacity());
        drop(scoped.reserve("packager", &RunOptions::default()).unwrap());
    }
}

#[test]
fn retained_contexts_leave_one_actual_leaf_without_changing_configured_capacity() {
    let base = runner(4, 4).cohort(4);
    let context = base
        .retain_context(
            3 * child_bytes(),
            child_bytes(),
            &Stop::new(),
            Duration::from_secs(1),
        )
        .unwrap();
    let scoped = base.for_packaging();
    assert_eq!(scoped.native_capacity(), (4, 4 * child_bytes()));
    let leaf = scoped.reserve("packager", &RunOptions::default()).unwrap();
    let waiting = scoped.reserve(
        "packager",
        &RunOptions {
            timeout: Some(Duration::from_millis(20)),
            ..RunOptions::default()
        },
    );
    assert_eq!(waiting.unwrap_err().kind, "timeout");
    drop(leaf);
    drop(context);
    drop(scoped.reserve("packager", &RunOptions::default()).unwrap());
    let bounded = base
        .within_context(3 * child_bytes(), child_bytes())
        .for_packaging();
    drop(bounded.reserve("packager", &RunOptions::default()).unwrap());
    let insufficient = base
        .within_context(3 * child_bytes() + 1, child_bytes())
        .for_packaging();
    assert_eq!(
        insufficient
            .reserve("packager", &RunOptions::default())
            .unwrap_err()
            .kind,
        "resource-budget"
    );
}

#[test]
fn private_packaging_workload_survives_sibling_mutations_and_observed_cleanup_forks() {
    let base = runner(4, 4).cohort(4);
    let scoped = base.for_packaging();
    let options = RunOptions::default();
    let observed = scoped.observed(Arc::new(crate::events::NoSink));
    let (cleanup, _) = scoped.ensuring_cleanup_owner();
    base.register_workload(Workload {
        max_pixels: Some(3840 * 2160),
        input_buffer_bytes: Some(32 * 1024 * 1024),
        tracks: 4,
    });
    assert!(base.call_claim(&options).memory_bytes > child_bytes());
    for fork in [&scoped, &observed, &cleanup] {
        let claim = fork.call_claim(&options);
        assert_eq!(claim.cpu_threads, 1);
        assert_eq!(claim.memory_bytes, child_bytes());
        assert_eq!(fork.native_capacity(), (4, 4 * child_bytes()));
    }
}

#[test]
fn concurrent_retained_coordinators_keep_their_guaranteed_leaf_headroom() {
    let base = runner(4, 4).cohort(4);
    let contexts: Vec<_> = (0..2)
        .map(|_| {
            base.retain_context(
                child_bytes(),
                child_bytes(),
                &Stop::new(),
                Duration::from_secs(1),
            )
            .unwrap()
        })
        .collect();
    let first = base
        .within_context(child_bytes(), child_bytes())
        .for_packaging();
    let second = base
        .within_context(child_bytes(), child_bytes())
        .for_packaging();
    assert_eq!(first.native_capacity().1, 3 * child_bytes());
    let (started, ready) = mpsc::sync_channel(2);
    std::thread::scope(|scope| {
        let mut releases = Vec::new();
        let mut workers = Vec::new();
        for output in [first, second] {
            let started = started.clone();
            let (release, released) = mpsc::sync_channel(1);
            releases.push(release);
            workers.push(scope.spawn(move || {
                let permit = output
                    .reserve(
                        "packager",
                        &RunOptions {
                            timeout: Some(Duration::from_secs(1)),
                            ..RunOptions::default()
                        },
                    )
                    .unwrap();
                started.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(2)).unwrap();
                drop(permit);
            }));
        }
        // Both outputs must enter while the other's permit and both contexts remain held.
        for _ in 0..2 {
            ready.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        for release in releases {
            release.send(()).unwrap();
        }
        for worker in workers {
            worker.join().unwrap();
        }
    });
    drop(contexts);
    drop(
        base.native_permit(
            Claim {
                cpu_threads: 4,
                memory_bytes: 4 * child_bytes(),
            },
            &Stop::new(),
            Duration::from_secs(1),
        )
        .unwrap(),
    );
}

#[test]
fn overflowing_or_insufficient_child_workloads_are_refused() {
    for (pixels, buffered) in [(u64::MAX, u64::MAX), (1, 0)] {
        let base = runner(64, 1);
        base.register_workload(Workload {
            max_pixels: Some(pixels),
            input_buffer_bytes: Some(buffered),
            tracks: 1,
        });
        let scoped = base.for_packaging();
        let claim = scoped.call_claim(&RunOptions::default());
        assert_eq!(claim.cpu_threads, 1);
        assert!(claim.memory_bytes > child_bytes());
        if pixels == u64::MAX {
            assert_eq!(claim.memory_bytes, u64::MAX);
        }
        assert_eq!(
            scoped
                .reserve("packager", &RunOptions::default())
                .unwrap_err()
                .kind,
            "resource-budget"
        );
    }
}

#[test]
fn two_joint_outputs_cannot_overbook_cpu_cohort_or_memory_and_release_after_failure() {
    for (cpus, children, maximum) in [(1, 8, 8), (8, 8, 1), (8, 1, 8)] {
        let base = runner(cpus, children).cohort(maximum);
        let first = base.for_packaging();
        let second = base.for_packaging();
        let (started, ready) = mpsc::sync_channel(1);
        let (release, released) = mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            let active = scope.spawn(move || {
                first.run_tool_with(
                    &["fixture".into()],
                    "packager",
                    &RunOptions::default(),
                    false,
                    None,
                    |_, _| {
                        started.send(()).unwrap();
                        released.recv_timeout(Duration::from_secs(2)).unwrap();
                        Err(Failure {
                            kind: "spawn-failed",
                            text: "synthetic final packaging failure".into(),
                        })
                    },
                )
            });
            ready.recv_timeout(Duration::from_secs(1)).unwrap();
            let blocked = second.reserve(
                "packager",
                &RunOptions {
                    timeout: Some(Duration::from_millis(20)),
                    ..RunOptions::default()
                },
            );
            let stopped = Stop::new();
            let queued_stop = stopped.clone();
            let queued = second.for_packaging();
            let (attempting, attempted) = mpsc::sync_channel(1);
            let waiter = std::thread::spawn(move || {
                attempting.send(()).unwrap();
                queued.reserve(
                    "packager",
                    &RunOptions {
                        stop: Some(&queued_stop),
                        timeout: Some(Duration::from_secs(1)),
                        ..RunOptions::default()
                    },
                )
            });
            attempted.recv_timeout(Duration::from_secs(1)).unwrap();
            stopped.set();
            let cancelled = waiter.join().unwrap();
            release.send(()).unwrap();
            let failed = active.join().unwrap();
            assert_eq!(blocked.unwrap_err().kind, "timeout");
            assert_eq!(cancelled.unwrap_err().kind, "cancelled");
            assert_eq!(failed.unwrap_err().kind, "spawn-failed");
        });
        drop(second.reserve("packager", &RunOptions::default()).unwrap());
        drop(
            base.native_permit(
                Claim {
                    cpu_threads: maximum.min(cpus),
                    memory_bytes: children * child_bytes(),
                },
                &Stop::new(),
                Duration::from_secs(1),
            )
            .unwrap(),
        );
    }
}
