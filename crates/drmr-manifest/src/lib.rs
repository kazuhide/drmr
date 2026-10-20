#![doc = include_str!("../README.md")]

mod dash;
mod executor;
mod hls;
mod lines;
mod model;
mod sink;
pub(crate) use sink::DiagnosticSink;
mod uri;
pub use executor::{estimate_document_sizes, estimate_resources, lint_document, lint_presentation};
pub use hls::child_manifest_references;
pub use model::*;
