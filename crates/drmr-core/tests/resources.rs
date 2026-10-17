//! Resource decisions depend on both the allowed hardware and the media workload.

use drmr_core::concurrency::Stop;
use drmr_core::resources::{Budget, Hardware, MemorySource, Workload};
use drmr_core::resources::{Claim, Limiter, ReservationError};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MIB: u64 = 1024 * 1024;

fn small_video() -> Workload {
    Workload {
        max_pixels: Some(640 * 360),
        input_buffer_bytes: Some(MIB),
        tracks: 2,
    }
}

fn admitted_small_source(root: &std::path::Path) -> drmr_core::source::SourceAnalysis {
    use drmr_core::source::{AnalyzeOptions, Direct, FetchPolicy, SourceRequest, analyze_hls_with};
    let input = root.join("input");
    std::fs::create_dir(&input).unwrap();
    std::fs::write(
        input.join("source.m3u8"),
        "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nsegment.ts\n#EXT-X-ENDLIST\n",
    )
    .unwrap();
    let mut segment = [0_u8; 188];
    segment[0] = 0x47;
    std::fs::write(input.join("segment.ts"), segment).unwrap();
    let policy = FetchPolicy {
        allowed_local_roots: vec![input.clone()],
        ..FetchPolicy::default()
    };
    let probe = |_: &std::path::Path, _: Option<&std::path::Path>| {
        Ok(vec![serde_json::json!({"index":0,"codec_type":"video","codec_name":"h264","width":640,"height":360}).as_object().unwrap().clone()])
    };
    let options = AnalyzeOptions {
        on_log: None,
        prober: &probe,
        platform: Arc::new(drmr_core::source::platform::SystemPlatform),
        hooks: &Direct,
    };
    analyze_hls_with(
        &SourceRequest::new(input.join("source.m3u8").to_string_lossy()),
        "resource-test",
        &root.join("output"),
        &policy,
        &options,
    )
    .unwrap()
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
fn real_ports_register_verified_content_and_preserve_the_worker_upper_bound() {
    use drmr_core::events::NoSink;
    use drmr_core::hls_pipeline_deps::{PipelineDeps, Ports};
    use drmr_core::toolrun::{RunOptions, Secrets, ToolRunner};
    let directory = tempfile::tempdir().unwrap();
    let analysis = admitted_small_source(directory.path());
    let workload = Workload::from_analysis(&analysis).unwrap();
    assert_eq!(workload.max_pixels, Some(640 * 360));
    assert_eq!(workload.input_buffer_bytes, Some(188));
    assert_eq!(workload.tracks, 1);
    let claim = Claim::for_workload(workload, 16 * 1024 * 1024, 1024 * 1024);
    let runner = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
            .with_limiter(Arc::new(Limiter::new(1, claim.memory_bytes))),
    );
    let ports = Ports::new(Arc::clone(&runner));
    assert_eq!(ports.resource_workers(&analysis, 1).unwrap(), 1);
    // This allocation fits the actual small source but not the unknown 4K fallback.
    assert_eq!(
        runner
            .run_tool(
                &["python3".into(), "-c".into(), "print('complete')".into()],
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
                &["python3".into(), "-c".into(), "print('complete')".into()],
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
fn streamed_decoding_uses_the_same_admission_pool_as_captured_tools() {
    use drmr_core::events::{Collector, Sink};
    use drmr_core::media::{MediaEngine, MediaError, Subprocess};
    use drmr_core::toolrun::{Secrets, ToolRunner};
    let collector = Arc::new(Collector::default());
    let sink: Arc<dyn Sink> = collector.clone();
    let runner = Arc::new(
        ToolRunner::new(Arc::new(Secrets::new()), sink)
            .with_limiter(Arc::new(Limiter::new(0, u64::MAX))),
    );
    let engine = Subprocess::new(runner);
    let error = engine
        .decoded_digest(std::path::Path::new("not-opened.mp4"), "video", 0)
        .unwrap_err();
    assert!(matches!(error, MediaError::Tool(error) if error.kind == "resource-budget"));
    assert!(
        collector.records().is_empty(),
        "no command is started without a reservation"
    );
}

#[test]
fn a_cancelled_source_probe_does_not_start_an_unavailable_tool() {
    use drmr_core::events::NoSink;
    use drmr_core::source::{Failure, FfprobeProber, Prober};
    use drmr_core::toolrun::{Secrets, ToolRunner};
    let directory = tempfile::tempdir().unwrap();
    let segment = directory.path().join("segment.ts");
    std::fs::write(&segment, [0x47_u8; 188]).unwrap();
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let probe = FfprobeProber::with_program(&runner, "not-a-real-ffprobe-resource-test");
    let stop = Stop::new();
    stop.set();
    assert!(matches!(
        probe.probe_with_stop(&segment, None, &stop),
        Err(Failure::Cancelled)
    ));
}

#[test]
fn captured_media_reports_the_output_limit_instead_of_tool_unavailability() {
    use drmr_core::events::NoSink;
    use drmr_core::media::{MediaError, Subprocess};
    use drmr_core::toolrun::{Secrets, ToolRunner};
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)));
    let error = Subprocess::new(runner)
        .run_media(
            &[
                "python3".into(),
                "-c".into(),
                "print('x'*(20*1024*1024))".into(),
            ],
            false,
        )
        .unwrap_err();
    assert!(matches!(error, MediaError::Hls(error) if error.token() == "media-output-limit"));
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
