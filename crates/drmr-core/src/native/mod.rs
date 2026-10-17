//! Operating-system boundaries for protected files and supervised processes.
//!
//! Manifest URLs and object keys retain their portable syntax. Native paths and
//! access controls are interpreted by the operating system at this boundary.

mod files;
pub use files::{
    PrivateTempDir, PrivateTempFile, create_private_dir_all, create_private_dir_new,
    create_private_new, create_private_or_truncate, create_private_update, open_private_read,
    private_tempdir_in, private_tempfile,
};
mod process;
pub(crate) use process::mark_asset_worker;
pub use process::{ManagedChild, in_asset_worker};
mod paths;
pub use paths::resolve_path;

#[cfg(windows)]
pub(crate) use paths::validate_windows_relative_path;

mod workspace;
pub use workspace::{create_media_dir_all, seal_media_tree};
