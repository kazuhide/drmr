//! Native filesystem paths, separate from URL and manifest path syntax.

use std::io;
use std::path::{Component, Path, PathBuf};

/// Resolve existing components through the filesystem, retaining missing children.
/// Native absolute-path normalization runs first, followed by canonicalization of
/// each existing prefix. Permission errors, redirect loops and other lookup failures
/// are preserved rather than treated as missing entries.
pub fn resolve_path(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(_) | Component::RootDir => resolved.push(component),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                resolved.push(name);
                match std::fs::symlink_metadata(&resolved) {
                    Ok(_) => resolved = std::fs::canonicalize(&resolved)?,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(resolved)
}
