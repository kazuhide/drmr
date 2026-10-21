//! Operating-system boundaries for protected files and supervised processes.
//!
//! Manifest URLs and object keys retain their portable syntax. Native paths and
//! access controls are interpreted by the operating system at this boundary.

mod pipe;
pub(crate) use pipe::{CapturedStderr, CapturedStdout, prepare_stderr, prepare_stdout};
mod identity;
pub use identity::FileIdentity;
mod files;
#[cfg(windows)]
pub use files::{PrivateDirectoryGuard, pin_private_directory};
pub use files::{
    PrivateTempDir, PrivateTempFile, create_private_dir_all, create_private_dir_new,
    create_private_new, create_private_or_truncate, create_private_update, open_private_read,
    open_private_shared_read, private_tempdir, private_tempdir_in, private_tempfile,
};
#[cfg(unix)]
mod broker;
mod process;
pub(crate) use process::mark_asset_worker;
pub use process::{ManagedChild, ManagedIo, ManagedStdio, in_asset_worker};

/// Initialize the private, versioned server-to-CLI ownership protocol.
pub fn initialize_supervised_cli() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        broker::initialize()
    }
    #[cfg(windows)]
    {
        Ok(())
    }
}

/// Whether this CLI has completed the private server ownership handshake.
pub fn is_supervised_cli() -> bool {
    #[cfg(unix)]
    {
        broker::active()
    }
    #[cfg(windows)]
    {
        false
    }
}
mod paths;
pub use paths::resolve_path;

#[cfg(windows)]
pub use paths::validate_windows_relative_path;

mod workspace;
#[cfg(windows)]
pub use workspace::pin_media_directory;
pub use workspace::{
    create_media_dir_all, create_media_new, create_media_or_truncate, seal_media_tree,
};
