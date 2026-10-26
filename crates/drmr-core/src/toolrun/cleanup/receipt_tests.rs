use super::*;
use crate::{
    events::{NoSink, Record, Sink},
    toolrun::{RunOptions, Secrets, ToolRunner},
};
use std::{cell::Cell, path::Path, time::Duration};

fn isolated_runner(owner: &Arc<CleanupOwner>, sink: Arc<dyn Sink>) -> ToolRunner {
    let options = RunOptions::default();
    let claim = crate::resources::Claim::for_workload(
        crate::resources::Workload::default(),
        options.max_stdout_bytes,
        options.max_stderr_bytes,
    );
    ToolRunner::new(Arc::new(Secrets::new()), sink)
        .with_cleanup_owner(owner.clone())
        .with_limiter(Arc::new(crate::resources::Limiter::new(
            claim.cpu_threads,
            claim.memory_bytes,
        )))
}

fn retain(runner: &ToolRunner, root: &Path) -> CleanupReceipt {
    let ready = root.join("retained-ready");
    let program = drmr_test_support::install(
        &root.join("retained"),
        &drmr_test_support::ToolSpec {
            stdout: vec![b'\n'],
            sleep_ms: 60_000,
            ..drmr_test_support::ToolSpec::default()
        }
        .with("ready_file", ready.to_str().unwrap()),
    )
    .unwrap();
    let mut receipt = None;
    let failed = runner.run_tool_injected_cleanup_with_receipt(
        &[program.to_str().unwrap().into()],
        &RunOptions {
            max_stdout_bytes: 0,
            ..RunOptions::default()
        },
        &mut receipt,
        |_| {
            assert!(ready.is_file());
            Err(io::Error::other("injected cleanup uncertainty"))
        },
    );
    assert_eq!(failed.unwrap_err().kind, "cleanup-unconfirmed");
    assert!(ready.is_file());
    receipt.unwrap()
}
fn alive(owner: &CleanupOwner) {
    let mut slot = owner.slot.lock().unwrap();
    assert!(
        slot.retained
            .as_mut()
            .unwrap()
            .child
            .try_wait()
            .unwrap()
            .is_none()
    );
}

#[test]
fn admission_receipts_are_only_issued_for_this_calls_successful_claim() {
    let root = crate::native::private_tempdir().unwrap();
    let marker = root.path().join("started");
    let program = drmr_test_support::install(
        &root.path().join("response"),
        &drmr_test_support::ToolSpec::default().with("ready_file", marker.to_str().unwrap()),
    )
    .unwrap();
    let argv = [program.to_str().unwrap().into()];
    let owner = Arc::new(CleanupOwner::new());
    let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
        .with_cleanup_owner(owner.clone());
    let lease = owner.claim().unwrap();
    let mut denied = None;
    let result =
        runner.run_tool_with_cleanup_receipt(&argv, "fixture", &RunOptions::default(), &mut denied);
    assert_eq!(result.unwrap_err().kind, "cleanup-unconfirmed");
    assert!(denied.is_none());
    assert!(owner.has_pending());
    assert!(!marker.exists());
    drop(lease);
    let mut admitted = None;
    runner
        .run_tool_with_cleanup_receipt(&argv, "fixture", &RunOptions::default(), &mut admitted)
        .unwrap();
    let admitted = admitted.unwrap();
    assert!(marker.exists());
    assert!(!admitted.has_pending());
    admitted.retry_finish().unwrap();
    let unowned = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink));
    let mut receipt = None;
    unowned
        .run_tool_with_cleanup_receipt(&argv, "fixture", &RunOptions::default(), &mut receipt)
        .unwrap();
    assert!(receipt.is_none());
}

#[test]
fn denied_and_stale_receipts_cannot_reap_a_later_retained_invocation() {
    let root = crate::native::private_tempdir().unwrap();
    let owner = Arc::new(CleanupOwner::new());
    let runner = isolated_runner(&owner, Arc::new(NoSink));
    let old = owner.claim().unwrap();
    let stale = old.receipt(&owner);
    drop(old);
    let retained = retain(&runner, root.path());
    assert!(retained.has_pending());
    assert!(!stale.has_pending());
    stale.retry_finish().unwrap();
    assert!(retained.has_pending());
    alive(&owner);
    let mut denied = None;
    let refused = runner.run_tool_with_cleanup_receipt(
        &[],
        "must-not-start",
        &RunOptions::default(),
        &mut denied,
    );
    assert_eq!(refused.unwrap_err().kind, "cleanup-unconfirmed");
    assert!(denied.is_none());
    assert!(retained.has_pending());
    alive(&owner);
    retained.retry_finish().unwrap();
    assert!(!retained.has_pending());
    assert!(!owner.has_pending());
}

#[derive(Default)]
struct PanicOnFailure {
    failure: Mutex<Option<String>>,
}
impl Sink for PanicOnFailure {
    fn emit(&self, record: &Record) {
        if record.get("level").and_then(serde_json::Value::as_str) == Some("error") {
            *self.failure.lock().unwrap() = record
                .get("stderr")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned);
            panic!("synthetic output sink failure");
        }
    }
}

#[test]
fn admission_timeout_unwind_does_not_retain_an_unstarted_child() {
    let root = crate::native::private_tempdir().unwrap();
    let ready = root.path().join("ready");
    let program = drmr_test_support::install(
        &root.path().join("response"),
        &drmr_test_support::ToolSpec::default().with("ready_file", ready.to_str().unwrap()),
    )
    .unwrap();
    let owner = Arc::new(CleanupOwner::new());
    let sink = Arc::new(PanicOnFailure::default());
    let runner = isolated_runner(&owner, sink.clone());
    let held = runner.reserve("held", &RunOptions::default()).unwrap();
    let mut receipt = None;
    let cleanup_called = Cell::new(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = runner.run_tool_injected_cleanup_with_receipt(
            &[program.to_str().unwrap().into()],
            &RunOptions {
                timeout: Some(Duration::from_millis(100)),
                ..RunOptions::default()
            },
            &mut receipt,
            |_| {
                cleanup_called.set(true);
                Err(io::Error::other("must not reach cleanup before admission"))
            },
        );
    }));
    assert!(result.is_err());
    assert_eq!(
        sink.failure.lock().unwrap().as_deref(),
        Some("timed out after 0.1s")
    );
    assert!(!cleanup_called.get());
    assert!(!ready.exists());
    let receipt = receipt.unwrap();
    assert!(!receipt.has_pending());
    receipt.retry_finish().unwrap();
    assert!(!owner.has_pending());
    drop(held);
    drop(runner.reserve("recovered", &RunOptions::default()).unwrap());
}

#[test]
fn receipt_is_installed_before_failure_record_emission_can_unwind() {
    let root = crate::native::private_tempdir().unwrap();
    let owner = Arc::new(CleanupOwner::new());
    let sink = Arc::new(PanicOnFailure::default());
    let runner = isolated_runner(&owner, sink.clone());
    let ready = root.path().join("ready");
    let program = drmr_test_support::install(
        &root.path().join("response"),
        &drmr_test_support::ToolSpec {
            stdout: vec![b'\n'],
            sleep_ms: 60_000,
            ..drmr_test_support::ToolSpec::default()
        }
        .with("ready_file", ready.to_str().unwrap()),
    )
    .unwrap();
    let mut receipt = None;
    let cleanup_called = Cell::new(false);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = runner.run_tool_injected_cleanup_with_receipt(
            &[program.to_str().unwrap().into()],
            &RunOptions {
                // The fixture writes readiness before overflowing this capture bound.
                max_stdout_bytes: 0,
                ..RunOptions::default()
            },
            &mut receipt,
            |_| {
                cleanup_called.set(true);
                assert!(ready.is_file());
                Err(io::Error::other("injected cleanup uncertainty"))
            },
        );
    }));
    assert!(result.is_err());
    assert!(cleanup_called.get());
    assert_eq!(
        sink.failure.lock().unwrap().as_deref(),
        Some("process tree cleanup could not be confirmed")
    );
    let receipt = receipt.unwrap();
    assert!(receipt.has_pending());
    alive(&owner);
    receipt.retry_finish().unwrap();
    assert!(!owner.has_pending());
}
