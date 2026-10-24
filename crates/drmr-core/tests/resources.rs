//! Resource decisions depend on both the allowed hardware and the media workload.

use drmr_core::concurrency::Stop;
use drmr_core::resources::{Budget, Hardware, MemorySource, Workload};
use drmr_core::resources::{Claim, Limiter, ReservationError};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;
const PYTHON: &str = if cfg!(windows) { "python" } else { "python3" };

fn small_video() -> Workload {
    Workload {
        max_pixels: Some(640 * 360),
        input_buffer_bytes: Some(MIB),
        tracks: 2,
    }
}

#[test]
fn process_cgroup_headroom_takes_precedence_over_host_memory() {
    let hardware = Hardware::from_observations(8, Some(16 * 1024 * MIB), Some(768 * MIB));
    assert_eq!(hardware.available_memory_bytes, Some(768 * MIB));
    assert_eq!(hardware.memory_source, MemorySource::ProcessCgroup);
    let limited = Budget::choose(hardware, small_video(), Some(64)).unwrap();
    let host = Budget::choose(
        Hardware::from_observations(8, Some(16 * 1024 * MIB), None),
        small_video(),
        Some(64),
    )
    .unwrap();
    assert!(limited.workers < host.workers);
    assert!(limited.memory_tokens_bytes < 768 * MIB);
}

#[test]
fn large_frames_and_segments_reduce_parallelism_without_raising_the_user_cap() {
    let hardware = Hardware::from_observations(32, Some(4 * 1024 * MIB), None);
    let small = Budget::choose(hardware, small_video(), Some(16)).unwrap();
    let large = Budget::choose(
        hardware,
        Workload {
            max_pixels: Some(3840 * 2160),
            input_buffer_bytes: Some(128 * MIB),
            tracks: 2,
        },
        Some(16),
    )
    .unwrap();
    assert!(large.workers < small.workers);
    assert!(large.estimated_child_bytes > small.estimated_child_bytes);
    assert_eq!(
        Budget::choose(hardware, small_video(), Some(2))
            .unwrap()
            .workers,
        2
    );
    assert_eq!(large.child_threads, 1);
}

#[test]
fn unknown_memory_uses_one_worker_and_zero_known_headroom_is_refused() {
    let unknown = Budget::choose(
        Hardware::from_observations(64, None, None),
        small_video(),
        Some(64),
    )
    .unwrap();
    assert_eq!(unknown.workers, 1);
    assert_eq!(unknown.memory_source, MemorySource::Unknown);
    assert!(
        Budget::choose(
            Hardware::from_observations(64, Some(16 * 1024 * MIB), Some(0)),
            small_video(),
            Some(64),
        )
        .is_err()
    );
}

#[test]
fn track_residency_and_overflow_are_accounted_for_conservatively() {
    let hardware = Hardware::from_observations(16, Some(2 * 1024 * MIB), None);
    let small = Budget::choose(hardware, small_video(), None).unwrap();
    let mut many = small_video();
    many.tracks = 64;
    assert!(Budget::choose(hardware, many, None).unwrap().workers < small.workers);
    let mut overflow = small_video();
    overflow.max_pixels = Some(u64::MAX);
    assert!(Budget::choose(hardware, overflow, None).is_err());
}

#[test]
fn hardware_snapshot_reports_only_numeric_capacity() {
    let hardware = Hardware::detect();
    assert!(hardware.effective_cpus >= 1);
    if let Ok(cpu) = std::env::var("DRMR_EXPECT_CPU_LIMIT") {
        assert!(hardware.effective_cpus <= cpu.parse::<usize>().unwrap());
    }
    if let Ok(memory) = std::env::var("DRMR_EXPECT_MEMORY_LIMIT") {
        assert!(hardware.available_memory_bytes.unwrap() <= memory.parse::<u64>().unwrap());
        assert_eq!(hardware.memory_source, MemorySource::ProcessCgroup);
        #[cfg(target_os = "linux")]
        assert!(
            std::fs::read_to_string("/proc/self/cgroup")
                .unwrap()
                .lines()
                .any(|line| line.starts_with("0::/") && line != "0::/"),
            "the controlled Linux measurement must exercise a nested process group"
        );
    }
    println!("hardware={hardware:?}");
}

#[test]
fn zero_or_out_of_range_worker_maximums_are_refused() {
    let hardware = Hardware::from_observations(64, Some(64 * 1024 * MIB), None);
    for maximum in [0, 65, usize::MAX] {
        assert!(Budget::choose(hardware, small_video(), Some(maximum)).is_err());
    }
    let unknown = Hardware::from_observations(64, None, None);
    let mut many = small_video();
    many.tracks = 64;
    assert!(Budget::choose(unknown, many, None).is_err());
}

#[test]
fn registered_content_controls_tool_admission_without_losing_the_worker_upper_bound() {
    use drmr_core::events::NoSink;
    use drmr_core::toolrun::{RunOptions, Secrets, ToolRunner};
    let workload = Workload {
        max_pixels: Some(640 * 360),
        input_buffer_bytes: Some(188),
        tracks: 1,
    };
    assert_eq!(workload.max_pixels, Some(640 * 360));
    assert_eq!(workload.input_buffer_bytes, Some(188));
    assert_eq!(workload.tracks, 1);
    let claim = Claim::for_workload(workload, 16 * 1024 * 1024, 1024 * 1024);
    let runner = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
            .with_limiter(Arc::new(Limiter::new(1, claim.memory_bytes))),
    );
    runner.register_workload(workload);
    let budget = Budget::choose(Hardware::detect(), workload, Some(1)).unwrap();
    assert_eq!(budget.workers, 1);
    runner.note_resource_budget(&budget);
    // This allocation fits the actual small source but not the unknown 4K fallback.
    assert_eq!(
        runner
            .run_tool(
                &[PYTHON.into(), "-c".into(), "print('complete')".into()],
                "registered-content",
                &RunOptions::default()
            )
            .unwrap()
            .returncode,
        Some(0)
    );
    runner.register_workload(Workload {
        max_pixels: Some(3840 * 2160),
        ..workload
    });
    assert_eq!(
        runner
            .run_tool(
                &[PYTHON.into(), "-c".into(), "print('complete')".into()],
                "larger-content",
                &RunOptions {
                    workload: Some(workload),
                    ..RunOptions::default()
                }
            )
            .unwrap_err()
            .kind,
        "resource-budget"
    );
}

#[test]
fn unwinding_returns_reserved_tokens() {
    let limiter = Arc::new(Limiter::new(1, 100));
    let claim = Claim {
        cpu_threads: 1,
        memory_bytes: 100,
    };
    let result = std::panic::catch_unwind(|| {
        let _lease = limiter.acquire(claim, None, None).unwrap();
        panic!("synthetic task panic");
    });
    assert!(result.is_err());
    assert!(
        limiter
            .acquire(claim, None, Some(Duration::from_millis(30)))
            .is_ok()
    );
}

#[test]
fn captured_decoders_cannot_start_without_shared_resource_admission() {
    use drmr_core::events::{Collector, Sink};
    use drmr_core::toolrun::{RunOptions, Secrets, ToolRunner};
    let collector = Arc::new(Collector::default());
    let sink: Arc<dyn Sink> = collector.clone();
    let runner = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), sink)
            .with_limiter(Arc::new(Limiter::new(0, u64::MAX))),
    );
    let directory = tempfile::tempdir().unwrap();
    let marker = directory.path().join("started");
    let tool = drmr_test_support::install(
        &directory.path().join("decoder"),
        &drmr_test_support::ToolSpec::default().with("ready_file", marker.to_str().unwrap()),
    )
    .unwrap();
    let error = runner
        .run_tool(
            &[tool.to_str().unwrap().into()],
            "decoder",
            &RunOptions::default(),
        )
        .unwrap_err();
    assert_eq!(error.kind, "resource-budget");
    assert!(!marker.exists(), "a denied decoder cannot start");
    let records = collector.records();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["event"], "command");
    assert!(
        records[1]["returncode"].is_null(),
        "a command record describes denied intent, not a started child"
    );
}

#[test]
fn already_cancelled_tools_record_the_admission_failure_without_spawning() {
    use drmr_core::events::Collector;
    use drmr_core::toolrun::{RunOptions, Secrets, ToolRunner};
    let directory = drmr_core::native::private_tempdir().unwrap();
    let marker = directory.path().join("started");
    let fixture = drmr_test_support::install(
        &directory.path().join("probe"),
        &drmr_test_support::ToolSpec::default().with("ready_file", marker.to_str().unwrap()),
    )
    .unwrap();
    let stop = Stop::new();
    stop.set();
    for program in [
        "not-a-real-ffprobe-resource-test",
        fixture.to_str().unwrap(),
    ] {
        let records = Arc::new(Collector::default());
        let runner = ToolRunner::new(Arc::new(Secrets::new()), records.clone());
        let error = runner
            .run_tool(
                &[program.into()],
                "probe",
                &RunOptions {
                    stop: Some(&stop),
                    ..RunOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(error.kind, "cancelled");
        // A command record describes intent; a denied admission has no child status.
        let events = records.records();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["event"], "command");
        assert_eq!(events[1]["event"], "tool-output");
        assert_eq!(events[0]["call_id"], events[1]["call_id"]);
        assert_eq!(events[1]["returncode"], serde_json::Value::Null);
        assert_eq!(events[1]["stdout"], "");
        assert_eq!(events[1]["stderr"], "tool admission failed: cancelled");
        assert!(
            !marker.exists(),
            "a cancelled call must not start its executable"
        );
    }
    // The same native executable must publish its marker when it is admitted.
    let runner = ToolRunner::new(
        Arc::new(Secrets::new()),
        Arc::new(drmr_core::events::NoSink),
    );
    assert_eq!(
        runner
            .run_tool(
                &[fixture.to_str().unwrap().into()],
                "probe",
                &RunOptions::default(),
            )
            .unwrap()
            .returncode,
        Some(0)
    );
    assert_eq!(std::fs::read(marker).unwrap(), b"ready");
}

#[test]
fn captured_media_reports_the_output_limit_instead_of_tool_unavailability() {
    use drmr_core::events::NoSink;
    use drmr_core::toolrun::{RunOptions, Secrets, ToolRunner};
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)));
    let error = runner
        .run_tool(
            &[
                PYTHON.into(),
                "-c".into(),
                "print('x'*(20*1024*1024))".into(),
            ],
            "oversized-media-reference",
            &RunOptions {
                log_stdout: false,
                ..RunOptions::default()
            },
        )
        .unwrap_err();
    assert_eq!(error.kind, "output-limit");
}

#[test]
fn a_lease_holds_both_cpu_and_memory_until_it_is_dropped() {
    let limiter = Arc::new(Limiter::new(4, 100));
    let claim = Claim {
        cpu_threads: 1,
        memory_bytes: 60,
    };
    let first = limiter.acquire(claim, None, None).unwrap();
    assert_eq!(
        limiter
            .acquire(claim, None, Some(Duration::from_millis(30)))
            .unwrap_err(),
        ReservationError::Timeout
    );
    drop(first);
    let second = limiter
        .acquire(claim, None, Some(Duration::from_millis(30)))
        .unwrap();
    drop(second);
    let all_cpus = limiter
        .acquire(
            Claim {
                cpu_threads: 4,
                memory_bytes: 1,
            },
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        limiter
            .acquire(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: 1
                },
                None,
                Some(Duration::from_millis(30))
            )
            .unwrap_err(),
        ReservationError::Timeout
    );
    drop(all_cpus);
}

#[test]
fn a_waiting_reservation_cancels_without_acquiring_or_leaking_tokens() {
    let limiter = Arc::new(Limiter::new(1, 100));
    let claim = Claim {
        cpu_threads: 1,
        memory_bytes: 60,
    };
    let first = limiter.acquire(claim, None, None).unwrap();
    let stop = Stop::new();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| limiter.acquire(claim, Some(&stop), None));
        stop.set();
        assert_eq!(
            waiter.join().unwrap().unwrap_err(),
            ReservationError::Cancelled
        );
    });
    drop(first);
    assert!(
        limiter
            .acquire(claim, None, Some(Duration::from_millis(30)))
            .is_ok()
    );
}

#[test]
fn an_impossible_claim_fails_immediately_instead_of_waiting_forever() {
    let limiter = Arc::new(Limiter::new(2, 100));
    let started = Instant::now();
    for claim in [
        Claim {
            cpu_threads: 3,
            memory_bytes: 1,
        },
        Claim {
            cpu_threads: 1,
            memory_bytes: 101,
        },
    ] {
        assert_eq!(
            limiter.acquire(claim, None, None).unwrap_err(),
            ReservationError::Insufficient
        );
    }
    assert!(started.elapsed() < Duration::from_secs(1));
}
