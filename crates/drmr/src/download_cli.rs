//! Remote-input classification and destination checks performed before acquisition.
use drmr_core::conversion::ConversionError;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub enum Input {
    Local(PathBuf),
    Url(String),
}

impl Input {
    pub fn parse(value: &OsStr) -> Result<Self, ConversionError> {
        if let Some(text) = value.to_str() {
            // Preserve drive paths, including forward-slash spellings, on every host.
            let drive = text.as_bytes().get(1) == Some(&b':')
                && text.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
            if !drive && let Some((scheme, _)) = text.split_once("://") {
                if scheme.eq_ignore_ascii_case("https") || scheme.eq_ignore_ascii_case("http") {
                    drmr_core::source::origin(text)
                        .map_err(|_| ConversionError("download-url-invalid"))?;
                    return Ok(Self::Url(text.into()));
                }
                if scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
                {
                    return Err(ConversionError("download-url-scheme-not-supported"));
                }
            }
        }
        Ok(Self::Local(value.into()))
    }
}

fn resolved(path: &Path) -> Result<PathBuf, ConversionError> {
    let invalid = ConversionError("download-destination-unsafe");
    let name = path.file_name().ok_or(invalid)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Resolve the original parent: removing `..` first would change symlink semantics.
    let parent = parent.canonicalize().map_err(|_| invalid)?;
    if !parent.is_dir() {
        return Err(invalid);
    }
    Ok(parent.join(name))
}

/// Check a new retained-source destination without creating it or requiring a future result.
pub fn preflight_source(directory: &Path) -> Result<(), ConversionError> {
    resolved(directory)?;
    if exists(directory)? {
        return Err(ConversionError("download-directory-exists"));
    }
    Ok(())
}

pub fn preflight(download: &Path, output: &Path) -> Result<(), ConversionError> {
    let source = resolved(download)?;
    let result = resolved(output)?;
    if source.starts_with(&result) || result.starts_with(&source) {
        return Err(ConversionError("download-output-overlap"));
    }
    if exists(download)? {
        return Err(ConversionError("download-directory-exists"));
    }
    if exists(output)? {
        return Err(ConversionError("output-exists-or-unsafe"));
    }
    // Check the original output spelling, not the canonical path used for overlap.
    drmr_core::conversion::preflight_download_output(output)?;
    // Acquisition reserves the source directory and rechecks native aliases before I/O.
    Ok(())
}

fn exists(path: &Path) -> Result<bool, ConversionError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(ConversionError("download-destination-unsafe")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classification_preserves_windows_drive_paths_and_rejects_other_url_schemes() {
        for path in [
            r"C:\media\input.mp4",
            "C://media/input.mp4",
            "./source.m3u8",
        ] {
            assert!(
                matches!(Input::parse(path.as_ref()).unwrap(), Input::Local(p) if p == Path::new(path))
            );
        }
        assert!(Input::parse("ftp://example.test/source".as_ref()).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn local_non_utf8_input_bytes_are_preserved() {
        use std::os::unix::ffi::OsStrExt;
        let path = OsStr::from_bytes(b"source-\xff.mp4");
        assert!(matches!(Input::parse(path).unwrap(), Input::Local(p) if p.as_os_str() == path));
    }
}
