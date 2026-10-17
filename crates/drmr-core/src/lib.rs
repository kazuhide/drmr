//! Media repackaging, manifest handling, source snapshots, and secure tool execution.

// The batch layer and its storage are POSIX-only (process groups, file modes).
#[cfg(unix)]
pub mod batch;
#[cfg(unix)]
pub mod batch_cli;
#[cfg(unix)]
pub mod batch_support;
pub mod canonical;
pub mod checks;
pub mod cli;
pub mod concurrency;
pub mod config;
pub mod events;
pub mod hls_cli;
pub mod hls_pipeline;
pub mod hls_pipeline_deps;
pub mod inspection;
pub mod m3u8;
pub mod manifest_dash;
pub mod manifest_gen;
pub mod manifest_hls;
pub mod manifest_support;
pub mod media;
pub mod mp4;
pub mod packager;
pub mod pipeline;
pub mod pipeline_deps;
pub mod playready;
pub mod pycompat;
pub mod snapshot;
pub mod source;
#[cfg(unix)]
pub mod storage;
pub mod toolrun;

/// The crate version (the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
