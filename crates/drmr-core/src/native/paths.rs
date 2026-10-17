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

/// Refuse Windows device names, alternate streams and normalized filename aliases.
#[cfg(any(windows, test))]
pub(crate) fn validate_windows_relative_path(key: &str) -> io::Result<()> {
    for component in key.split('/') {
        let stem = component
            .split('.')
            .next()
            .unwrap_or("")
            .trim_end_matches(' ');
        let reserved = ["CON", "PRN", "AUX", "NUL", "CLOCK$", "CONIN$", "CONOUT$"]
            .iter()
            .any(|name| stem.eq_ignore_ascii_case(name))
            || ["COM", "LPT"].iter().any(|prefix| {
                stem.get(..3)
                    .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                    && matches!(
                        stem.get(3..),
                        Some("1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
                    )
            });
        if component.is_empty()
            || component.ends_with([' ', '.'])
            || component.contains(['<', '>', ':', '"', '\\', '|', '?', '*'])
            || component.chars().any(|character| character < ' ')
            || reserved
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid Windows file name",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_relative_names_cannot_be_devices_streams_or_normalized_aliases() {
        for key in [
            "C:/outside",
            "x:stream",
            "folder/CON",
            "aux.mp4",
            "NUL .txt",
            "PRN",
            "COM1",
            "com9.txt",
            "LPT1",
            "LPT³.bin",
            "CONIN$",
            "CONOUT$",
            "trailing.",
            "trailing ",
            "folder/..",
            "",
            "a//b",
            "a\\b",
            "wild*card",
        ] {
            assert!(super::validate_windows_relative_path(key).is_err(), "{key}");
        }
        for key in [
            "folder/normal.mp4",
            "spaces and 日本語/part_1.m4s",
            "COM10.mp4",
            "conifer.txt",
        ] {
            super::validate_windows_relative_path(key).unwrap();
        }
    }
}
