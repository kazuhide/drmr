//! Shared manifest helpers for numeric, path, XML, and error handling.
//!
//! Compatibility helpers preserve the `CPython` behavior that affects serialized bytes
//! or parsing decisions. Configuration, signaling, and media types come from their
//! respective core modules.

pub mod fraction;
pub mod path;
pub mod py;
pub mod xml;

use crate::config::ConfigError;
use crate::media::MediaError;
use py::{PyErr, PyOsError};

/// The `OSError` of an I/O failure whose file name the failing module did not keep:
/// `[Errno N] <strerror>`, as Python prints an `OSError` without a file name.
pub fn os_error(error: &std::io::Error) -> PyErr {
    let text = error.to_string();
    let message = match error.raw_os_error() {
        Some(code) => {
            let strerror = text
                .strip_suffix(&format!(" (os error {code})"))
                .unwrap_or(&text);
            format!("[Errno {code}] {strerror}")
        }
        None => text,
    };
    PyErr::Os(PyOsError {
        kind: error.kind(),
        errno: error.raw_os_error(),
        message,
    })
}

/// `config`'s exceptions as the manifest modules raise them: `find_download_root` and
/// `resolve_local_path` raise `ValueError` (not nested under `download/`, a path escaping the
/// root), `RuntimeError` (a symlink loop in `Path.resolve`) and `OSError`.
impl From<ConfigError> for PyErr {
    fn from(error: ConfigError) -> Self {
        match error {
            ConfigError::Value(message) => PyErr::Value(message),
            ConfigError::Xml(message) => PyErr::Other {
                class: "ParseError",
                message,
            },
            ConfigError::Runtime(message) => PyErr::Runtime(message),
            ConfigError::ZeroDivision => PyErr::ZeroDivision("division by zero".to_string()),
            ConfigError::Io(error) => os_error(&error),
        }
    }
}

/// The media helpers' exceptions as the gates see them: `HlsMediaError`, `ToolError` and the
/// tools' own failures are `RuntimeError`s (the only class the gates catch, recording
/// `str(exc)`); the rest keep their class and propagate.
impl From<MediaError> for PyErr {
    fn from(error: MediaError) -> Self {
        match error {
            MediaError::Hls(_) | MediaError::Runtime(_) | MediaError::Tool(_) => {
                PyErr::Runtime(error.to_string())
            }
            MediaError::Value(message) => PyErr::Value(message),
            MediaError::Os(error) => os_error(&error),
            MediaError::Key(message) => PyErr::Other {
                class: "KeyError",
                message,
            },
            MediaError::Uncaught { class, message } => match class {
                "IndexError" => PyErr::Index(message),
                "ZeroDivisionError" => PyErr::ZeroDivision(message),
                "OverflowError" => PyErr::Overflow(message),
                _ => PyErr::Other { class, message },
            },
        }
    }
}
