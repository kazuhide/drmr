use super::*;
use crate::{conversion::InputKeys, events::Sink};

struct BudgetSeen(mpsc::SyncSender<()>);
impl Sink for BudgetSeen {
    fn emit(&self, record: &crate::events::Record) {
        if record.get("message").and_then(serde_json::Value::as_str) == Some("resource-budget") {
            let _ = self.0.try_send(());
        }
    }
}

fn shared_command(limiter: &Arc<Limiter>, sink: Arc<dyn Sink>) -> Converter {
    let runner = ToolRunner::new(Arc::new(Secrets::new()), sink).with_limiter(Arc::clone(limiter));
    Converter::new(Arc::new(runner), "packager", "ffmpeg")
        .with_max_workers(Some(1))
        .unwrap()
        .with_timeout(Duration::from_secs(5))
}

fn known_command(limiter: &Arc<Limiter>, sink: Arc<dyn Sink>) -> Converter {
    let command = shared_command(limiter, sink).bundle_cohort();
    command.backend.runner.register_workload(Workload {
        max_pixels: Some(1280 * 720),
        input_buffer_bytes: Some(64 * 1024),
        tracks: 1,
    });
    command
}

#[test]
fn an_independent_decoded_mp4_verification_waits_for_another_commands_context() {
    let root = crate::native::private_tempdir().unwrap();
    let fixture =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let input = root.path().join("video.mp4");
    let created = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture)
        .args(["-map", "0:v:0", "-t", "0.2", "-c", "copy"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "short MP4 fixture creation failed"
    );
    let limiter = Arc::new(Limiter::new(1, 512 * 1024 * 1024));
    let first = known_command(&limiter, Arc::new(NoSink));
    let second = shared_command(&limiter, Arc::new(NoSink)).with_decode(true);
    let (entered, ready_context) = mpsc::sync_channel(1);
    let (finished, result) = mpsc::sync_channel(1);
    let release = Stop::new();
    std::thread::scope(|scope| {
        let parent = scope.spawn(|| {
            observed(&first).run(&plan(1), |_| {
                entered.send(()).unwrap();
                if !release.wait(Some(Duration::from_secs(5))) {
                    return Err(ConversionError("finite-context-rescue"));
                }
                Ok(())
            })
        });
        let ready = ready_context.recv_timeout(Duration::from_secs(2));
        let job = scope.spawn(|| {
            let result =
                second.verify_mp4(&input, &InputKeys::default(), &input, &InputKeys::default());
            finished.send(result).unwrap();
        });
        let premature = result.recv_timeout(Duration::from_millis(100));
        release.set();
        let parent = parent.join().unwrap();
        let parent_ok = parent.is_ok();
        drop(parent);
        job.join().unwrap();
        let outcome = premature.as_ref().map(|result| {
            result
                .as_ref()
                .map(|report| report.passed)
                .map_err(|error| error.0)
        });
        assert!(ready.is_ok() && parent_ok);
        assert!(
            premature.is_err(),
            "independent job did not queue: {outcome:?}"
        );
        assert!(
            result
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap()
                .passed
        );
    });
    assert_eq!(limiter.capacity(), (1, 512 * 1024 * 1024));
}

#[derive(Debug)]
struct BlockingCleanup {
    entered: mpsc::SyncSender<()>,
    release: Stop,
    finished: Arc<AtomicBool>,
}
impl Drop for BlockingCleanup {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        let _ = self.release.wait(Some(Duration::from_secs(5)));
        self.finished.store(true, Ordering::Release);
    }
}

#[cfg(unix)]
#[test]
fn a_final_destination_refusal_holds_context_until_result_cleanup_finishes() {
    let root = crate::native::private_tempdir().unwrap();
    let parent = root.path().join("parent");
    std::fs::create_dir(&parent).unwrap();
    let parent = parent.canonicalize().unwrap();
    let destination =
        crate::conversion::plan::destination::OutputGuard::new(&parent.join("output")).unwrap();
    let limiter = Arc::new(Limiter::new(1, 544 * 1024 * 1024));
    let first = shared_command(&limiter, Arc::new(NoSink)).bundle_cohort();
    let (budget, planned) = mpsc::sync_channel(4);
    let second = shared_command(&limiter, Arc::new(BudgetSeen(budget))).bundle_cohort();
    let release = Stop::new();
    let finished = Arc::new(AtomicBool::new(false));
    let (cleanup, cleaning) = mpsc::sync_channel(1);
    let (admitted, observed_admission) = mpsc::sync_channel(1);
    std::thread::scope(|scope| {
        let one = scope.spawn(|| -> Result<(), ConversionError> {
            let _retained;
            let plan = plan(1);
            let (execution, retained) = observed(&first).prepare::<BlockingCleanup>(&plan, None)?;
            _retained = retained;
            let completed = execution.run(&plan, |_| {
                std::fs::rename(&parent, root.path().join("held-parent")).unwrap();
                std::fs::create_dir(&parent).unwrap();
                Ok(BlockingCleanup {
                    entered: cleanup.clone(),
                    release: release.clone(),
                    finished: Arc::clone(&finished),
                })
            });
            destination.check()?;
            let _completed = completed?;
            Ok(())
        });
        let cleanup_started = cleaning.recv_timeout(Duration::from_secs(2));
        let two = scope.spawn(|| {
            observed(&second).run(&plan(1), |_| {
                admitted.send(finished.load(Ordering::Acquire)).unwrap();
                Ok(())
            })
        });
        let ready = planned.recv_timeout(Duration::from_secs(2));
        let premature = observed_admission.recv_timeout(Duration::from_millis(40));
        release.set();
        let one = one.join().unwrap();
        let two = two.join().unwrap();
        assert!(cleanup_started.is_ok() && ready.is_ok());
        assert_eq!(
            one.unwrap_err(),
            ConversionError("execution-plan-destination-unsafe")
        );
        assert!(two.is_ok() && finished.load(Ordering::Acquire));
        assert!(
            premature.is_err(),
            "context escaped while output inventory cleanup was active"
        );
        assert!(
            observed_admission
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
        );
    });
    assert_eq!(limiter.capacity(), (1, 544 * 1024 * 1024));
}

#[test]
fn concurrent_native_parents_leave_room_for_large_valid_output_manifests() {
    let root = crate::native::private_tempdir().unwrap();
    let mut playlist = String::from("#EXTM3U\n#");
    playlist.push_str(&"comment".repeat(512 * 1024));
    playlist.push_str(
        "\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n",
    );
    let plan = output_plan(root.path(), &playlist);
    let limiter = Arc::new(Limiter::new(1, 544 * 1024 * 1024));
    let first = known_command(&limiter, Arc::new(NoSink));
    let (budget, planned) = mpsc::sync_channel(4);
    let second = known_command(&limiter, Arc::new(BudgetSeen(budget)));
    for command in [&first, &second] {
        command.backend.runner.register_workload(Workload {
            max_pixels: Some(1920 * 1080),
            input_buffer_bytes: Some(64 * 1024),
            tracks: 1,
        });
    }
    let release = Stop::new();
    let (entered, ready) = mpsc::sync_channel(2);
    let run = |command: &Converter| run_known(command, &plan, &entered, &release);
    std::thread::scope(|scope| {
        let one = scope.spawn(|| run(&first));
        let first_ready = ready.recv_timeout(Duration::from_secs(2));
        let two = scope.spawn(|| run(&second));
        let second_planned = planned.recv_timeout(Duration::from_secs(2));
        let premature = ready.recv_timeout(Duration::from_millis(40));
        release.set();
        let one = one.join().unwrap();
        let two = two.join().unwrap();
        assert!(first_ready.is_ok() && second_planned.is_ok());
        assert!(
            one.is_ok() && two.is_ok(),
            "large manifests were incorrectly refused: first={one:?}, second={two:?}"
        );
        assert!(
            premature.is_err(),
            "the second parent consumed the first linter's headroom"
        );
    });
    assert_eq!(limiter.capacity(), (1, 544 * 1024 * 1024));
}

fn output_plan(root: &std::path::Path, playlist: &str) -> BundlePlan {
    let mut plan = plan(1);
    plan.output_root = root.join("result");
    let directory = plan.output_root.join("output-0");
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::write(directory.join("master.m3u8"), playlist).unwrap();
    std::fs::write(directory.join("segment.ts"), b"bounded media fixture").unwrap();
    plan
}

fn run_known(
    command: &Converter,
    plan: &BundlePlan,
    entered: &mpsc::SyncSender<()>,
    release: &Stop,
) -> Result<(), ConversionError> {
    let _retained;
    let manifests = super::super::super::outputs::ManifestDemands::capture(command, plan)?;
    let (execution, retained) =
        observed(command).prepare::<frozen::FrozenSource>(plan, Some(&manifests))?;
    _retained = retained;
    let results = execution.run(plan, |ordinal| {
        entered.send(()).unwrap();
        if !release.wait(Some(Duration::from_secs(5))) {
            return Err(ConversionError("finite-lint-rescue"));
        }
        let result = execution
            .command
            .capture_source(&super::super::super::result_entrypoint(
                &plan.output_root,
                &plan.outputs[ordinal],
            ))?;
        let checked = manifests.bind(&execution.command, &result, ordinal)?;
        let report = execution
            .command
            .lint_manifest(result.entrypoint(), OutputFormat::Hls)?;
        assert!(report.is_clean_for_scope());
        checked.unwrap().check(&execution.command)?;
        Ok(result)
    })?;
    assert_eq!(results.len(), plan.outputs.len());
    Ok(())
}

#[test]
fn known_small_manifest_parents_still_overlap_on_the_same_pool() {
    let root = crate::native::private_tempdir().unwrap();
    let plan = output_plan(
        root.path(),
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n",
    );
    let limiter = Arc::new(Limiter::new(1, 544 * 1024 * 1024));
    let first = known_command(&limiter, Arc::new(NoSink));
    let second = known_command(&limiter, Arc::new(NoSink));
    let release = Stop::new();
    let (entered, observed) = mpsc::sync_channel(2);
    std::thread::scope(|scope| {
        let one = scope.spawn(|| run_known(&first, &plan, &entered, &release));
        let first_ready = observed.recv_timeout(Duration::from_secs(2));
        let two = scope.spawn(|| run_known(&second, &plan, &entered, &release));
        let second_ready = observed.recv_timeout(Duration::from_secs(2));
        release.set();
        let one = one.join().unwrap();
        let two = two.join().unwrap();
        assert!(
            first_ready.is_ok() && second_ready.is_ok(),
            "known independent parents were serialized"
        );
        assert!(
            one.is_ok() && two.is_ok(),
            "known parent verification failed: first={one:?}, second={two:?}"
        );
    });
    assert_eq!(limiter.capacity(), (1, 544 * 1024 * 1024));
}

#[test]
fn unknown_parents_queue_without_serializing_their_parallel_output_workers() {
    let limiter = Arc::new(Limiter::new(4, 2 * 1024 * 1024 * 1024));
    let command = |sink| {
        shared_command(&limiter, sink)
            .with_max_workers(Some(2))
            .unwrap()
            .bundle_cohort()
    };
    let first = command(Arc::new(NoSink));
    let (budget, planned) = mpsc::sync_channel(4);
    let second = command(Arc::new(BudgetSeen(budget)));
    for command in [&first, &second] {
        command.backend.runner.register_workload(Workload {
            max_pixels: Some(1280 * 720),
            input_buffer_bytes: Some(64 * 1024),
            tracks: 1,
        });
    }
    let release = Stop::new();
    let (entered, observed_entry) = mpsc::sync_channel(4);
    let run = |command: &Converter, number: usize| -> Result<(), ConversionError> {
        let _retained;
        let plan = plan(2);
        let (execution, retained) = observed(command).prepare::<usize>(&plan, None)?;
        _retained = retained;
        let results = execution.run(&plan, |ordinal| {
            let _leaf = execution
                .command
                .backend
                .runner
                .native_permit(
                    crate::resources::Claim {
                        cpu_threads: 1,
                        memory_bytes: 128 * 1024 * 1024,
                    },
                    &execution.command.backend.stop,
                    Duration::from_secs(2),
                )
                .map_err(|_| ConversionError("output-leaf-admission-failed"))?;
            entered.send(number).unwrap();
            if !release.wait(Some(Duration::from_secs(5))) {
                return Err(ConversionError("finite-output-rescue"));
            }
            Ok(ordinal)
        })?;
        assert_eq!(results, [0, 1]);
        Ok(())
    };
    std::thread::scope(|scope| {
        let one = scope.spawn(|| run(&first, 1));
        let first_one = observed_entry.recv_timeout(Duration::from_secs(2));
        let first_two = observed_entry.recv_timeout(Duration::from_secs(2));
        let two = scope.spawn(|| run(&second, 2));
        let ready = planned.recv_timeout(Duration::from_secs(2));
        let premature = observed_entry.recv_timeout(Duration::from_millis(40));
        release.set();
        let one = one.join().unwrap();
        let two = two.join().unwrap();
        assert_eq!(first_one.unwrap(), 1);
        assert_eq!(first_two.unwrap(), 1);
        assert!(ready.is_ok() && premature.is_err());
        assert!(
            one.is_ok() && two.is_ok(),
            "unknown parent failed: first={one:?}, second={two:?}"
        );
        assert_eq!(
            observed_entry.recv_timeout(Duration::from_secs(2)).unwrap(),
            2
        );
        assert_eq!(
            observed_entry.recv_timeout(Duration::from_secs(2)).unwrap(),
            2
        );
    });
    assert_eq!(limiter.capacity(), (4, 2 * 1024 * 1024 * 1024));
}

#[test]
fn growing_a_known_manifest_is_refused_before_oversized_linter_admission() {
    let _retained;
    let root = crate::native::private_tempdir().unwrap();
    let text = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n";
    let plan = output_plan(root.path(), text);
    let limiter = Arc::new(Limiter::new(1, 544 * 1024 * 1024));
    let command = known_command(&limiter, Arc::new(NoSink));
    let manifests =
        super::super::super::outputs::ManifestDemands::capture(&command, &plan).unwrap();
    let (execution, retained) = observed(&command)
        .prepare::<()>(&plan, Some(&manifests))
        .unwrap();
    _retained = retained;
    let path = super::super::super::result_entrypoint(&plan.output_root, &plan.outputs[0]);
    let grown = format!("{text}#{}\n", "comment".repeat(560 * 1024));
    std::fs::write(&path, grown).unwrap();
    let result = execution.command.capture_source(&path).unwrap();
    let failed = manifests.bind(&execution.command, &result, 0);
    assert_eq!(
        failed.err().unwrap(),
        ConversionError("verification-output-changed")
    );
}

#[test]
fn source_owner_cleanup_keeps_its_lease_on_guard_failure_and_unwind() {
    for unwind in [false, true] {
        let limiter = Arc::new(Limiter::new(1, 544 * 1024 * 1024));
        let first = shared_command(&limiter, Arc::new(NoSink)).bundle_cohort();
        let (budget, planned) = mpsc::sync_channel(4);
        let second = shared_command(&limiter, Arc::new(BudgetSeen(budget))).bundle_cohort();
        let release = Stop::new();
        let finished = Arc::new(AtomicBool::new(false));
        let (cleanup, cleaning) = mpsc::sync_channel(1);
        let (admitted, observed_admission) = mpsc::sync_channel(1);
        std::thread::scope(|scope| {
            let one = scope.spawn(|| {
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                    || -> Result<(), ConversionError> {
                        let _retained;
                        // The source owner predates admission, just as reopened
                        // source inventories do in the public bundle entrypoints.
                        let _source = BlockingCleanup {
                            entered: cleanup.clone(),
                            release: release.clone(),
                            finished: Arc::clone(&finished),
                        };
                        let plan = plan(1);
                        let (execution, retained) =
                            observed(&first).prepare::<usize>(&plan, None)?;
                        _retained = retained;
                        let _results = execution.run(&plan, Ok)?;
                        assert!(!unwind, "synthetic final callback panic");
                        Err(ConversionError("planned-source-changed"))
                    },
                ))
            });
            let ready_cleanup = cleaning.recv_timeout(Duration::from_secs(2));
            let two = scope.spawn(|| {
                observed(&second).run(&plan(1), |_| {
                    admitted.send(finished.load(Ordering::Acquire)).unwrap();
                    Ok(())
                })
            });
            let ready_budget = planned.recv_timeout(Duration::from_secs(2));
            let premature = observed_admission.recv_timeout(Duration::from_millis(40));
            release.set();
            let one = one.join().unwrap();
            let two = two.join().unwrap();
            assert!(ready_cleanup.is_ok() && ready_budget.is_ok() && premature.is_err());
            if unwind {
                assert!(one.is_err());
            } else {
                assert_eq!(
                    one.unwrap().unwrap_err(),
                    ConversionError("planned-source-changed")
                );
            }
            assert!(
                two.is_ok()
                    && observed_admission
                        .recv_timeout(Duration::from_secs(2))
                        .unwrap()
            );
        });
        assert_eq!(limiter.capacity(), (1, 544 * 1024 * 1024));
    }
}

#[test]
fn a_later_preflight_error_does_not_hide_an_earlier_output_failure() {
    let root = crate::native::private_tempdir().unwrap();
    let limiter = Arc::new(Limiter::new(1, 544 * 1024 * 1024));
    let command = known_command(&limiter, Arc::new(NoSink));
    let mut plan = plan(2);
    plan.output_root = root.path().join("result");
    let first = plan.output_root.join("output-0");
    std::fs::create_dir_all(&first).unwrap();
    let input = first.join("master.m3u8");
    std::fs::write(&input, b"invalid output media").unwrap();
    let expected = command.capture_source(&input).unwrap_err();
    let result = (|| -> Result<Vec<()>, ConversionError> {
        let _retained;
        let manifests = super::super::super::outputs::ManifestDemands::capture(&command, &plan)?;
        let (execution, retained) = observed(&command).prepare::<()>(&plan, Some(&manifests))?;
        _retained = retained;
        execution.run(&plan, |ordinal| {
            let source =
                execution
                    .command
                    .capture_source(&super::super::super::result_entrypoint(
                        &plan.output_root,
                        &plan.outputs[ordinal],
                    ))?;
            manifests.bind(&execution.command, &source, ordinal)?;
            Ok(())
        })
    })();
    assert_eq!(result.unwrap_err(), expected);
}
