//! Explicit ownership for one serial caller's unconfirmed child cleanup.

use crate::native::ManagedChild;
use std::io;
use std::sync::{Mutex, PoisonError};

#[derive(Default)]
struct Slot {
    active: bool,
    retained: Option<Retained>,
}

struct Retained {
    child: ManagedChild,
    permit: Option<crate::resources::Permit>,
}

/// Retain at most one active or unconfirmed tool tree for an explicit caller.
///
/// Keep this owner alive after a failed call. New tools are refused until an
/// explicit `retry_finish` confirms cleanup. Normal runners do not use this slot.
#[derive(Default)]
pub struct CleanupOwner {
    slot: Mutex<Slot>,
}

impl std::fmt::Debug for CleanupOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CleanupOwner").finish_non_exhaustive()
    }
}

impl CleanupOwner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a call is active or a tree still requires cleanup confirmation.
    pub fn has_pending(&self) -> bool {
        {
            let slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
            slot.active || slot.retained.is_some()
        }
    }

    /// Retry the retained tree once. Only confirmed cleanup releases the slot.
    ///
    /// Native cleanup keeps its existing deadline; this is not a whole-I/O hard
    /// wall. An active call must finish its reader scopes before retry is allowed.
    pub fn retry_finish(&self) -> io::Result<()> {
        self.retry_with(ManagedChild::finish)
    }

    fn retry_with(
        &self,
        cleanup: impl FnOnce(&mut ManagedChild) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.active {
            return Err(io::Error::other("tool-cleanup-still-active"));
        }
        if let Some(retained) = &mut slot.retained {
            cleanup(&mut retained.child)?;
            slot.retained = None;
        }
        Ok(())
    }

    pub(super) fn claim(&self) -> io::Result<Lease<'_>> {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.active || slot.retained.is_some() {
            return Err(io::Error::other("tool-cleanup-owner-unavailable"));
        }
        slot.active = true;
        Ok(Lease { owner: self })
    }
}

pub(super) struct Lease<'a> {
    owner: &'a CleanupOwner,
}

impl Lease<'_> {
    pub(super) fn retain(&self, child: ManagedChild) {
        self.owner
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retained = Some(Retained {
            child,
            permit: None,
        });
    }
    pub(super) fn retain_permit(&self, permit: crate::resources::Permit) {
        if let Some(retained) = &mut self
            .owner
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retained
        {
            retained.permit = Some(permit);
        }
    }
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let mut slot = self
            .owner
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        slot.active = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::NoSink;
    use crate::toolrun::{RunOptions, Secrets, ToolRunner};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn fixture_options() -> RunOptions<'static> {
        RunOptions {
            timeout: Some(Duration::from_secs(3)),
            ..RunOptions::default()
        }
    }

    #[test]
    fn reservation_is_exclusive_and_active_retry_does_not_release_it() {
        let owner = CleanupOwner::new();
        let lease = owner.claim().unwrap();
        assert!(owner.has_pending());
        assert!(owner.claim().is_err());
        assert!(owner.retry_finish().is_err());
        assert!(owner.has_pending());
        drop(lease);
        assert!(!owner.has_pending());
        owner.retry_finish().unwrap();
    }

    #[test]
    fn spawn_failure_and_confirmed_success_release_the_opt_in_slot() {
        let root = tempfile::tempdir().unwrap();
        let owner = Arc::new(CleanupOwner::new());
        let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
            .with_cleanup_owner(owner.clone());
        let missing = root.path().join("absent");
        let error = runner
            .run_tool(
                &[missing.to_str().unwrap().to_owned()],
                "fixture",
                &fixture_options(),
            )
            .unwrap_err();
        assert_eq!(error.kind, "unavailable");
        assert!(!owner.has_pending());
        let program = drmr_test_support::install(
            &root.path().join("response"),
            &drmr_test_support::ToolSpec::default(),
        )
        .unwrap();
        assert_eq!(
            runner
                .run_tool(
                    &[program.to_str().unwrap().to_owned()],
                    "fixture",
                    &fixture_options()
                )
                .unwrap()
                .returncode,
            Some(0)
        );
        assert!(!owner.has_pending());
    }

    fn assert_native_owner_alive(owner: &CleanupOwner) {
        let mut slot = owner.slot.lock().unwrap();
        let child = &mut slot
            .retained
            .as_mut()
            .expect("native owner was not transferred")
            .child;
        assert!(
            child.try_wait().unwrap().is_none(),
            "fixture must still be alive when readers join"
        );
    }

    fn resource_probe(
        limiter: &Arc<crate::resources::Limiter>,
        timeout: Duration,
    ) -> Result<crate::resources::Permit, crate::resources::ReservationError> {
        limiter.acquire(
            crate::resources::Claim {
                cpu_threads: 1,
                memory_bytes: 1,
            },
            None,
            Some(timeout),
        )
    }

    #[test]
    fn uncertain_live_child_is_retained_across_observed_calls_and_explicit_retries() {
        let root = tempfile::tempdir().unwrap();
        let ready = root.path().join("held-ready");
        let lock = root.path().join("held-lock");
        let program = drmr_test_support::install(
            &root.path().join("response"),
            &drmr_test_support::ToolSpec::default(),
        )
        .unwrap();
        let argv = vec![
            program.to_str().unwrap().to_owned(),
            "--fixture-lock".into(),
            lock.to_str().unwrap().into(),
            ready.to_str().unwrap().into(),
        ];
        let owner = Arc::new(CleanupOwner::new());
        let limiter = Arc::new(crate::resources::Limiter::new(1, 1024 * 1024 * 1024));
        let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
            .with_cleanup_owner(owner.clone())
            .with_limiter(Arc::clone(&limiter));
        let mut attempts = 0;
        let mut ready_observed = false;
        let result = runner.run_tool_injected_cleanup(
            &argv,
            &RunOptions {
                timeout: Some(Duration::from_millis(100)),
                ..RunOptions::default()
            },
            |_| {
                attempts += 1;
                let deadline = Instant::now() + Duration::from_secs(10);
                while !ready.is_file() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                ready_observed = ready.is_file();
                Err(io::Error::other("injected cleanup uncertainty"))
            },
        );
        assert_eq!(result.unwrap_err().kind, "cleanup-unconfirmed");
        assert_eq!(attempts, 1);
        assert!(ready_observed, "held fixture did not start");
        assert!(owner.has_pending());
        assert_native_owner_alive(&owner);
        assert_eq!(
            resource_probe(&limiter, Duration::from_millis(20)).unwrap_err(),
            crate::resources::ReservationError::Timeout
        );
        let observed = runner.observed(Arc::new(NoSink));
        let marker = root.path().join("must-not-start");
        let other = drmr_test_support::install(
            &root.path().join("other"),
            &drmr_test_support::ToolSpec::default().with("ready_file", marker.to_str().unwrap()),
        )
        .unwrap();
        let denied = observed
            .run_tool(
                &[other.to_str().unwrap().into()],
                "fixture",
                &RunOptions {
                    timeout: Some(Duration::from_millis(100)),
                    ..RunOptions::default()
                },
            )
            .unwrap_err();
        assert_eq!(denied.kind, "cleanup-unconfirmed");
        assert!(!marker.exists());
        assert!(
            owner
                .retry_with(|_| Err(io::Error::other("still unconfirmed")))
                .is_err()
        );
        assert!(owner.has_pending());
        owner.retry_finish().unwrap();
        assert!(!owner.has_pending());
        drop(resource_probe(&limiter, Duration::from_secs(1)).unwrap());
        let released = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock)
            .unwrap();
        released.try_lock().unwrap();
        owner.retry_finish().unwrap();
        assert_eq!(
            observed
                .run_tool(
                    &[other.to_str().unwrap().into()],
                    "fixture",
                    &fixture_options()
                )
                .unwrap()
                .returncode,
            Some(0)
        );
        assert!(marker.is_file());
    }
}
