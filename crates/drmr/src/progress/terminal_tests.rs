//! Exercise the actual output stream with independent primary and alternate buffers.
use super::*;
use crate::progress::terminal::LiveTerminal;

const PRIOR_LOG: &str = "Existing log text";

#[derive(Debug)]
struct Buffer {
    lines: Vec<Vec<char>>,
    x: usize,
    y: usize,
    wrap: bool,
}
impl Buffer {
    fn new(width: usize, height: usize) -> Self {
        Self {
            lines: vec![vec![' '; width]; height],
            x: 0,
            y: 0,
            wrap: false,
        }
    }

    fn resize(&mut self, width: usize, height: usize) {
        for line in &mut self.lines {
            line.resize(width, ' ');
        }
        self.lines.resize(height, vec![' '; width]);
        self.x = self.x.min(width - 1);
        self.y = self.y.min(height - 1);
        self.wrap = false;
    }

    fn down(&mut self, width: usize, height: usize) {
        self.y += 1;
        if self.y >= height {
            self.lines.remove(0);
            self.lines.push(vec![' '; width]);
            self.y = height - 1;
        }
    }
}

#[derive(Debug)]
struct Cursor {
    width: usize,
    height: usize,
    primary: Buffer,
    alternate: Option<Buffer>,
    onlcr: bool,
    synchronized: bool,
    pending: String,
    labels: Vec<(&'static str, usize, usize, bool)>,
    written: usize,
    resize_at: Option<(usize, usize, usize)>,
    resize_count: usize,
}
impl Cursor {
    fn active(&mut self) -> &mut Buffer {
        self.alternate.as_mut().unwrap_or(&mut self.primary)
    }

    fn resize(&mut self, width: usize, height: usize) {
        self.width = width;
        self.height = height;
        self.primary.resize(width, height);
        if let Some(alternate) = &mut self.alternate {
            alternate.resize(width, height);
        }
        self.resize_count += 1;
    }

    fn control(&mut self, parameters: &str, command: char) {
        match (parameters, command) {
            ("?1049", 'h') => {
                assert!(self.alternate.is_none(), "alternate view entered twice");
                self.alternate = Some(Buffer::new(self.width, self.height));
            }
            ("?1049", 'l') => {
                assert!(self.alternate.take().is_some(), "unowned alternate exit");
            }
            ("?2026", 'h') => self.synchronized = true,
            ("?2026", 'l') => self.synchronized = false,
            (_, 'H') => {
                let mut coordinates = parameters
                    .split(';')
                    .map(|value| value.parse::<usize>().unwrap_or(1).saturating_sub(1));
                let y = coordinates.next().unwrap_or(0).min(self.height - 1);
                let x = coordinates.next().unwrap_or(0).min(self.width - 1);
                let active = self.active();
                active.x = x;
                active.y = y;
                active.wrap = false;
            }
            ("2", 'J') => {
                assert!(self.alternate.is_some(), "primary history was erased");
                for line in &mut self.active().lines {
                    line.fill(' ');
                }
            }
            _ => panic!("unexpected output control: {parameters}{command}"),
        }
    }

    fn write(&mut self, value: &str) {
        self.pending.push_str(value);
        while !self.pending.is_empty() {
            if self.pending.starts_with('\x1b') {
                if self.pending.len() < 2 {
                    return;
                }
                assert!(self.pending.starts_with("\x1b["));
                let Some((end, command)) = self
                    .pending
                    .char_indices()
                    .skip(2)
                    .find(|(_, character)| ('@'..='~').contains(character))
                else {
                    return;
                };
                let parameters = self.pending[2..end].to_owned();
                self.control(&parameters, command);
                self.pending.drain(..end + command.len_utf8());
                continue;
            }
            for label in ["drmr convert", "Source", "Overall"] {
                if self.pending.starts_with(label) {
                    let alternate = self.alternate.is_some();
                    let active = self.active();
                    let position = (label, active.x, active.y, alternate);
                    self.labels.push(position);
                }
            }
            let character = self.pending.chars().next().unwrap();
            self.pending.drain(..character.len_utf8());
            let (width, height, onlcr) = (self.width, self.height, self.onlcr);
            let active = self.active();
            match character {
                '\r' => {
                    active.x = 0;
                    active.wrap = false;
                }
                '\n' => {
                    active.down(width, height);
                    if onlcr {
                        active.x = 0;
                    }
                    active.wrap = false;
                }
                _ => {
                    assert!(!character.is_control());
                    if active.wrap {
                        active.x = 0;
                        active.down(width, height);
                        active.wrap = false;
                    }
                    active.lines[active.y][active.x] = character;
                    active.x += 1;
                    if active.x == width {
                        active.x -= 1;
                        active.wrap = true;
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
        let mut primary = Buffer::new(width, 24);
        for (index, character) in PRIOR_LOG.chars().take(column).enumerate() {
            primary.lines[0][index] = character;
        }
        primary.x = column;
        Self(Arc::new(Mutex::new(Cursor {
            width,
            height: 24,
            primary,
            alternate: None,
            onlcr,
            synchronized: false,
            pending: String::new(),
            labels: Vec::new(),
            written: 0,
            resize_at: None,
            resize_count: 0,
        })))
    }

    fn labels(&self) -> Vec<(&'static str, usize, usize, bool)> {
        self.0.lock().unwrap().labels.clone()
    }

    fn primary(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap()
            .primary
            .lines
            .iter()
            .map(|line| line.iter().collect::<String>().trim_end().to_owned())
            .filter(|line| !line.is_empty())
            .collect()
    }

    fn restored(&self) -> bool {
        let state = self.0.lock().unwrap();
        state.alternate.is_none() && !state.synchronized && state.pending.is_empty()
    }

    fn resize_during_write(&self, after_bytes: usize, columns: usize, rows: usize) {
        self.0.lock().unwrap().resize_at = Some((after_bytes, columns, rows));
    }
}
impl Write for Terminal {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let mut state = self.0.lock().unwrap();
        let accepted = state.resize_at.map_or(bytes.len(), |(at, _, _)| {
            bytes.len().min(at - state.written)
        });
        state.write(std::str::from_utf8(&bytes[..accepted]).unwrap());
        state.written += accepted;
        if let Some((at, columns, rows)) = state.resize_at
            && state.written == at
        {
            state.resize(columns, rows);
            state.resize_at = None;
        }
        Ok(accepted)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn progress(terminal: &Terminal, columns: u16) -> Progress {
    Progress::with_terminal(
        Mode::Text,
        "convert",
        Box::new(terminal.clone()),
        Duration::from_mins(1),
        Some(LiveTerminal::for_test(columns, 24)),
    )
}

#[test]
fn terminal_frame_starts_each_row_at_column_zero_after_a_partial_log_line() {
    for width in [40_u16, 80, 120, 300] {
        for onlcr in [false, true] {
            let terminal = Terminal::new(usize::from(width), 17, onlcr);
            let progress = progress(&terminal, width);
            progress.mark("configuration");
            let labels = terminal.labels();
            assert_ne!(labels.len(), 0);
            assert_eq!(labels[0].2, 0, "the live view starts at its own origin");
            assert!(
                labels
                    .iter()
                    .all(|(_, x, _, alternate)| *x == 0 && *alternate),
                "{width} {onlcr} {labels:?}"
            );
            progress.finish();
            drop(progress);
            let primary = terminal.primary();
            assert_eq!(primary.first().map(String::as_str), Some(PRIOR_LOG));
            assert!(
                primary
                    .iter()
                    .any(|line| line.starts_with("Finished convert"))
            );
            assert!(terminal.restored(), "{width} {onlcr} {primary:?}");
        }
    }
}

#[test]
fn terminal_rows_keep_their_origin_after_a_log_and_redraw() {
    let terminal = Terminal::new(120, 0, false);
    let progress = progress(&terminal, 120);
    progress.mark("acquisition");
    let before = terminal.labels().len();
    writeln!(terminal.clone(), "A completed step.").unwrap();
    progress.mark("verification");
    let labels = terminal.labels();
    assert!(labels.len() > before);
    assert!(
        labels[before..]
            .iter()
            .all(|(_, x, _, alternate)| *x == 0 && *alternate),
        "{labels:?}"
    );
    progress.finish();
    drop(progress);
    assert!(terminal.restored());
}

#[test]
fn resize_during_a_partial_frame_restores_history_before_every_result_outcome() {
    for onlcr in [false, true] {
        for (code, status) in [
            (None, "Finished"),
            (Some("source-changed"), "Failed"),
            (Some("cancelled"), "Cancelled"),
        ] {
            let terminal = Terminal::new(120, 17, onlcr);
            // Short-write the real packet, then change the physical geometry.
            // Production-engine reflow is tested separately; this checks ownership.
            terminal.resize_during_write(100, 40, 8);
            let progress = progress(&terminal, 120);
            progress.mark("configuration");
            assert_eq!(terminal.0.lock().unwrap().resize_count, 1);
            progress.begin_result();
            assert!(terminal.restored());
            writeln!(terminal.clone(), "Command result is visible.").unwrap();
            if let Some(code) = code {
                progress.diagnostic("convert", code);
            } else {
                progress.finish();
            }
            drop(progress);
            let primary = terminal.primary();
            assert_eq!(primary.first().map(String::as_str), Some(PRIOR_LOG));
            assert_eq!(
                primary
                    .iter()
                    .filter(|line| *line == "Command result is visible.")
                    .count(),
                1
            );
            assert!(
                primary
                    .iter()
                    .any(|line| line.starts_with(&format!("{status} convert"))),
                "{primary:?}"
            );
            assert!(
                primary.iter().all(|line| !line.starts_with("drmr ")
                    && !line.starts_with("Source")
                    && !line.starts_with("Overall")),
                "{primary:?}"
            );
            assert!(terminal.restored());
        }
    }
}
