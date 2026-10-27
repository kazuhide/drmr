use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Step {
    Write(String),
    Clear,
    Move(usize),
}

#[derive(Clone, Debug, Default)]
struct Terminal(Arc<Mutex<Vec<Step>>>);
impl Terminal {
    fn note(&self, step: Step) {
        self.0.lock().unwrap().push(step);
    }

    fn snapshot(&self) -> Vec<Step> {
        self.0.lock().unwrap().clone()
    }
}
impl indicatif::TermLike for Terminal {
    fn width(&self) -> u16 {
        120
    }
    fn move_cursor_up(&self, n: usize) -> std::io::Result<()> {
        self.note(Step::Move(n));
        Ok(())
    }
    fn move_cursor_down(&self, n: usize) -> std::io::Result<()> {
        self.note(Step::Move(n));
        Ok(())
    }
    fn move_cursor_right(&self, n: usize) -> std::io::Result<()> {
        self.note(Step::Move(n));
        Ok(())
    }
    fn move_cursor_left(&self, n: usize) -> std::io::Result<()> {
        self.note(Step::Move(n));
        Ok(())
    }
    fn write_line(&self, value: &str) -> std::io::Result<()> {
        self.note(Step::Write(format!("{value}\n")));
        Ok(())
    }
    fn write_str(&self, value: &str) -> std::io::Result<()> {
        self.note(Step::Write(value.to_owned()));
        Ok(())
    }
    fn clear_line(&self) -> std::io::Result<()> {
        self.note(Step::Clear);
        Ok(())
    }
    fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Write for Terminal {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.note(Step::Write(String::from_utf8_lossy(bytes).into_owned()));
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
        let bar = ProgressBar::with_draw_target(
            None,
            ProgressDrawTarget::term_like(Box::new(terminal.clone())),
        );
        bar.set_style(ProgressStyle::with_template("{msg}").unwrap());
        let progress = Progress::with_terminal(
            Mode::Text,
            "convert",
            Box::new(terminal.clone()),
            Duration::from_mins(1),
            Some(bar.clone()),
        );
        progress.mark("publication");
        bar.force_draw();
        progress.begin_result();
        assert!(!progress.inner.state.lock().unwrap().finished);
        let drained = terminal.snapshot().len();
        // A late observation, heartbeat, or ticker cannot resume live drawing.
        progress.mark("verification");
        progress.source_retained();
        {
            let mut state = progress.inner.state.lock().unwrap();
            progress.inner.line(&mut state, "heartbeat", None, true);
        }
        bar.tick();
        bar.force_draw();
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
        let steps = terminal.snapshot();
        assert!(
            steps[result_end..]
                .iter()
                .all(|step| !matches!(step, Step::Clear | Step::Move(1..))),
            "{steps:?}"
        );
        let written: String = steps
            .into_iter()
            .filter_map(|step| match step {
                Step::Write(value) => Some(value),
                Step::Clear | Step::Move(_) => None,
            })
            .collect();
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
