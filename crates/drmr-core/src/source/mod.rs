//! Bounded fetching and playlist parsing shared by URL media acquisition.
//!
//! Source addresses remain private. Errors expose fixed classification tokens. Each
//! network attempt uses a validated pinned address, bounded I/O, and verified TLS.

mod fetch;
pub mod http;
pub mod netaddr;
pub mod platform;
pub(crate) mod playlist;
mod policy;
pub(crate) mod progress;
pub mod pypath;
pub mod pytext;
pub mod pyurl;

pub use crate::m3u8::SourceError;
pub(crate) use fetch::MetadataRead;
pub use fetch::{ByteRange, Fetched, Fetcher};
pub use playlist::{COMMON, MASTER, MEDIA, PlaylistKind, lines, positive_int};
pub use policy::{FetchPolicy, MAX_CONNECTIONS};

use crate::concurrency::UnitError;
use pyurl::{urljoin, urlsplit};
use std::fmt;

/// An unclassified source error, preserved by class name: the CLI
/// reports `source-config-invalid` (exit 2) for an `OSError`, `ValueError` or `TypeError` and
/// `source-infrastructure-error` (exit 2) for any other class (`the analysis command`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Escaped {
    /// The Python class: `OSError`, `ValueError`, `TypeError`, `AttributeError`, `KeyError`,
    /// `OverflowError`, `RuntimeError` (a symlink loop in `Path.resolve`) or `Cancelled`.
    pub class: &'static str,
}

impl Escaped {
    /// An escaping exception of `class`.
    pub fn new(class: &'static str) -> Self {
        Escaped { class }
    }

    /// Whether the CLI's `except (ValueError, TypeError, OSError)` catches it.
    pub fn is_config_class(self) -> bool {
        matches!(self.class, "OSError" | "ValueError" | "TypeError")
    }
}

impl fmt::Display for Escaped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.class)
    }
}

/// How an analysis, a fetch or a unit of one failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    /// A classification token.
    #[error("{0}")]
    Source(SourceError),
    /// The unit stopped because the analysis is stopping (`concurrency.Cancelled`).
    #[error("cancelled")]
    Cancelled,
    /// An unclassified exception escaping as it is.
    #[error("{0}")]
    Escaped(Escaped),
}

impl Failure {
    /// The token, when this is a [`SourceError`].
    pub fn token(&self) -> Option<&str> {
        match self {
            Failure::Source(error) => Some(error.token()),
            _ => None,
        }
    }

    pub(crate) fn source(name: &str) -> Failure {
        Failure::Source(SourceError::new(name))
    }
}

impl From<SourceError> for Failure {
    fn from(error: SourceError) -> Self {
        Failure::Source(error)
    }
}

impl UnitError for Failure {
    fn cancelled() -> Self {
        Failure::Cancelled
    }

    fn is_cancelled(&self) -> bool {
        matches!(self, Failure::Cancelled)
    }
}

/// `_origin(url)`: `scheme://host[:port]` with the host in lower case (an IPv6 address in
/// brackets) and the scheme's default port omitted; a URL without a host, with credentials or
/// a fragment, or that does not parse, is `source-not-allowed`.
pub fn origin(url: &str) -> Result<String, SourceError> {
    let refused = || SourceError::new("source-not-allowed");
    let parts = urlsplit(url, "").map_err(|_| refused())?;
    let Some(host) = parts.hostname() else {
        return Err(refused());
    };
    let has = |value: Option<&str>| value.is_some_and(|text| !text.is_empty());
    if has(parts.username()) || has(parts.password()) || !parts.fragment.is_empty() {
        return Err(refused());
    }
    let default_port = if parts.scheme == "https" { 443 } else { 80 };
    let port = match parts.port().map_err(|_| refused())? {
        Some(port) if port != 0 => port,
        _ => default_port,
    };
    let host = host.to_lowercase();
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    };
    let suffix = if port == default_port {
        String::new()
    } else {
        format!(":{port}")
    };
    Ok(format!("{}://{host}{suffix}", parts.scheme))
}

/// `_child_url(base, reference)`: `urljoin`, refusing control characters in the reference
/// and a remote playlist's reference to a `file:` URL (a remotely controlled playlist never
/// gains the operator's local-file capability). A URL the parser rejects is Python's
/// `ValueError`, escaping as it is.
pub fn child_url(base: &str, reference: &str) -> Result<String, Failure> {
    if reference.chars().any(|c| u32::from(c) < 32) {
        return Err(Failure::source("source-not-allowed"));
    }
    let value_error = |_| Failure::Escaped(Escaped::new("ValueError"));
    let result = urljoin(base, reference).map_err(value_error)?;
    let base_scheme = urlsplit(base, "").map_err(value_error)?.scheme;
    if (base_scheme == "https" || base_scheme == "http")
        && urlsplit(&result, "").map_err(value_error)?.scheme == "file"
    {
        return Err(Failure::source("source-not-allowed"));
    }
    Ok(result)
}
