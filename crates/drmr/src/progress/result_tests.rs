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
        progress.begin_result();
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
    progress.server_handoff();
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
