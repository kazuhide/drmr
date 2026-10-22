//! Media conversion, manifest handling, bounded acquisition, and secure tool execution.

pub mod canonical;
pub mod concurrency;
pub mod conversion;
pub mod events;
pub mod inspection;
pub mod m3u8;
pub mod manifest_support;
pub mod media;
pub mod mp4;
pub mod playready;
pub mod pycompat;
pub mod resources;
pub mod source;
pub mod tool_diagnostics;
pub mod toolrun;

/// The crate version (the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub mod native;
#[cfg(test)]
mod test_harness;
