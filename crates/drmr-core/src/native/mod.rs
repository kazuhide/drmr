//! Operating-system boundaries for protected files and supervised processes.
//!
//! Manifest URLs and object keys retain their portable syntax. Native paths and
//! access controls are interpreted by the operating system at this boundary.

mod files;
pub use files::{create_private_dir_all, create_private_new, open_private_read};
mod process;
pub use process::ManagedChild;
