//! Exercise the real indicatif draw path with cursor and delayed-wrap semantics.
use super::*;

#[derive(Debug)]
struct Cursor {
    width: usize,
    x: usize,
    y: usize,
    wrap: bool,
    onlcr: bool,
    labels: Vec<(&'static str, usize, usize)>,
}
impl Cursor {
    fn write(&mut self, value: &str) {
        for (offset, ch) in value.char_indices() {
            match ch {
                '\r' => {
                    self.x = 0;
                    self.wrap = false;
                }
                '\n' => {
                    self.y += 1;
                    if self.onlcr {
                        self.x = 0;
                    }
                    self.wrap = false;
                }
                _ => {
                    if self.wrap {
                        self.x = 0;
                        self.y += 1;
                        self.wrap = false;
                    }
                    for label in ["drmr convert", "Source", "Overall"] {
                        if value[offset..].starts_with(label) {
                            self.labels.push((label, self.x, self.y));
                        }
                    }
                    self.x += 1;
                    if self.x == self.width {
                        self.x -= 1;
                        self.wrap = true;
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
struct Terminal(Arc<Mutex<Cursor>>);
impl Terminal {
    fn new(width: usize, column: usize, onlcr: bool) -> Self {
        Self(Arc::new(Mutex::new(Cursor {
            width,
            x: column,
            y: 0,
            wrap: false,
            onlcr,
            labels: Vec::new(),
        })))
    }

    fn labels(&self) -> Vec<(&'static str, usize, usize)> {
        self.0.lock().unwrap().labels.clone()
    }
}
impl indicatif::TermLike for Terminal {
    fn width(&self) -> u16 {
        u16::try_from(self.0.lock().unwrap().width).unwrap()
    }
    fn height(&self) -> u16 {
        24
    }
    fn move_cursor_up(&self, count: usize) -> std::io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.y = state.y.saturating_sub(count);
        state.wrap = false;
        Ok(())
    }
    fn move_cursor_down(&self, count: usize) -> std::io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.y += count;
        state.wrap = false;
        Ok(())
    }
    fn move_cursor_right(&self, count: usize) -> std::io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.x = state.x.saturating_add(count).min(state.width - 1);
        state.wrap = false;
        Ok(())
    }
    fn move_cursor_left(&self, count: usize) -> std::io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.x = state.x.saturating_sub(count);
        state.wrap = false;
        Ok(())
    }
    fn write_line(&self, value: &str) -> std::io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.write(value);
        state.write("\n");
        Ok(())
    }
    fn write_str(&self, value: &str) -> std::io::Result<()> {
        self.0.lock().unwrap().write(value);
        Ok(())
    }
    fn clear_line(&self) -> std::io::Result<()> {
        let mut state = self.0.lock().unwrap();
        state.x = 0;
        state.wrap = false;
        Ok(())
    }
    fn flush(&self) -> std::io::Result<()> {
        Ok(())
    }
}
impl Write for Terminal {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap()
            .write(std::str::from_utf8(bytes).unwrap());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn progress(terminal: &Terminal) -> (Progress, ProgressBar) {
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
    (progress, bar)
}

#[test]
fn terminal_frame_starts_each_row_at_column_zero_after_a_partial_log_line() {
    for width in [40, 80, 120, 300] {
        for onlcr in [false, true] {
            let terminal = Terminal::new(width, 17, onlcr);
            let (progress, bar) = progress(&terminal);
            progress.mark("configuration");
            bar.force_draw();
            let labels = terminal.labels();
            progress.finish();
            assert!(!labels.is_empty());
            assert!(labels[0].2 > 0, "the preceding log line must remain intact");
            assert!(
                labels.iter().all(|(_, x, _)| *x == 0),
                "{width} {onlcr} {labels:?}"
            );
        }
    }
}

#[test]
fn terminal_rows_keep_their_origin_after_an_indicatif_log_and_redraw() {
    let terminal = Terminal::new(120, 0, false);
    let (progress, bar) = progress(&terminal);
    progress.mark("acquisition");
    bar.force_draw();
    let before = terminal.labels().len();
    bar.println("A completed step.");
    progress.mark("verification");
    bar.force_draw();
    let labels = terminal.labels();
    progress.finish();
    assert!(labels.len() > before);
    assert!(
        labels[before..].iter().all(|(_, x, _)| *x == 0),
        "{labels:?}"
    );
}
