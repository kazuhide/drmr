use super::*;
use crate::progress::terminal::LiveTerminal;

#[derive(Clone, Debug, Default)]
pub(super) struct Terminal(Arc<Mutex<Vec<String>>>);
impl Terminal {
    pub(super) fn snapshot(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }

    pub(super) fn text(&self) -> String {
        self.snapshot().concat()
    }
}
impl Write for Terminal {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap()
            .push(String::from_utf8(bytes.to_vec()).unwrap());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn result_handoff_drains_drawing_without_success_and_never_erases_the_result() {
    for failed in [false, true] {
        let terminal = Terminal::default();
        let live = LiveTerminal::for_test(120, 24);
        let progress = Progress::with_terminal(
            Mode::Text,
            "convert",
            Box::new(terminal.clone()),
            Duration::from_mins(1),
            Some(live),
        );
        progress.mark("publication");
        assert!(terminal.text().contains("drmr convert"));
        progress.begin_result().unwrap();
        assert!(!progress.inner.state.lock().unwrap().finished);
        let drained = terminal.snapshot().len();
        // A late observation or heartbeat cannot resume live drawing.
        progress.mark("verification");
        progress.source_retained();
        {
            let mut state = progress.inner.state.lock().unwrap();
            progress.inner.line(&mut state, "heartbeat", None, true);
        }
        assert_eq!(
            terminal.snapshot().len(),
            drained,
            "{:?}",
            &terminal.snapshot()[drained..]
        );
        writeln!(terminal.clone(), "Command result is visible.").unwrap();
        let result_end = terminal.snapshot().len();
        if failed {
            progress.diagnostic("convert", "result-output-unavailable");
        } else {
            progress.finish();
        }
        drop(progress);
        let steps = terminal.snapshot();
        assert!(
            steps[result_end..]
                .iter()
                .all(|step| !step.contains('\x1b')),
            "{steps:?}"
        );
        let written = steps.concat();
        assert_eq!(written.matches("Command result is visible.").count(), 1);
        if failed {
            assert!(!written.contains("Finished"), "{written}");
            assert!(written.contains("Failed convert"), "{written}");
        } else {
            assert_eq!(written.matches("Finished convert").count(), 1);
            assert!(written.find("Command result").unwrap() < written.find("Finished").unwrap());
        }
    }
}

#[test]
fn dashboard_retained_source_notice_does_not_append_a_second_log_stream() {
    let terminal = Terminal::default();
    let live = LiveTerminal::for_test(120, 24);
    let progress = Progress::with_terminal(
        Mode::Text,
        "convert",
        Box::new(terminal.clone()),
        Duration::from_mins(1),
        Some(live),
    );
    progress.mark("acquisition");
    progress.source_retained();
    let text = terminal.text();
    assert!(
        !text.contains("Downloaded source retained for offline use."),
        "{text}"
    );
}

#[test]
fn server_handoff_keeps_address_and_access_logs_without_redrawing() {
    let terminal = Terminal::default();
    let live = LiveTerminal::for_test(120, 24);
    let progress = Progress::with_terminal(
        Mode::Text,
        "serve",
        Box::new(terminal.clone()),
        Duration::from_mins(1),
        Some(live),
    );
    progress.mark("server-startup");
    progress.server_handoff().unwrap();
    progress.mark("serving");
    progress.server_address("127.0.0.1:3030".parse().unwrap());
    writeln!(terminal.clone(), "Access log remains readable.").unwrap();
    let log_end = terminal.snapshot().len();
    {
        let mut state = progress.inner.state.lock().unwrap();
        progress.inner.line(&mut state, "heartbeat", None, true);
    }
    drop(progress);
    assert_eq!(terminal.snapshot().len(), log_end);
    let text = terminal.text();
    assert_eq!(
        text.matches(" * Running on http://127.0.0.1:3030").count(),
        1,
        "{text}"
    );
    assert!(text.contains("Access log remains readable."));
}

#[derive(Clone, Default)]
struct Interrupted {
    terminal: Terminal,
    remaining: Arc<Mutex<Option<usize>>>,
    fail_flush: Arc<std::sync::atomic::AtomicBool>,
}

impl Interrupted {
    fn progress(&self, command: &'static str) -> Progress {
        Progress::with_terminal(
            Mode::Text,
            command,
            Box::new(self.clone()),
            Duration::from_mins(1),
            Some(LiveTerminal::for_test(120, 24)),
        )
    }
}

impl Write for Interrupted {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut remaining = self.remaining.lock().unwrap();
        let amount = remaining.map_or(bytes.len(), |left| left.min(bytes.len()));
        if amount == 0 && !bytes.is_empty() {
            return Err(std::io::Error::other("fixture terminal unavailable"));
        }
        let accepted = self.terminal.write(&bytes[..amount])?;
        if let Some(left) = &mut *remaining {
            *left -= accepted;
        }
        Ok(accepted)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.fail_flush.load(std::sync::atomic::Ordering::Acquire) {
            Err(std::io::Error::other("fixture terminal flush unavailable"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn failed_inline_cleanup_blocks_results_and_late_paints_until_recovery() {
    for flush in [false, true] {
        let mut stream = Interrupted::default();
        let progress = stream.progress("convert");
        progress.mark("configuration");
        if flush {
            stream
                .fail_flush
                .store(true, std::sync::atomic::Ordering::Release);
        } else {
            *stream.remaining.lock().unwrap() = Some(2);
        }
        let mut called = false;
        let failed = progress.write_result(|| {
            called = true;
            writeln!(stream, "Result must not be written before inline cleanup")
        });
        assert!(failed.is_err());
        assert!(!called, "a failed handoff must never invoke its writer");
        assert!(!progress.inner.state.lock().unwrap().finished);
        let stopped = stream.terminal.snapshot();
        progress.mark("verification");
        progress.source_retained();
        {
            let mut state = progress.inner.state.lock().unwrap();
            progress.inner.line(&mut state, "heartbeat", None, true);
        }
        assert_eq!(stream.terminal.snapshot(), stopped);
        *stream.remaining.lock().unwrap() = None;
        stream
            .fail_flush
            .store(false, std::sync::atomic::Ordering::Release);
        progress
            .write_result(|| {
                called = true;
                write!(stream, "Recovered result is visible.\r\n")
            })
            .unwrap();
        assert!(called);
        let restored = stream.terminal.text();
        let result_start = restored.find("Recovered result").unwrap();
        assert!(restored.find("\x1b[?2026l\r\n").unwrap() < result_start);
        assert_eq!(restored.matches("\x1b[?2026l\r\n").count(), 1);
        progress.finish();
        drop(progress);
        assert!(!stream.terminal.text()[result_start..].contains('\x1b'));
    }
}

#[test]
fn failed_server_handoff_preserves_the_error_and_suppresses_late_paints() {
    let stream = Interrupted::default();
    let progress = stream.progress("serve");
    progress.mark("server-startup");
    *stream.remaining.lock().unwrap() = Some(2);
    assert!(progress.server_handoff().is_err());
    let stopped = stream.terminal.snapshot();
    progress.mark("serving");
    assert_eq!(stream.terminal.snapshot(), stopped);
    assert!(!stream.terminal.text().contains("Running on"));
    *stream.remaining.lock().unwrap() = None;
    progress.server_handoff().unwrap();
    progress.server_address("127.0.0.1:3030".parse().unwrap());
    let restored = stream.terminal.text();
    assert!(restored.find("\x1b[?2026l\r\n").unwrap() < restored.find("Running on").unwrap());
    let end = stream.terminal.snapshot().len();
    drop(progress);
    assert_eq!(stream.terminal.snapshot().len(), end);
}
