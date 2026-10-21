//! Private output and complete coverage checks for isolated unit-test children.
use crate::native::{self, ManagedChild, PrivateTempFile};
use std::{
    io::{self, Read as _, Seek as _},
    process::{Command, ExitStatus, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};

const OUTPUT_LIMIT: usize = 64 * 1024;
const DIAGNOSTIC_LIMIT: usize = 4096;
static SUMMARY: OnceLock<regex::Regex> = OnceLock::new();

struct Captured {
    status: Option<ExitStatus>,
    stopped: Option<&'static str>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

pub(crate) fn run(mut command: Command, name: &str, timeout: Duration) {
    command.arg("--color=never");
    let captured = capture(command, timeout).expect("isolated test capture or cleanup failed");
    let validation = captured.validate(name);
    assert!(
        validation.is_ok(),
        "isolated test {name} did not establish complete coverage: {:?}; stdout: {}; stderr: {}",
        validation,
        diagnostic(&captured.stdout),
        diagnostic(&captured.stderr),
    );
}

fn capture(command: Command, timeout: Duration) -> io::Result<Captured> {
    capture_with_cleanup(command, timeout, ManagedChild::finish)
}

fn capture_with_cleanup(
    mut command: Command,
    timeout: Duration,
    mut cleanup: impl FnMut(&mut ManagedChild) -> io::Result<()>,
) -> io::Result<Captured> {
    // Files deliberately avoid the pipe readers exercised by these children.
    let mut stdout = native::private_tempfile()?;
    let mut stderr = native::private_tempfile()?;
    command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    let mut child = ManagedChild::spawn(command, true)?;
    let began = Instant::now();
    let observed = (|| -> io::Result<_> {
        loop {
            if stdout.metadata()?.len() > OUTPUT_LIMIT as u64
                || stderr.metadata()?.len() > OUTPUT_LIMIT as u64
            {
                return Ok((None, Some("output-limit")));
            }
            if let Some(status) = child.try_wait()? {
                return Ok((Some(status), None));
            }
            if began.elapsed() >= timeout {
                return Ok((None, Some("watchdog")));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    })();
    // A failed observation or watchdog must still explicitly finish its owner.
    cleanup(&mut child)?;
    let (status, stopped) = observed?;
    Ok(Captured {
        status,
        stopped,
        stdout: read_limited(&mut stdout)?,
        stderr: read_limited(&mut stderr)?,
    })
}

fn read_limited(file: &mut PrivateTempFile) -> io::Result<Vec<u8>> {
    file.rewind()?;
    let mut bytes = Vec::new();
    (&mut **file)
        .take(OUTPUT_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn diagnostic(bytes: &[u8]) -> String {
    let end = bytes.len().min(DIAGNOSTIC_LIMIT);
    let mut text = String::from_utf8_lossy(&bytes[..end]).into_owned();
    if end < bytes.len() {
        text.push_str(" [diagnostic truncated]");
    }
    text
}

impl Captured {
    fn validate(&self, name: &str) -> Result<(), &'static str> {
        if let Some(reason) = self.stopped {
            return Err(reason);
        }
        if !self.status.is_some_and(|status| status.success()) {
            return Err("non-success-status");
        }
        if self.stdout.len() > OUTPUT_LIMIT || self.stderr.len() > OUTPUT_LIMIT {
            return Err("output-limit");
        }
        let stdout = std::str::from_utf8(&self.stdout).map_err(|_| "invalid-output-encoding")?;
        let stderr = std::str::from_utf8(&self.stderr).map_err(|_| "invalid-output-encoding")?;
        if [stdout, stderr]
            .iter()
            .any(|text| text.contains("skipped:") || text.contains(" ... ignored"))
        {
            return Err("unexpected-skip");
        }
        let summary = SUMMARY.get_or_init(|| {
            regex::Regex::new(r"^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out(?:; finished in [0-9]+(?:\.[0-9]+)?s)?$")
                .expect("fixed isolated-test summary expression")
        });
        if stdout.matches("test result:").count() != 1
            || stderr.contains("test result:")
            || stdout.lines().filter(|line| summary.is_match(line)).count() != 1
        {
            return Err("incomplete-test-summary");
        }
        let passed = format!("test {name} ... ok");
        if stdout.lines().filter(|line| *line == passed).count() != 1 {
            return Err("requested-test-not-confirmed");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const NAME: &str = "isolated::requested";

    fn status(code: u8) -> ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            ExitStatus::from_raw(i32::from(code) << 8)
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::ExitStatusExt as _;
            ExitStatus::from_raw(u32::from(code))
        }
    }

    fn successful() -> Captured {
        Captured {
            status: Some(status(0)),
            stopped: None,
            stdout: format!("\nrunning 1 test\ntest {NAME} ... ok\n\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 12 filtered out; finished in 0.01s\n").into_bytes(),
            stderr: Vec::new(),
        }
    }

    #[test]
    fn exact_child_coverage_requires_the_requested_name_and_one_complete_summary() {
        assert!(successful().validate(NAME).is_ok());
        assert!(successful().validate("other::test").is_err());
        for (from, to) in [
            ("1 passed", "0 passed"),
            ("1 passed", "2 passed"),
            ("0 failed", "1 failed"),
            ("0 ignored", "1 ignored"),
            ("0 measured", "1 measured"),
            ("filtered out", "filtered"),
        ] {
            let mut captured = successful();
            captured.stdout = String::from_utf8(captured.stdout)
                .unwrap()
                .replace(from, to)
                .into_bytes();
            assert!(captured.validate(NAME).is_err(), "{from} -> {to}");
        }
    }

    #[test]
    fn duplicate_partial_missing_and_skipped_child_evidence_is_rejected() {
        for extra in [
            "test result: ok. truncated\n",
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n",
            "test isolated::requested ... ok\n",
            "test other ... ignored, hidden child\n",
            "skipped: unperformed child oracle\n",
        ] {
            let mut captured = successful();
            captured.stdout.extend_from_slice(extra.as_bytes());
            assert!(captured.validate(NAME).is_err());
        }
        let mut captured = successful();
        captured.stdout.clear();
        assert!(captured.validate(NAME).is_err());
        let mut captured = successful();
        captured.stderr = b"skipped: unperformed child oracle".to_vec();
        assert!(captured.validate(NAME).is_err());
        let mut captured = successful();
        captured.stderr = b"test result: ok. truncated".to_vec();
        assert!(captured.validate(NAME).is_err());
    }

    #[test]
    fn status_encoding_and_size_are_not_replaced_by_success_looking_text() {
        let mut captured = successful();
        captured.status = Some(status(7));
        assert!(captured.validate(NAME).is_err());
        for field in [true, false] {
            let mut captured = successful();
            let bytes = if field {
                &mut captured.stdout
            } else {
                &mut captured.stderr
            };
            bytes.push(0xff);
            assert!(captured.validate(NAME).is_err());
        }
        let mut captured = successful();
        captured.stderr = vec![b'x'; OUTPUT_LIMIT + 1];
        assert!(captured.validate(NAME).is_err());
        assert!(diagnostic(&captured.stderr).len() < DIAGNOSTIC_LIMIT + 64);
    }

    fn fixture(root: &std::path::Path, spec: &drmr_test_support::ToolSpec) -> Command {
        Command::new(drmr_test_support::install(&root.join("response"), spec).unwrap())
    }

    #[test]
    fn failed_child_and_watchdog_never_publish_success() {
        let root = native::private_tempdir().unwrap();
        let spec = drmr_test_support::ToolSpec {
            stdout: successful().stdout,
            exit_code: 7,
            ..Default::default()
        };
        let captured = capture(fixture(root.path(), &spec), Duration::from_secs(10)).unwrap();
        assert!(captured.validate(NAME).is_err());
        let root = native::private_tempdir().unwrap();
        let spec = drmr_test_support::ToolSpec {
            sleep_ms: 60_000,
            ..Default::default()
        };
        let captured = capture(fixture(root.path(), &spec), Duration::from_millis(100)).unwrap();
        assert_eq!(captured.stopped, Some("watchdog"));
        assert!(captured.validate(NAME).is_err());
    }

    #[test]
    fn output_limit_and_cleanup_failure_cannot_become_coverage() {
        let root = native::private_tempdir().unwrap();
        let spec = drmr_test_support::ToolSpec::default()
            .with("stdout_repeat", (OUTPUT_LIMIT + 1024).to_string());
        let captured = capture(fixture(root.path(), &spec), Duration::from_secs(10)).unwrap();
        assert!(captured.stdout.len() <= OUTPUT_LIMIT + 1);
        assert!(captured.validate(NAME).is_err());
        let root = native::private_tempdir().unwrap();
        let result = capture_with_cleanup(
            fixture(root.path(), &drmr_test_support::ToolSpec::default()),
            Duration::from_secs(10),
            |child| {
                child.finish()?;
                Err(io::Error::other("cleanup failure control"))
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn direct_success_still_finishes_its_live_descendant_before_returning() {
        let root = native::private_tempdir().unwrap();
        let lock = root.path().join("descendant.lock");
        let spec = drmr_test_support::ToolSpec {
            stdout: successful().stdout,
            ..Default::default()
        }
        .with(
            "descendant_pid_file",
            root.path().join("descendant.pid").to_str().unwrap(),
        )
        .with("descendant_lock_file", lock.to_str().unwrap());
        let captured = capture(fixture(root.path(), &spec), Duration::from_secs(10)).unwrap();
        assert!(captured.validate(NAME).is_ok());
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock)
            .unwrap();
        file.try_lock().unwrap();
    }

    #[test]
    fn isolated_children_do_not_publish_nested_harness_summaries() {
        let filters = [
            ("native::pipe::tests::", if cfg!(unix) { 3 } else { 2 }),
            (
                "toolrun::capture::lifecycle_tests::actual_managed_stdio_readers_join_when_cleanup_cannot_close_live_writers",
                1,
            ),
            #[cfg(unix)]
            (
                "native::broker::wire::tests::excess_rights_are_rejected_and_every_received_descriptor_is_closed",
                1,
            ),
            #[cfg(unix)]
            (
                "toolrun::capture::lifecycle_tests::readers_join_while_parent_owned_writers_remain_open",
                1,
            ),
        ];
        for (filter, expected) in filters {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command.args([filter, "--nocapture", "--test-threads=4", "--color=never"]);
            let captured = capture(command, Duration::from_secs(30)).unwrap();
            assert!(captured.status.is_some_and(|status| status.success()));
            assert!(captured.stopped.is_none());
            let text = std::str::from_utf8(&captured.stdout).unwrap();
            assert_eq!(
                text.matches("test result:").count(),
                1,
                "isolated children wrote nested summaries into the parent log for {filter}"
            );
            let summary =
                format!("test result: ok. {expected} passed; 0 failed; 0 ignored; 0 measured; ");
            assert!(
                text.lines().any(|line| line.starts_with(&summary)),
                "filtered parent did not execute the expected cases for {filter}"
            );
        }
    }
}
