use super::*;
use crate::{events::Sink, resources::Claim, toolrun::RunOptions};

struct BudgetSeen(mpsc::SyncSender<()>);
impl Sink for BudgetSeen {
    fn emit(&self, record: &crate::events::Record) {
        if record.get("message").and_then(serde_json::Value::as_str) == Some("resource-budget") {
            let _ = self.0.try_send(());
        }
    }
}

fn command(limiter: &Arc<Limiter>, sink: Arc<dyn Sink>) -> Converter {
    let runner = ToolRunner::new(Arc::new(Secrets::new()), sink).with_limiter(Arc::clone(limiter));
    Converter::new(Arc::new(runner), "packager", "ffmpeg")
        .with_decode(true)
        .with_max_workers(Some(1))
        .unwrap()
        .with_timeout(Duration::from_secs(3))
        .bundle_cohort()
}

fn decoded_plan() -> BundlePlan {
    let mut plan = plan(1);
    plan.source_format = OutputFormat::HlsTs;
    plan.outputs[0].format = OutputFormat::HlsTs;
    plan.required_depth = Some(crate::conversion::ValidationDepth::Decoded);
    plan
}

fn run_parent(
    command: &Converter,
    plan: &BundlePlan,
    probe: &std::path::Path,
    entered: &mpsc::SyncSender<()>,
    allow_probe: &Stop,
) -> Result<(), ConversionError> {
    let (retained, output) = observed(command).run(plan, |_| {
        entered.send(()).unwrap();
        if !allow_probe.wait(Some(Duration::from_secs(5))) {
            return Err(ConversionError("finite-parent-rescue"));
        }
        let options = RunOptions {
            // Exactly the reservation made by decoded TS canonical capture;
            // the fixture itself emits only a few bytes.
            max_stdout_bytes: crate::toolrun::MAX_CAPTURE_BYTES,
            timeout: Some(Duration::from_secs(2)),
            ..RunOptions::default()
        };
        let output = command
            .backend
            .runner
            .run_tool(
                &[probe.to_str().unwrap().into()],
                "canonical-fixture",
                &options,
            )
            .map_err(|_| ConversionError("canonical-reservation-failed"))?;
        assert_eq!(output.stdout, b"canonical");
        Ok(())
    })?;
    assert_eq!(output.len(), 1);
    drop(retained);
    Ok(())
}

#[test]
fn concurrent_decoded_parents_wait_for_actual_large_capture_claims() {
    let root = crate::native::private_tempdir().unwrap();
    let probe = drmr_test_support::install(
        &root.path().join("probe"),
        &drmr_test_support::ToolSpec {
            stdout: b"canonical".to_vec(),
            ..Default::default()
        },
    )
    .unwrap();
    let limiter = Arc::new(Limiter::new(1, 768 * 1024 * 1024));
    let first = command(&limiter, Arc::new(NoSink));
    let (budget, observed_budget) = mpsc::sync_channel(4);
    let second = command(&limiter, Arc::new(BudgetSeen(budget)));
    let plan = decoded_plan();
    let (first_entered, first_observed) = mpsc::sync_channel(1);
    let (second_entered, second_observed) = mpsc::sync_channel(1);
    let allow_probe = Stop::new();
    std::thread::scope(|scope| {
        let one = scope.spawn(|| run_parent(&first, &plan, &probe, &first_entered, &allow_probe));
        let first_ready = first_observed.recv_timeout(Duration::from_secs(2));
        let two = scope.spawn(|| run_parent(&second, &plan, &probe, &second_entered, &allow_probe));
        let planned = observed_budget.recv_timeout(Duration::from_secs(2));
        let premature = second_observed.recv_timeout(Duration::from_millis(40));
        allow_probe.set();
        let one = one.join().unwrap();
        let two = two.join().unwrap();
        assert!(first_ready.is_ok() && planned.is_ok());
        assert!(
            one.is_ok() && two.is_ok(),
            "large captures were incorrectly refused: first={one:?}, second={two:?}"
        );
        assert!(
            premature.is_err(),
            "the second parent consumed the first parent's leaf headroom"
        );
    });
    assert_eq!(limiter.capacity(), (1, 768 * 1024 * 1024));
    let actual = Claim::for_workload(
        Workload::default(),
        crate::toolrun::MAX_CAPTURE_BYTES,
        crate::toolrun::DEFAULT_STDERR_BYTES,
    );
    assert!(actual.memory_bytes > 512 * 1024 * 1024);
}

#[test]
fn only_decoded_work_reserves_the_larger_capture_floor() {
    let workload = Workload::default();
    let plan = plan(1);
    let native =
        retained::<()>(&plan, workload, crate::conversion::ValidationDepth::Native).unwrap();
    let decoded =
        retained::<()>(&plan, workload, crate::conversion::ValidationDepth::Decoded).unwrap();
    let normal = Claim::for_workload(
        workload,
        crate::toolrun::DEFAULT_STDOUT_BYTES,
        crate::toolrun::DEFAULT_STDERR_BYTES,
    )
    .memory_bytes
    .max(NATIVE_LEAF_BYTES);
    let large = Claim::for_workload(
        workload,
        crate::toolrun::MAX_CAPTURE_BYTES,
        crate::toolrun::DEFAULT_STDERR_BYTES,
    )
    .memory_bytes;
    assert_eq!(native.leaf, normal);
    assert_eq!(decoded.leaf, large);
    assert_eq!(decoded.leaf - native.leaf, 192 * 1024 * 1024);
}

#[test]
fn a_cancelled_waiting_parent_does_not_consume_the_first_parents_headroom() {
    let root = crate::native::private_tempdir().unwrap();
    let probe = drmr_test_support::install(
        &root.path().join("probe"),
        &drmr_test_support::ToolSpec {
            stdout: b"canonical".to_vec(),
            ..Default::default()
        },
    )
    .unwrap();
    let limiter = Arc::new(Limiter::new(1, 768 * 1024 * 1024));
    let first = command(&limiter, Arc::new(NoSink));
    let (budget, observed_budget) = mpsc::sync_channel(4);
    let second = command(&limiter, Arc::new(BudgetSeen(budget)));
    let plan = decoded_plan();
    let (first_entered, first_observed) = mpsc::sync_channel(1);
    let (second_entered, second_observed) = mpsc::sync_channel(1);
    let allow_probe = Stop::new();
    std::thread::scope(|scope| {
        let one = scope.spawn(|| run_parent(&first, &plan, &probe, &first_entered, &allow_probe));
        let first_ready = first_observed.recv_timeout(Duration::from_secs(2));
        let two = scope.spawn(|| run_parent(&second, &plan, &probe, &second_entered, &allow_probe));
        let planned = observed_budget.recv_timeout(Duration::from_secs(2));
        let premature = second_observed.recv_timeout(Duration::from_millis(40));
        second.backend.stop.set();
        let two = two.join().unwrap();
        allow_probe.set();
        let one = one.join().unwrap();
        assert!(first_ready.is_ok() && planned.is_ok() && premature.is_err());
        assert_eq!(two.unwrap_err(), ConversionError("cancelled"));
        assert!(one.is_ok() && !first.backend.stop.is_set());
    });
    assert_eq!(limiter.capacity(), (1, 768 * 1024 * 1024));
}
