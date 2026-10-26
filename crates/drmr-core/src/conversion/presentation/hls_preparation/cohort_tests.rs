use super::*;
use crate::{
    concurrency::Stop,
    conversion::{Converter, execution::Scheduler},
    events::{Collector, Record, Sink},
    resources::{Claim, Hardware, Limiter},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

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
fn nested_rendition_and_segment_pools_share_one_native_worker_ceiling() {
    for maximum in [1, 2] {
        for renditions in [1, 2] {
            let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(crate::events::NoSink))
                .with_limiter(Arc::new(Limiter::new(4, 4 * 1024 * 1024 * 1024)));
            let converter = Converter::new(Arc::new(runner), "packager", "ffmpeg");
            let backend = preparation_backend(&converter.backend, Some(maximum));
            let release = Stop::new();
            let (entered, observed) = mpsc::sync_channel(4);
            std::thread::scope(|scope| {
                let worker = scope.spawn(|| {
                    let outer = Scheduler::observed(&backend, Some(maximum), hardware());
                    collect(&outer, &paths(renditions), |_| {
                        let inner = Scheduler::observed(&backend, Some(maximum), hardware());
                        inner.run(2, 1, 0, |_| {
                            let _permit = backend
                                .runner
                                .native_permit(
                                    Claim {
                                        cpu_threads: 1,
                                        memory_bytes: 128 * 1024 * 1024,
                                    },
                                    &backend.stop,
                                    Duration::from_secs(2),
                                )
                                .map_err(|_| ConversionError("nested-native-admission-failed"))?;
                            entered.send(()).unwrap();
                            if !release.wait(Some(Duration::from_secs(2))) {
                                return Err(ConversionError("finite-native-test-rescue"));
                            }
                            Ok(())
                        })
                    })
                });
                let mut started = Vec::new();
                for _ in 0..maximum {
                    started.push(observed.recv_timeout(Duration::from_secs(2)));
                }
                let extra = observed.recv_timeout(Duration::from_millis(30));
                release.set();
                let result = worker.join().unwrap().unwrap();
                assert!(started.iter().all(Result::is_ok));
                assert!(
                    extra.is_err(),
                    "nested HLS pools exceeded --max-workers={maximum}"
                );
                assert_eq!(result.len(), renditions * 2);
                assert_eq!(converter.backend.runner.native_capacity().0, 4);
            });
        }
    }
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

struct ReleaseOnDrop {
    attempts: Stop,
    outputs: Stop,
    cancellation: Stop,
}

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.cancellation.set();
        }
        self.attempts.set();
        self.outputs.set();
    }
}

struct AdmittedOutputs {
    records: Collector,
    entered: mpsc::SyncSender<()>,
    release: Stop,
    active: AtomicUsize,
    peak: AtomicUsize,
    timed_out: AtomicBool,
}

impl Sink for AdmittedOutputs {
    fn emit(&self, record: &Record) {
        self.records.emit(record);
        if record.get("message").and_then(serde_json::Value::as_str) != Some("tool-output")
            || record.get("returncode").and_then(serde_json::Value::as_i64) != Some(0)
        {
            return;
        }
        // ToolRunner emits this after child cleanup while still holding admission.
        // Command events precede admission and would also count queued callers.
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(active, Ordering::SeqCst);
        let _ = self.entered.send(());
        if !self.release.wait(Some(HANDSHAKE_TIMEOUT)) {
            self.timed_out.store(true, Ordering::SeqCst);
        }
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

fn receive_before(receiver: &mpsc::Receiver<()>, count: usize, deadline: Instant) -> bool {
    (0..count).all(|_| {
        receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .is_ok()
    })
}

#[test]
fn nested_decoded_tools_share_one_ceiling_for_single_and_multiple_renditions() {
    for maximum in [2, 1] {
        for renditions in [2, 1] {
            let owner = crate::native::private_tempdir().unwrap();
            let spec = drmr_test_support::ToolSpec {
                stdout: b"{}".to_vec(),
                ..Default::default()
            };
            let program = drmr_test_support::install(&owner.path().join("probe"), &spec).unwrap();
            let (entered, observed) = mpsc::sync_channel(4);
            let events = Arc::new(AdmittedOutputs {
                records: Collector::default(),
                entered,
                release: Stop::new(),
                active: AtomicUsize::new(0),
                peak: AtomicUsize::new(0),
                timed_out: AtomicBool::new(false),
            });
            let runner = ToolRunner::new(Arc::new(Secrets::new()), events.clone())
                .with_limiter(Arc::new(Limiter::new(4, 4 * 1024 * 1024 * 1024)));
            let converter = Converter::new(Arc::new(runner), "packager", "ffmpeg")
                .with_decode(true)
                .with_timeout(Duration::from_mins(1));
            let backend = preparation_backend(&converter.backend, Some(maximum));
            let first_wave = maximum.min(renditions) * maximum.min(2);
            let attempted = AtomicUsize::new(0);
            let attempts_release = Stop::new();
            let (attempts, attempts_observed) = mpsc::sync_channel(4);
            std::thread::scope(|scope| {
                let release = ReleaseOnDrop {
                    attempts: attempts_release.clone(),
                    outputs: events.release.clone(),
                    cancellation: backend.stop.clone(),
                };
                let worker = scope.spawn(|| {
                    let outer = Scheduler::observed(&backend, Some(maximum), hardware());
                    collect(&outer, &paths(renditions), |_| {
                        let inner = Scheduler::observed(&backend, Some(maximum), hardware());
                        inner.run(2, 1, 0, |_| {
                            if attempted.fetch_add(1, Ordering::SeqCst) < first_wave {
                                let _ = attempts.send(());
                                if !attempts_release.wait(Some(HANDSHAKE_TIMEOUT)) {
                                    return Err(ConversionError("finite-decoded-test-rescue"));
                                }
                            }
                            backend.run(&[program.to_str().unwrap().into()], "ffprobe", &[])?;
                            Ok(())
                        })
                    })
                });
                let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
                let all_attempted = receive_before(&attempts_observed, first_wave, deadline);
                attempts_release.set();
                let all_admitted = all_attempted && receive_before(&observed, maximum, deadline);
                if !all_admitted {
                    backend.stop.set();
                }
                drop(release);
                let result = worker.join().unwrap().unwrap();
                assert!(
                    all_attempted,
                    "nested tool callers did not reach their start gate"
                );
                assert!(
                    all_admitted,
                    "decoded tools did not acquire their worker slots"
                );
                assert_eq!(
                    events.peak.load(Ordering::SeqCst),
                    maximum,
                    "decoded nested HLS pools exceeded their worker ceiling"
                );
                assert!(!events.timed_out.load(Ordering::SeqCst));
                assert_eq!(events.active.load(Ordering::SeqCst), 0);
                assert_eq!(result.len(), renditions * 2);
                let records = events.records.records();
                for message in ["command", "tool-output"] {
                    assert_eq!(
                        records
                            .iter()
                            .filter(|record| record["message"] == message)
                            .count(),
                        renditions * 2
                    );
                }
                assert_eq!(backend.runner.native_capacity().0, maximum);
                assert_eq!(converter.backend.runner.native_capacity().0, 4);
            });
        }
    }
}
