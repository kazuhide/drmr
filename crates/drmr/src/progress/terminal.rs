//! An owned dashboard screen, independent of previously wrapped terminal rows.

use std::io::{self, IsTerminal as _, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, Once, PoisonError};

const ENTER: &str = "\r\n\x1b[?1049h";
const PAINT: &str = "\x1b[?2026h\x1b[H\x1b[2J";
const END_PAINT: &str = "\x1b[?2026l";
const LEAVE: &str = "\x1b[?2026l\x1b[?1049l";

static SCREEN_ACTIVE: AtomicBool = AtomicBool::new(false);
static PANICKED: AtomicBool = AtomicBool::new(false);

struct Session {
    entered: bool,
    closed: bool,
    #[cfg(windows)]
    mode: Option<OutputMode>,
    #[cfg(test)]
    message: String,
}

pub(super) struct LiveTerminal {
    session: Mutex<Session>,
    real: bool,
    dimensions: Option<(u16, u16)>,
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
        if !supports_screen_switching(term.as_deref()) {
            return None;
        }
        #[cfg(windows)]
        let mode = OutputMode::enable(term.as_deref())?;
        install_panic_restore();
        Some(Self {
            session: Mutex::new(Session {
                entered: false,
                closed: false,
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
                #[cfg(windows)]
                mode: None,
                message: String::new(),
            }),
            real: false,
            dimensions: Some((rows, columns)),
        }
    }

    pub(super) fn size(&self) -> (u16, u16) {
        self.dimensions
            .unwrap_or_else(|| console::Term::stderr().size())
    }

    #[cfg(test)]
    pub(super) fn message(&self) -> String {
        self.session
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .message
            .clone()
    }

    pub(super) fn draw(&self, out: &mut dyn Write, message: &str) -> io::Result<()> {
        let mut session = self.session.lock().unwrap_or_else(PoisonError::into_inner);
        if session.closed {
            return Ok(());
        }
        // One bounded frame and one writer: no terminal-width padding or
        // relative cursor position survives into the next paint.
        let mut frame = String::with_capacity(message.len() + 64);
        if !session.entered {
            frame.push_str(ENTER);
        }
        frame.push_str(PAINT);
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
            // restore the primary screen and then race an already queued paint.
            if PANICKED.load(Ordering::Acquire) {
                return Ok(());
            }
            SCREEN_ACTIVE.store(true, Ordering::Release);
            session.entered = true;
            stderr.write_all(frame.as_bytes())?;
            stderr.flush()?;
        } else {
            session.entered = true;
            out.write_all(frame.as_bytes())?;
            out.flush()?;
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
        if self.real {
            let mut stderr = std::io::stderr().lock();
            if session.entered && SCREEN_ACTIVE.load(Ordering::Acquire) {
                stderr.write_all(LEAVE.as_bytes())?;
                SCREEN_ACTIVE.store(false, Ordering::Release);
            }
            // The panic hook may already have restored the primary screen.
            // Repeating 1049l can restore its saved cursor over that diagnostic.
            session.entered = false;
            stderr.flush()?;
        } else {
            if session.entered {
                out.write_all(LEAVE.as_bytes())?;
            }
            session.entered = false;
            out.flush()?;
        }
        session.closed = true;
        // Leave while VT processing is still enabled, then restore its prior mode.
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

fn install_panic_restore() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        let original = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANICKED.store(true, Ordering::Release);
            {
                let mut stderr = std::io::stderr().lock();
                if SCREEN_ACTIVE.load(Ordering::Acquire) {
                    if stderr.write_all(LEAVE.as_bytes()).is_ok() {
                        SCREEN_ACTIVE.store(false, Ordering::Release);
                    }
                    let _ = stderr.flush();
                }
            }
            original(info);
        }));
    });
}

fn supports_screen_switching(term: Option<&str>) -> bool {
    // Basic ANSI cursor controls do not imply an alternate screen (for example,
    // the Linux virtual console). Unknown terminal declarations use plain lines.
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
        if console::Term::stderr().features().is_msys_tty() {
            return supports_screen_switching(term).then_some(Self(None));
        }
        let old = winapi_util::console::mode(std::io::stderr()).ok()?;
        winapi_util::console::Console::stderr()
            .ok()?
            .set_virtual_terminal_processing(true)
            .ok()?;
        Some(Self(Some(old)))
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
