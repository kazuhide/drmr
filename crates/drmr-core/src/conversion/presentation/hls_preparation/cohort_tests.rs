use super::*;
use crate::{
    concurrency::Stop,
    conversion::{Converter, execution::Scheduler},
    resources::{Claim, Hardware, Limiter},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, mpsc},
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

fn ready_children(root: &Path) -> usize {
    fs::read_dir(root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.path().join("complete").is_file())
        .count()
}

#[test]
fn nested_decoded_tools_share_one_ceiling_for_single_and_multiple_renditions() {
    for maximum in [1, 2] {
        for renditions in [1, 2] {
            let owner = crate::native::private_tempdir().unwrap();
            let calls = owner.path().join("calls");
            fs::create_dir(&calls).unwrap();
            let release = owner.path().join("release");
            let spec = drmr_test_support::ToolSpec {
                stdout: b"{}".to_vec(),
                ..Default::default()
            }
            .with("invocation_root", calls.to_str().unwrap())
            .with("release_file", release.to_str().unwrap());
            let program = drmr_test_support::install(&owner.path().join("probe"), &spec).unwrap();
            let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(crate::events::NoSink))
                .with_limiter(Arc::new(Limiter::new(4, 4 * 1024 * 1024 * 1024)));
            let converter = Converter::new(Arc::new(runner), "packager", "ffmpeg")
                .with_decode(true)
                .with_timeout(Duration::from_secs(5));
            let backend = preparation_backend(&converter.backend, Some(maximum));
            std::thread::scope(|scope| {
                let worker = scope.spawn(|| {
                    let outer = Scheduler::observed(&backend, Some(maximum), hardware());
                    collect(&outer, &paths(renditions), |_| {
                        let inner = Scheduler::observed(&backend, Some(maximum), hardware());
                        inner.run(2, 1, 0, |_| {
                            backend.run(&[program.to_str().unwrap().into()], "ffprobe", &[])?;
                            Ok(())
                        })
                    })
                });
                let deadline = Instant::now() + Duration::from_secs(3);
                while ready_children(&calls) < maximum && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                std::thread::sleep(Duration::from_millis(30));
                let held = ready_children(&calls);
                fs::write(&release, b"release").unwrap();
                let result = worker.join().unwrap().unwrap();
                assert_eq!(
                    held, maximum,
                    "decoded nested HLS pools exceeded their worker ceiling"
                );
                assert_eq!(result.len(), renditions * 2);
                assert_eq!(ready_children(&calls), renditions * 2);
                assert_eq!(converter.backend.runner.native_capacity().0, 4);
            });
        }
    }
}
