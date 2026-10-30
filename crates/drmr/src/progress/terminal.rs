//! An inline dashboard that redraws only its own rows below existing output.

use std::io::{self, IsTerminal as _, Write};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, Once, PoisonError};

const ENTER: &str = "\r\n";
const PAINT: &str = "\x1b[?2026h";
const END_PAINT: &str = "\x1b[?2026l";
const LEAVE: &str = "\x1b[?2026l\r\n";

static SCREEN_ACTIVE: AtomicBool = AtomicBool::new(false);
static PANICKED: AtomicBool = AtomicBool::new(false);
static RECOVERY_ROWS: AtomicUsize = AtomicUsize::new(0);

struct Session {
    entered: bool,
    closed: bool,
    frame: Option<((u16, u16), usize)>,
    recovery_rows: usize,
    #[cfg(windows)]
    mode: Option<OutputMode>,
    #[cfg(test)]
    message: String,
}

pub(super) struct LiveTerminal {
    session: Mutex<Session>,
    real: bool,
    dimensions: Option<Mutex<(u16, u16)>>,
}

impl LiveTerminal {
    pub(super) fn for_stderr() -> Option<Self> {
        if !std::io::stderr().is_terminal() || PANICKED.load(Ordering::Acquire) {
            return None;
        }
        let term = std::env::var("TERM").ok();
        if term.as_deref() == Some("dumb") {
            return None;
        }
        #[cfg(not(windows))]
        if !supports_inline_updates(term.as_deref()) {
            return None;
        }
        #[cfg(windows)]
        let mode = OutputMode::enable(term.as_deref())?;
        install_panic_restore();
        Some(Self {
            session: Mutex::new(Session {
                entered: false,
                closed: false,
                frame: None,
                recovery_rows: 0,
                #[cfg(windows)]
                mode: Some(mode),
                #[cfg(test)]
                message: String::new(),
            }),
            real: true,
            dimensions: None,
        })
    }

    #[cfg(test)]
    pub(super) fn for_test(columns: u16, rows: u16) -> Self {
        Self {
            session: Mutex::new(Session {
                entered: false,
                closed: false,
                frame: None,
                recovery_rows: 0,
                #[cfg(windows)]
                mode: None,
                message: String::new(),
            }),
            real: false,
            dimensions: Some(Mutex::new((rows, columns))),
        }
    }

    pub(super) fn size(&self) -> (u16, u16) {
        self.dimensions.as_ref().map_or_else(
            || console::Term::stderr().size(),
            |size| *size.lock().unwrap_or_else(PoisonError::into_inner),
        )
    }

    #[cfg(test)]
    pub(super) fn resize_for_test(&self, columns: u16, rows: u16) {
        *self.dimensions.as_ref().unwrap().lock().unwrap() = (rows, columns);
    }

    #[cfg(test)]
    pub(super) fn message(&self) -> String {
        self.session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .message
            .clone()
    }

    #[cfg(test)]
    pub(super) fn draw(&self, out: &mut dyn Write, message: &str) -> io::Result<()> {
        self.draw_at_size(out, message, self.size())
    }

    pub(super) fn draw_at_size(
        &self,
        out: &mut dyn Write,
        message: &str,
        size: (u16, u16),
    ) -> io::Result<()> {
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if session.closed || self.size() != size {
            // The caller laid out this frame at a different width. Wait for the
            // next observation instead of writing a frame that can wrap.
            return Ok(());
        }
        let previous = session.frame.take();
        let recovery = session.recovery_rows;
        session.recovery_rows = recovery
            .max(previous.map_or(0, |(_, rows)| rows))
            .max(message.split('\n').count());
        let mut frame = String::with_capacity(message.len() + 128);
        frame.push_str(PAINT);
        if recovery > 0 {
            frame.push_str(&ENTER.repeat(recovery + 1));
        } else if !session.entered || !clear_frame(&mut frame, previous, size) {
            // After a resize, the terminal may have reflowed the old rows.
            // Keep that snapshot in history rather than erasing guessed rows.
            frame.push_str(ENTER);
        }
        for (index, line) in message.split('\n').enumerate() {
            if index != 0 {
                frame.push_str("\r\n");
            }
            frame.push_str(line);
        }
        frame.push_str(END_PAINT);
        if self.real {
            let mut stderr = std::io::stderr().lock();
            // The hook and every live paint share this lock. A panic cannot
            // emit its diagnostic and then race an already queued paint.
            if PANICKED.load(Ordering::Acquire) {
                return Ok(());
            }
            SCREEN_ACTIVE.store(true, Ordering::Release);
            RECOVERY_ROWS.store(session.recovery_rows, Ordering::Release);
            session.entered = true;
            stderr.write_all(frame.as_bytes())?;
            stderr.flush()?;
        } else {
            session.entered = true;
            out.write_all(frame.as_bytes())?;
            out.flush()?;
        }
        session.recovery_rows = 0;
        if self.real {
            RECOVERY_ROWS.store(0, Ordering::Release);
        }
        if self.size() == size {
            session.frame = Some((size, message.split('\n').count()));
        }
        #[cfg(test)]
        message.clone_into(&mut session.message);
        Ok(())
    }

    pub(super) fn close(&self, out: &mut dyn Write) -> io::Result<()> {
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if session.closed {
            return Ok(());
        }
        let mut frame = String::new();
        if session.entered {
            frame.push_str(PAINT);
            let previous = session.frame.take();
            let recovery = session.recovery_rows;
            session.recovery_rows = recovery.max(previous.map_or(0, |(_, rows)| rows));
            if recovery > 0 {
                frame.push_str(&ENTER.repeat(recovery + 1));
            } else if !clear_frame(&mut frame, previous, self.size()) {
                frame.push_str(ENTER);
            }
            frame.push_str(LEAVE);
        }
        if self.real {
            let mut stderr = std::io::stderr().lock();
            // The panic hook has already moved beyond the live view. Do not
            // move the cursor back over that diagnostic during destruction.
            if !PANICKED.load(Ordering::Acquire) && session.entered {
                RECOVERY_ROWS.store(session.recovery_rows, Ordering::Release);
                stderr.write_all(frame.as_bytes())?;
                RECOVERY_ROWS.store(0, Ordering::Release);
                SCREEN_ACTIVE.store(false, Ordering::Release);
            }
            session.entered = false;
            session.recovery_rows = 0;
            stderr.flush()?;
        } else {
            out.write_all(frame.as_bytes())?;
            session.entered = false;
            session.recovery_rows = 0;
            out.flush()?;
        }
        session.closed = true;
        // Finish the inline update before restoring the previous Windows mode.
        #[cfg(windows)]
        drop(session.mode.take());
        Ok(())
    }
}

impl Drop for LiveTerminal {
    fn drop(&mut self) {
        if self.real {
            // A failed write keeps the session open so destruction can retry.
            let _ = self.close(&mut io::sink());
        }
    }
}

// Hard line breaks and one unused terminal column keep every owned row from
// wrapping at the measured width. A geometry change invalidates that ownership.
fn clear_frame(
    frame: &mut String,
    previous: Option<((u16, u16), usize)>,
    size: (u16, u16),
) -> bool {
    use std::fmt::Write as _;
    let Some((geometry, lines)) = previous.filter(|(geometry, _)| *geometry == size) else {
        return false;
    };
    if lines == 0 || lines >= usize::from(geometry.0) {
        return false;
    }
    frame.push('\r');
    if lines > 1 {
        write!(frame, "\x1b[{}A", lines - 1).expect("string write");
    }
    for index in 0..lines {
        frame.push_str("\x1b[2K");
        if index + 1 < lines {
            frame.push_str("\r\n");
        }
    }
    if lines > 1 {
        write!(frame, "\x1b[{}A", lines - 1).expect("string write");
    }
    frame.push('\r');
    true
}

fn install_panic_restore() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANICKED.store(true, Ordering::Release);
            {
                let mut stderr = std::io::stderr().lock();
                if SCREEN_ACTIVE.load(Ordering::Acquire) {
                    let recovery = RECOVERY_ROWS.load(Ordering::Acquire);
                    let restore = format!("{END_PAINT}{}", ENTER.repeat(recovery + 1));
                    if stderr.write_all(restore.as_bytes()).is_ok() {
                        SCREEN_ACTIVE.store(false, Ordering::Release);
                    }
                    let _ = stderr.flush();
                }
            }
            original(info);
        }));
    });
}

fn supports_inline_updates(term: Option<&str>) -> bool {
    // Unknown terminal declarations use plain lines instead of cursor controls.
    term.is_some_and(|term| {
        [
            "xterm",
            "screen",
            "tmux",
            "rxvt",
            "alacritty",
            "foot",
            "wezterm",
            "ghostty",
            "kitty",
            "vte",
            "st",
        ]
        .iter()
        .any(|family| {
            term == *family
                || term
                    .strip_prefix(family)
                    .is_some_and(|suffix| suffix.starts_with('-'))
        })
    })
}

#[cfg(windows)]
struct OutputMode(Option<u32>);

#[cfg(windows)]
impl OutputMode {
    fn enable(term: Option<&str>) -> Option<Self> {
        // console also reports native consoles with VT already enabled as MSYS.
        // Check the native handle first, independently of TERM, and retain every
        // original output flag for restoration after the inline dashboard.
        if let Ok(old) = winapi_util::console::mode(std::io::stderr()) {
            // ENABLE_PROCESSED_OUTPUT is required alongside VT processing.
            const REQUIRED_OUTPUT: u32 = 0x0001 | 0x0004;
            winapi_util::console::set_mode(std::io::stderr(), old | REQUIRED_OUTPUT).ok()?;
            return Some(Self(Some(old)));
        }
        (console::Term::stderr().features().is_msys_tty() && supports_inline_updates(term))
            .then_some(Self(None))
    }
}

#[cfg(windows)]
impl Drop for OutputMode {
    fn drop(&mut self) {
        if let Some(mode) = self.0 {
            let _ = winapi_util::console::set_mode(std::io::stderr(), mode);
        }
    }
}

#[cfg(test)]
mod tests;
