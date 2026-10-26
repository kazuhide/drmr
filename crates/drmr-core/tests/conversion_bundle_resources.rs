//! Shared admission includes the actual final manifest, not only media-tool estimates.
use drmr_core::{
    concurrency::Stop,
    conversion::{
        Converter, HlsInputKeys, InputKeys, VerificationSource,
        conformance::ManifestValidationTools,
        key_file::BundleOperatorKeys,
        plan::bundle::{BundleOutputIntent, BundlePlanRequest, ManifestValidation},
    },
    events::{NoSink, Record, Sink},
    inspection::{OutputFormat, TargetProtection},
    resources::{Claim, Limiter, Workload},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

struct LintGate {
    started: mpsc::SyncSender<()>,
    planned: Option<mpsc::SyncSender<()>>,
    release: Option<Stop>,
    seen: AtomicBool,
}
impl Sink for LintGate {
    fn emit(&self, record: &Record) {
        if record.get("message").and_then(serde_json::Value::as_str) == Some("resource-budget")
            && record
                .get("estimated_child_bytes")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|bytes| bytes > 220 * 1024 * 1024)
            && let Some(planned) = &self.planned
        {
            let _ = planned.try_send(());
        }
        if record.get("event").and_then(serde_json::Value::as_str) == Some("task")
            && record.get("task").and_then(serde_json::Value::as_str) == Some("manifest-lint")
            && record.get("state").and_then(serde_json::Value::as_str) == Some("started")
            && !self.seen.swap(true, Ordering::AcqRel)
        {
            let _ = self.started.send(());
            if let Some(release) = &self.release {
                let _ = release.wait(Some(Duration::from_secs(10)));
            }
        }
    }
}

fn command(limiter: &Arc<Limiter>, sink: Arc<dyn Sink>) -> Converter {
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::clone(&sink))
        .with_limiter(Arc::clone(limiter));
    runner.register_workload(Workload {
        max_pixels: Some(1920 * 1080),
        input_buffer_bytes: Some(64 * 1024),
        tracks: 1,
    });
    Converter::new(Arc::new(runner), "packager", "missing-decoder")
        .with_activity_sink(sink)
        .with_ffprobe("missing-probe")
        .with_max_workers(Some(1))
        .unwrap()
        .with_timeout(Duration::from_secs(15))
}

fn source(root: &std::path::Path) -> (std::path::PathBuf, Vec<u8>) {
    let input = root.join("source.mp4");
    let clip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb/clip.mp4");
    let created = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(clip)
        .args([
            "-map",
            "0:v:0",
            "-t",
            "2",
            "-c",
            "copy",
            "-an",
            "-movflags",
            "delay_moov+default_base_moof+frag_keyframe",
        ])
        .arg(&input)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "short MP4 fixture creation failed"
    );
    let original = std::fs::read(&input).unwrap();
    (input, original)
}

fn assert_pool_released(limiter: &Arc<Limiter>, bytes: u64) {
    drop(
        limiter
            .acquire(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: bytes,
                },
                None,
                Some(Duration::from_secs(1)),
            )
            .unwrap(),
    );
}

#[test]
fn fresh_public_bundle_verifications_queue_for_a_large_valid_control_manifest() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let (input, original) = source(root.path());
    let limiter = Arc::new(Limiter::new(1, 544 * 1024 * 1024));
    let setup = command(&limiter, Arc::new(NoSink));
    let keys = BundleOperatorKeys::default();
    let tools = ManifestValidationTools::default();
    let plan = setup
        .create_bundle_plan(BundlePlanRequest {
            input: VerificationSource {
                input: &input,
                input_keys: &InputKeys::default(),
                hls_keys: &HlsInputKeys::default(),
            },
            output_root: &root.path().join("result"),
            outputs: &[BundleOutputIntent {
                dash_addressing: None,
                id: "hls".into(),
                format: OutputFormat::Hls,
                target: TargetProtection::Clear,
                drms: Vec::new(),
                conformance: ManifestValidation::None,
                processing: None,
            }],
        })
        .unwrap();
    setup.execute_bundle_plan(&plan, &keys, &tools).unwrap();
    let manifest = plan.output_root.join("hls/master.m3u8");
    let mut text = std::fs::read_to_string(&manifest).unwrap();
    text.push_str("\n#");
    text.push_str(&"comment".repeat(512 * 1024));
    text.push('\n');
    std::fs::write(&manifest, text).unwrap();
    let release = Stop::new();
    let (first_started, first_seen) = mpsc::sync_channel(1);
    let (second_started, second_seen) = mpsc::sync_channel(1);
    let (planned, observed_plan) = mpsc::sync_channel(4);
    let first = command(
        &limiter,
        Arc::new(LintGate {
            started: first_started,
            planned: None,
            release: Some(release.clone()),
            seen: AtomicBool::new(false),
        }),
    );
    let second = command(
        &limiter,
        Arc::new(LintGate {
            started: second_started,
            planned: Some(planned),
            release: None,
            seen: AtomicBool::new(false),
        }),
    );
    std::thread::scope(|scope| {
        let one = scope.spawn(|| first.verify_bundle_plan(&plan, &keys, &tools));
        let first_ready = first_seen.recv_timeout(Duration::from_secs(5));
        let two = scope.spawn(|| second.verify_bundle_plan(&plan, &keys, &tools));
        let second_planned = observed_plan.recv_timeout(Duration::from_secs(5));
        let premature = second_seen.recv_timeout(Duration::from_millis(50));
        release.set();
        let one = one.join().unwrap();
        let two = two.join().unwrap();
        assert!(
            first_ready.is_ok() && second_planned.is_ok(),
            "public verification did not reach its gates: first={first_ready:?}, second={second_planned:?}, one={:?}, two={:?}",
            one.as_ref()
                .map(|report| report.passed)
                .map_err(|error| error.0),
            two.as_ref()
                .map(|report| report.passed)
                .map_err(|error| error.0)
        );
        assert!(
            premature.is_err(),
            "another parent consumed the first linter's headroom"
        );
        for result in [one, two] {
            let report = result.unwrap();
            assert!(report.passed && report.source_unchanged);
            assert!(report.outputs[0].verification.checks.is_pass());
            assert!(
                report.outputs[0]
                    .verification
                    .native_manifest
                    .as_ref()
                    .unwrap()
                    .is_clean_for_scope()
            );
        }
    });
    assert_eq!(std::fs::read(input).unwrap(), original);
    assert_pool_released(&limiter, 544 * 1024 * 1024);
}
