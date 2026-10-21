//! Origin and executable policy shared by the native host and its tests.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use url::Url;

#[cfg(feature = "desktop")]
pub mod plan_export;

/// The exact listener owned by this application instance.
#[derive(Clone, Debug)]
pub struct ConsoleOrigin(Url);

impl ConsoleOrigin {
    pub fn new(address: SocketAddr) -> Option<Self> {
        if address.ip() != IpAddr::V4(Ipv4Addr::LOCALHOST) || address.port() == 0 {
            return None;
        }
        Some(Self(Url::parse(&format!("http://{address}")).ok()?))
    }

    pub fn url(&self) -> &Url {
        &self.0
    }

    pub fn permits_navigation(&self, url: &Url) -> bool {
        url.scheme() == "http"
            && url.username().is_empty()
            && url.password().is_none()
            && url.origin() == self.0.origin()
    }

    /// Permit only an original plan attachment from this console instance.
    pub fn permits_plan_export(&self, url: &Url) -> bool {
        let Some(id) = url
            .path()
            .strip_prefix("/api/common/plans/")
            .and_then(|path| path.strip_suffix("/export"))
        else {
            return false;
        };
        self.permits_navigation(url)
            && url.query().is_none()
            && url.fragment().is_none()
            && id.len() == 32
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

/// Locate only the bundled CLI next to the native executable.
pub fn bundled_cli(executable: &Path) -> Option<PathBuf> {
    if !executable.is_absolute() {
        return None;
    }
    executable
        .parent()
        .map(|parent| parent.join(format!("drmr{}", std::env::consts::EXE_SUFFIX)))
}

/// Permit only the bundled, script-free startup document before a server exists.
pub fn permits_startup_navigation(url: &Url) -> bool {
    url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && matches!(url.path(), "" | "/" | "/index.html")
        && matches!(
            (url.scheme(), url.host_str()),
            ("tauri", Some("localhost")) | ("http", Some("tauri.localhost"))
        )
}
