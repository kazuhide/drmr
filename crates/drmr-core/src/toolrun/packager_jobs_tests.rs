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
fn parallel_packager_requires_every_input_job_to_fit_cpu_memory_and_cohort_limits() {
    for (cpus, children, maximum, jobs, expected) in [
        (4, 4, 4, 3, 3),
        (4, 3, 4, 3, 3),
        (4, 2, 4, 3, 1),
        (2, 4, 4, 3, 1),
        (4, 4, 2, 3, 1),
        (4, 4, 1, 3, 1),
        (4, 4, 4, 1, 1),
        (4, 4, 4, 0, 1),
        (64, 64, 64, 65, 1),
    ] {
        let scoped = runner(cpus, children)
            .cohort(maximum)
            .for_packager_jobs(jobs);
        assert_eq!(scoped.packager_jobs(), expected);
    }
}

#[test]
fn job_choice_uses_configured_capacity_and_its_own_context_not_temporary_occupancy() {
    let base = runner(4, 4).cohort(4);
    let context = base
        .retain_context(
            2 * child_bytes(),
            child_bytes(),
            &Stop::new(),
            Duration::from_secs(1),
        )
        .unwrap();
    let scoped = base.for_packager_jobs(3);
    assert_eq!(scoped.packager_jobs(), 3);
    let waiting = scoped.reserve(
        "packager",
        &RunOptions {
            timeout: Some(Duration::from_millis(20)),
            ..RunOptions::default()
        },
    );
    assert_eq!(waiting.unwrap_err().kind, "timeout");
    drop(context);
    drop(scoped.reserve("packager", &RunOptions::default()).unwrap());
    assert_eq!(
        base.within_context(2 * child_bytes(), child_bytes())
            .for_packager_jobs(3)
            .packager_jobs(),
        1
    );
    assert_eq!(
        runner(8, 8)
            .within_context(child_bytes(), 2 * child_bytes())
            .for_packager_jobs(3)
            .packager_jobs(),
        1,
        "a retained coordinator must leave enough guaranteed leaf headroom for every job"
    );
}

#[test]
fn a_private_joint_scope_charges_all_jobs_only_to_packager_and_survives_cleanup_forks() {
    let base = runner(4, 4).cohort(4);
    let scoped = base.for_packager_jobs(3);
    let options = RunOptions::default();
    for tool in ["ffmpeg", "ffprobe", "fixture"] {
        let claim = scoped.call_claim(tool, &options).unwrap();
        assert_eq!(claim.cpu_threads, 1);
        assert_eq!(claim.memory_bytes, child_bytes());
    }
    let claim = scoped.call_claim("packager", &options).unwrap();
    assert_eq!(claim.cpu_threads, 3);
    assert_eq!(claim.memory_bytes, 3 * child_bytes());
    assert_eq!(base.packager_jobs(), 1);
    assert_eq!(
        base.call_claim("packager", &options).unwrap().cpu_threads,
        1
    );
    let observed = scoped.observed(Arc::new(crate::events::NoSink));
    let (cleanup, _) = scoped.ensuring_cleanup_owner();
    for fork in [&observed, &cleanup] {
        assert_eq!(fork.packager_jobs(), 3);
        assert_eq!(
            fork.call_claim("packager", &options).unwrap().cpu_threads,
            3
        );
    }
    base.register_workload(Workload {
        max_pixels: Some(3840 * 2160),
        input_buffer_bytes: Some(32 * 1024 * 1024),
        tracks: 4,
    });
    assert_eq!(
        scoped
            .call_claim("packager", &options)
            .unwrap()
            .memory_bytes,
        3 * child_bytes(),
        "the private call must retain the workload used for its admission choice"
    );
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
        .for_packager_jobs(2);
    let second = base
        .within_context(child_bytes(), child_bytes())
        .for_packager_jobs(2);
    assert_eq!(first.native_capacity().1, 3 * child_bytes());
    assert_eq!(first.packager_jobs(), 1);
    assert_eq!(second.packager_jobs(), 1);
    let options = RunOptions {
        timeout: Some(Duration::from_secs(1)),
        ..RunOptions::default()
    };
    let left = first.reserve("packager", &options).unwrap();
    let right = second.reserve("packager", &options).unwrap();
    drop((left, right));
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
fn parallel_child_memory_overflow_cannot_enable_threading() {
    let base = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(crate::events::NoSink))
        .with_limiter(Arc::new(Limiter::new(64, u64::MAX)));
    base.register_workload(Workload {
        max_pixels: Some(u64::MAX),
        input_buffer_bytes: Some(u64::MAX),
        tracks: 1,
    });
    assert_eq!(base.for_packager_jobs(2).packager_jobs(), 1);
}

#[test]
fn two_joint_outputs_cannot_overbook_cpu_cohort_or_memory_and_release_after_failure() {
    for (cpus, children, maximum) in [(4, 8, 8), (8, 8, 4), (8, 4, 8)] {
        let base = runner(cpus, children).cohort(maximum);
        let first = base.for_packager_jobs(3);
        let second = base.for_packager_jobs(2);
        assert_eq!(first.packager_jobs(), 3);
        assert_eq!(second.packager_jobs(), 2);
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
            let queued = second.for_packager_jobs(2);
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
