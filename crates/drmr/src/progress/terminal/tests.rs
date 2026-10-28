use super::*;

#[test]
fn screen_switching_requires_a_known_terminal_family() {
    for term in [
        "xterm",
        "xterm-256color",
        "screen",
        "screen-256color",
        "tmux-256color",
        "rxvt-unicode-256color",
        "alacritty",
        "foot-extra",
        "wezterm",
        "ghostty",
        "xterm-kitty",
        "vte-256color",
        "st-256color",
    ] {
        assert!(supports_screen_switching(Some(term)), "{term}");
    }
    for term in [
        None,
        Some(""),
        Some("dumb"),
        Some("linux"),
        Some("vt100"),
        Some("vt220"),
        Some("ansi"),
        Some("unknown"),
        Some("xterminal"),
        Some("screenly"),
    ] {
        assert!(!supports_screen_switching(term), "{term:?}");
    }
}

#[test]
fn screen_entry_paints_and_restore_have_one_ordered_owner() {
    let terminal = LiveTerminal::for_test(120, 24);
    let mut out = b"preceding partial line".to_vec();
    terminal.draw(&mut out, "first\nsecond").unwrap();
    terminal.draw(&mut out, "replacement").unwrap();
    terminal.close(&mut out).unwrap();
    terminal.close(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.starts_with(&format!("preceding partial line{ENTER}{PAINT}")));
    assert_eq!(text.matches(ENTER).count(), 1);
    assert_eq!(text.matches(PAINT).count(), 2);
    assert!(text.contains(&format!("first\r\nsecond{END_PAINT}")));
    assert!(text.ends_with(LEAVE));
    assert_eq!(text.matches("\x1b[?1049l").count(), 1);
    assert!(!text.contains("\x1b[3J"));
    assert!(!text.contains("\x1b[?25"));
}

#[test]
fn closed_sessions_never_reenter_or_clear_the_result() {
    let terminal = LiveTerminal::for_test(80, 12);
    let mut out = Vec::new();
    terminal.draw(&mut out, "live").unwrap();
    terminal.close(&mut out).unwrap();
    out.extend_from_slice(b"result\n");
    let saved = out.clone();
    terminal.draw(&mut out, "late event").unwrap();
    assert_eq!(out, saved);
    let untouched = LiveTerminal::for_test(80, 12);
    untouched.close(&mut out).unwrap();
    untouched.draw(&mut out, "never started").unwrap();
    assert_eq!(out, saved);
}

#[derive(Default)]
struct Interrupted {
    bytes: Vec<u8>,
    remaining: Option<usize>,
    fail_flush: bool,
}

impl Write for Interrupted {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let amount = self
            .remaining
            .map_or(bytes.len(), |left| left.min(bytes.len()));
        if amount == 0 && !bytes.is_empty() {
            return Err(io::Error::other("fixture output unavailable"));
        }
        self.bytes.extend_from_slice(&bytes[..amount]);
        if let Some(remaining) = &mut self.remaining {
            *remaining -= amount;
        }
        Ok(amount)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::Error::other("fixture flush unavailable"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn partial_entry_and_paint_failure_still_allow_restoration() {
    for limit in [
        0,
        1,
        3,
        ENTER.len() - 1,
        ENTER.len(),
        ENTER.len() + PAINT.len() + 2,
    ] {
        let terminal = LiveTerminal::for_test(80, 12);
        let mut out = Interrupted {
            remaining: Some(limit),
            ..Interrupted::default()
        };
        assert!(terminal.draw(&mut out, "first\nsecond").is_err());
        assert_eq!(out.bytes.len(), limit);
        // A failed alternate-screen entry never proceeds to the screen clear.
        if limit < ENTER.len() {
            assert!(!out.bytes.windows(4).any(|bytes| bytes == b"\x1b[2J"));
        }
        out.remaining = None;
        terminal.close(&mut out).unwrap();
        assert!(out.bytes.ends_with(LEAVE.as_bytes()));
        let saved = out.bytes.clone();
        terminal.close(&mut out).unwrap();
        assert_eq!(out.bytes, saved);
    }
}

#[test]
fn unsuccessful_restore_is_retried_before_the_session_is_closed() {
    let terminal = LiveTerminal::for_test(80, 12);
    let mut out = Interrupted::default();
    terminal.draw(&mut out, "live").unwrap();
    out.remaining = Some(2);
    assert!(terminal.close(&mut out).is_err());
    out.remaining = None;
    terminal.close(&mut out).unwrap();
    assert!(out.bytes.ends_with(LEAVE.as_bytes()));
    let saved = out.bytes.clone();
    terminal.draw(&mut out, "late event").unwrap();
    assert_eq!(out.bytes, saved);
}

#[test]
fn a_flush_error_does_not_discard_cleanup_authority() {
    let terminal = LiveTerminal::for_test(80, 12);
    let mut out = Interrupted {
        fail_flush: true,
        ..Interrupted::default()
    };
    assert!(terminal.draw(&mut out, "live").is_err());
    assert!(terminal.close(&mut out).is_err());
    out.fail_flush = false;
    terminal.close(&mut out).unwrap();
    assert!(out.bytes.ends_with(LEAVE.as_bytes()));
    let text = String::from_utf8(out.bytes).unwrap();
    assert_eq!(text.matches(LEAVE).count(), 1);
}

#[test]
fn panic_restore_precedes_the_existing_diagnostic_hook() {
    const CHILD: &str = "DRMR_TEST_TERMINAL_PANIC_CHILD";
    const NAME: &str =
        "progress::terminal::tests::panic_restore_precedes_the_existing_diagnostic_hook";
    if std::env::var_os(CHILD).is_some() {
        install_panic_restore();
        let mut terminal = LiveTerminal::for_test(80, 12);
        terminal.real = true;
        terminal
            .draw(&mut io::sink(), "panic fixture dashboard")
            .unwrap();
        panic!("terminal panic diagnostic fixture");
    }
    let root = tempfile::tempdir().unwrap();
    let stdout = root.path().join("stdout");
    let stderr = root.path().join("stderr");
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", NAME, "--nocapture"])
        .env(CHILD, "1")
        .env("RUST_BACKTRACE", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(stdout).unwrap())
        .stderr(std::fs::File::create(&stderr).unwrap());
    let mut child = drmr_core::native::ManagedChild::spawn(command, true).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            child.finish().unwrap();
            panic!("terminal panic fixture exceeded its deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    };
    child.finish().unwrap();
    assert!(!status.success());
    let bytes = std::fs::read(stderr).unwrap();
    assert!(bytes.len() < 64 * 1024);
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.find(LEAVE).unwrap() < text.find("terminal panic diagnostic fixture").unwrap());
    assert_eq!(
        text.matches(LEAVE).count(),
        1,
        "panic cleanup must not restore the saved cursor twice"
    );
}
