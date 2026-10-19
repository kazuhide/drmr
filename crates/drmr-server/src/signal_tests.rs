//! A fresh process receives a native signal at the exact ready-before-first-poll boundary.
use super::*;
use rustix::process::{Pid, Signal, kill_process};
use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

struct OwnedChild(Option<Child>);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn ready_server_handles_native_signals_before_the_shutdown_future_is_polled() {
    const CHILD: &str = "DRMR_TEST_READY_SIGNAL_CHILD";
    if let Some(mode) = std::env::var_os(CHILD) {
        let signal = match mode.to_str() {
            Some("term") => Signal::TERM,
            Some("interrupt") => Signal::INT,
            _ => panic!("unknown test signal"),
        };
        let root = drmr_core::native::private_tempdir().unwrap();
        let catalog = root.path().join("catalog");
        std::fs::create_dir(&catalog).unwrap();
        let mut config = ServerConfig::new(catalog, root.path().join("output"));
        config.bind_port = 0;
        serve_with_ready(config, |_address| {
            let pid = Pid::from_raw(i32::try_from(std::process::id()).unwrap()).unwrap();
            // This synchronous callback finishes before either select branch is first polled.
            kill_process(pid, signal).unwrap();
        })
        .unwrap();
        return;
    }
    for mode in ["term", "interrupt"] {
        let child=Command::new(std::env::current_exe().unwrap())
            .args(["--exact","signal_tests::ready_server_handles_native_signals_before_the_shutdown_future_is_polled","--nocapture"])
            .env(CHILD,mode).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap();
        let mut owned = OwnedChild(Some(child));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if owned.0.as_mut().unwrap().try_wait().unwrap().is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "ready signal shutdown must finish within five seconds"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let output = owned.0.take().unwrap().wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
