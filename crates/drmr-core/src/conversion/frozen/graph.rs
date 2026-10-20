//! Discover references only from the manifest bytes already copied into the private graph.
use super::{ConversionError, INVALID, LIMIT, MAX_FILES, cancelled, relative};
use crate::concurrency::Stop;
use std::{
    collections::BTreeSet,
    io::Read as _,
    path::{Component, Path, PathBuf},
};

pub(super) fn walk(
    stage: &Path,
    entrypoint: &Path,
    stop: &Stop,
    mut ready: impl FnMut(&Path) -> Result<(), ConversionError>,
) -> Result<(), ConversionError> {
    ready(entrypoint)?;
    let mut prefix = [0u8; 64];
    let count = super::super::files::regular_input(&stage.join(entrypoint))?
        .read(&mut prefix)
        .map_err(|_| INVALID)?;
    if count >= 8 && matches!(&prefix[4..8], b"ftyp" | b"styp" | b"moov" | b"free") {
        return Ok(());
    }
    let bytes = super::super::files::read_manifest(&stage.join(entrypoint))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| INVALID)?
        .trim_start_matches('\u{feff}')
        .trim_start();
    if text.starts_with("#EXTM3U") {
        let mut seen = BTreeSet::new();
        hls(stage, entrypoint, stop, &mut ready, &mut seen, 0)
    } else {
        for path in super::super::dash::source_references(&bytes)? {
            cancelled(stop)?;
            let path = normalize(&path)?;
            ready(&path)?;
        }
        Ok(())
    }
}

fn normalize(path: &Path) -> Result<PathBuf, ConversionError> {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::Normal(name) => result.push(name),
            _ => return Err(INVALID),
        }
    }
    relative(&result)?;
    Ok(result)
}
fn reference(base: &Path, uri: &str) -> Result<PathBuf, ConversionError> {
    if uri.is_empty()
        || uri.len() > 4096
        || uri.chars().any(char::is_control)
        || uri.contains([':', '\\', '?', '#'])
        || uri.bytes().enumerate().any(|(at, byte)| {
            byte == b'%'
                && !uri
                    .as_bytes()
                    .get(at + 1..at + 3)
                    .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
        })
    {
        return Err(INVALID);
    }
    let decoded = crate::source::pyurl::unquote(uri);
    if decoded.contains('\u{fffd}') {
        return Err(INVALID);
    }
    normalize(&base.join(normalize(Path::new(&decoded))?))
}
fn hls(
    stage: &Path,
    manifest: &Path,
    stop: &Stop,
    ready: &mut impl FnMut(&Path) -> Result<(), ConversionError>,
    seen: &mut BTreeSet<PathBuf>,
    depth: usize,
) -> Result<(), ConversionError> {
    cancelled(stop)?;
    if depth > 1 || seen.len() >= 65 {
        return Err(LIMIT);
    }
    if !seen.insert(manifest.to_path_buf()) {
        return Ok(());
    }
    let bytes = super::super::files::read_manifest(&stage.join(manifest))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| INVALID)?
        .trim_start_matches('\u{feff}');
    let lines: Vec<_> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.first() != Some(&"#EXTM3U") {
        return Err(INVALID);
    }
    let master = lines
        .iter()
        .any(|line| line.starts_with("#EXT-X-STREAM-INF:") || line.starts_with("#EXT-X-MEDIA:"));
    if depth > 0 && master {
        return Err(INVALID);
    }
    let base = manifest.parent().unwrap_or(Path::new(""));
    let mut references = 0usize;
    for line in lines {
        cancelled(stop)?;
        let mut child = false;
        let uri = if !line.starts_with('#') {
            child = master;
            Some(line.to_owned())
        } else if line.starts_with("#EXT-X-MAP:") || line.starts_with("#EXT-X-MEDIA:") {
            child = line.starts_with("#EXT-X-MEDIA:");
            let attrs = super::super::hls::attributes(line.split_once(':').ok_or(INVALID)?.1)?;
            attrs.get("URI").map(str::to_owned)
        } else {
            // Key addresses identify operator-supplied keys. They never become fetches.
            if line.contains("URI=") && !line.starts_with("#EXT-X-KEY:") {
                return Err(INVALID);
            }
            None
        };
        if let Some(uri) = uri {
            references += 1;
            if references > MAX_FILES {
                return Err(LIMIT);
            }
            let path = reference(base, &uri)?;
            ready(&path)?;
            if child {
                hls(stage, &path, stop, ready, seen, depth + 1)?;
            }
        }
    }
    Ok(())
}
