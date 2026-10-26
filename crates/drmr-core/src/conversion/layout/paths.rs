//! Pure placement and URI serialization; no network or filesystem capabilities.
use super::{COLLISION, ConversionError, INVALID, LayoutMode, UNSUPPORTED};
use crate::source::pyurl;
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};
use unicode_normalization::UnicodeNormalization as _;

const MAX_PATH: usize = 4096;
const MAX_ENTRIES: usize = 100_000;
const MAX_BYTES: usize = 128 * 1024 * 1024;

/// Acquisition-only mapping state. Source addresses are never included in its debug output.
pub struct UrlPathMapper {
    mode: LayoutMode,
    base: Vec<String>,
    assigned: BTreeMap<String, PathBuf>,
    claims: PathClaims,
    bytes: usize,
}
impl std::fmt::Debug for UrlPathMapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UrlPathMapper")
            .field("mode", &self.mode)
            .field("resources", &self.assigned.len())
            .finish_non_exhaustive()
    }
}
impl UrlPathMapper {
    /// Bind placement to the effective entrypoint's URL-resolved directory.
    pub fn new(root_url: &str, mode: LayoutMode) -> Result<Self, ConversionError> {
        split(root_url)?;
        crate::source::origin(root_url).map_err(|_| INVALID)?;
        let base = if mode == LayoutMode::Source {
            let directory = pyurl::urljoin(root_url, ".").map_err(|_| INVALID)?;
            let parts = split(&directory)?;
            let path = parts.path.strip_suffix('/').ok_or(INVALID)?;
            if path.is_empty() {
                Vec::new()
            } else {
                url_components(path)?
            }
        } else {
            Vec::new()
        };
        Ok(Self {
            mode,
            base,
            assigned: BTreeMap::new(),
            claims: PathClaims::default(),
            bytes: 0,
        })
    }

    /// Keep one placement per exact URL and reject any distinct address with an aliasing path.
    pub fn map(&mut self, url: &str, fallback_relative: &str) -> Result<PathBuf, ConversionError> {
        self.map_admitted(url, fallback_relative, &mut |_| Ok(()))
    }

    pub(in crate::conversion) fn map_admitted(
        &mut self,
        url: &str,
        fallback_relative: &str,
        admit: &mut dyn FnMut(usize) -> Result<(), ConversionError>,
    ) -> Result<PathBuf, ConversionError> {
        if let Some(path) = self.assigned.get(url) {
            return Ok(path.clone());
        }
        let parts = split(url)?;
        crate::source::origin(url).map_err(|_| INVALID)?;
        if self.assigned.len() >= MAX_ENTRIES {
            return Err(ConversionError("download-graph-limit"));
        }
        let path = if self.mode == LayoutMode::Normalized {
            PathBuf::from(fallback_relative)
        } else {
            if !parts.fragment.is_empty() {
                return Err(UNSUPPORTED);
            }
            let components = url_components(&parts.path)?;
            if !components.starts_with(&self.base) || components.len() == self.base.len() {
                return Err(UNSUPPORTED);
            }
            let relative = components[self.base.len()..].join("/");
            let reserved = relative.to_ascii_lowercase();
            if reserved.starts_with("raw/")
                || reserved == "raw"
                || matches!(reserved.as_str(), "input.m3u8" | "input.mpd" | "input.mp4")
            {
                return Err(COLLISION);
            }
            PathBuf::from(relative)
        };
        relative_components(&path)?;
        let candidates = self.claims.prepare(&path)?;
        let mut charge = url
            .len()
            .checked_add(path.capacity().checked_mul(2).ok_or(INVALID)?)
            .and_then(|n| n.checked_add(512))
            .ok_or(INVALID)?;
        // Each claim owns the complete ancestor, not just its last component.
        // Include native path storage and conservative B-tree allocation overhead.
        for (key, path, _) in &candidates {
            charge = charge
                .checked_add(key.capacity())
                .and_then(|n| n.checked_add(path.capacity().checked_mul(2)?))
                .and_then(|n| n.checked_add(512))
                .ok_or(INVALID)?;
        }
        let bytes = self
            .bytes
            .checked_add(charge)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or(ConversionError("download-graph-limit"))?;
        admit(charge)?;
        self.bytes = bytes;
        self.claims.commit(candidates);
        self.assigned.insert(url.into(), path.clone());
        Ok(path)
    }

    /// Conservative retained-map charge; the owning graph includes it in its existing budget.
    pub fn retained_bytes(&self) -> usize {
        self.bytes
    }
}

#[derive(Default)]
pub(super) struct PathClaims(BTreeMap<String, (PathBuf, bool)>);
impl PathClaims {
    fn prepare(&self, path: &Path) -> Result<Vec<(String, PathBuf, bool)>, ConversionError> {
        let components = relative_components(path)?;
        let mut candidates = Vec::with_capacity(components.len());
        let mut relative = PathBuf::new();
        for (index, component) in components.iter().enumerate() {
            relative.push(component);
            let key = alias(&relative)?;
            let directory = index + 1 < components.len();
            if let Some((existing, is_directory)) = self.0.get(&key) {
                if !directory || !*is_directory || existing != &relative {
                    return Err(COLLISION);
                }
            } else {
                candidates.push((key, relative.clone(), directory));
            }
        }
        Ok(candidates)
    }
    fn commit(&mut self, candidates: Vec<(String, PathBuf, bool)>) {
        for (key, path, directory) in candidates {
            self.0.insert(key, (path, directory));
        }
    }
    pub(super) fn reserve_file(&mut self, path: &Path) -> Result<(), ConversionError> {
        let candidates = self.prepare(path)?;
        self.commit(candidates);
        Ok(())
    }
}

fn split(url: &str) -> Result<pyurl::SplitResult, ConversionError> {
    if url.len() > MAX_PATH || url.chars().any(char::is_control) {
        return Err(INVALID);
    }
    let result = pyurl::urlsplit(url, "").map_err(|_| INVALID)?;
    if !matches!(result.scheme.as_str(), "https" | "http") || result.netloc.is_empty() {
        return Err(INVALID);
    }
    Ok(result)
}

fn url_components(path: &str) -> Result<Vec<String>, ConversionError> {
    let path = path.strip_prefix('/').ok_or(INVALID)?;
    let mut result = Vec::new();
    for component in path.split('/') {
        if component.is_empty()
            || component.bytes().enumerate().any(|(at, b)| {
                b == b'%'
                    && !component
                        .as_bytes()
                        .get(at + 1..at + 3)
                        .is_some_and(|digits| digits.iter().all(u8::is_ascii_hexdigit))
            })
        {
            return Err(INVALID);
        }
        let component = pyurl::unquote(component);
        if component.contains(['/', '\\', '\u{fffd}']) {
            return Err(INVALID);
        }
        relative_components(Path::new(&component))?;
        result.push(component);
    }
    Ok(result)
}

pub(super) fn relative_components(path: &Path) -> Result<Vec<String>, ConversionError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(name) = component else {
            return Err(INVALID);
        };
        let name = name.to_str().ok_or(INVALID)?;
        if name.is_empty() || name.len() > 255 || name.chars().any(char::is_control) {
            return Err(INVALID);
        }
        parts.push(name.to_owned());
    }
    let joined = parts.join("/");
    if joined.is_empty() || joined.len() > MAX_PATH || parts.len() > 64 {
        return Err(INVALID);
    }
    crate::native::validate_windows_relative_path(&joined).map_err(|_| INVALID)?;
    Ok(parts)
}

pub(super) fn alias(path: &Path) -> Result<String, ConversionError> {
    Ok(relative_components(path)?
        .join("/")
        .nfc()
        .flat_map(char::to_lowercase)
        .collect())
}

/// Percent-encode a confined relative path. XML callers separately apply XML attribute escaping.
pub fn path_uri(path: &Path) -> Result<String, ConversionError> {
    Ok(pyurl::quote_from_bytes(path_text(path)?.as_bytes()))
}

/// Return a decoded relative path with portable directory separators, not a manifest URI.
pub fn path_text(path: &Path) -> Result<String, ConversionError> {
    Ok(relative_components(path)?.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_bases_keep_resource_validation_and_collision_rules() {
        for root in ["https://example.invalid/", "https://example.invalid/vod/"] {
            for suffix in [
                "%2e%2e/escape.m4s",
                "a%2fb.m4s",
                "a%5cb.m4s",
                "raw/control",
                "input.mpd",
                "video//segment.m4s",
                "video/%GG.m4s",
            ] {
                let mut mapper = UrlPathMapper::new(root, LayoutMode::Source).unwrap();
                assert!(mapper.map(&format!("{root}{suffix}"), "unused").is_err());
            }
            let mut mapper = UrlPathMapper::new(root, LayoutMode::Source).unwrap();
            let first = format!("{root}video/seg.m4s?token=first");
            assert_eq!(
                mapper.map(&first, "unused").unwrap(),
                Path::new("video/seg.m4s")
            );
            assert_eq!(
                mapper.map(&first, "unused").unwrap(),
                Path::new("video/seg.m4s")
            );
            assert_eq!(
                mapper
                    .map(&format!("{root}video/seg.m4s?token=other"), "unused")
                    .unwrap_err(),
                COLLISION
            );
            assert_eq!(
                mapper
                    .map(&format!("{root}Video/SEG.m4s"), "unused")
                    .unwrap_err(),
                COLLISION
            );
        }
        let mut mapper =
            UrlPathMapper::new("https://example.invalid/vod/", LayoutMode::Source).unwrap();
        assert_eq!(
            mapper
                .map("https://example.invalid/other/seg.m4s", "unused")
                .unwrap_err(),
            UNSUPPORTED
        );
    }

    #[test]
    fn rejected_combined_budget_does_not_claim_a_url_or_its_ancestors() {
        let mut mapper =
            UrlPathMapper::new("https://example.invalid/v/master.m3u8", LayoutMode::Source)
                .unwrap();
        let url = "https://example.invalid/v/video/segment.ts";
        assert_eq!(
            mapper
                .map_admitted(url, "unused", &mut |_| Err(ConversionError(
                    "download-graph-limit"
                )))
                .unwrap_err(),
            ConversionError("download-graph-limit")
        );
        assert_eq!(mapper.retained_bytes(), 0);
        assert!(mapper.assigned.is_empty() && mapper.claims.0.is_empty());
        mapper.map(url, "unused").unwrap();
    }

    #[test]
    fn deep_paths_charge_every_retained_ancestor_allocation() {
        let mut mapper =
            UrlPathMapper::new("https://example.invalid/v/master.m3u8", LayoutMode::Source)
                .unwrap();
        let relative = (0..64)
            .map(|_| "a".repeat(60))
            .collect::<Vec<_>>()
            .join("/");
        mapper
            .map(&format!("https://example.invalid/v/{relative}"), "unused")
            .unwrap();
        let allocations = mapper
            .claims
            .0
            .iter()
            .map(|(key, (path, _))| key.capacity() + path.capacity())
            .sum::<usize>();
        assert!(
            mapper.retained_bytes() >= allocations,
            "the charge must cover every complete ancestor key and path"
        );
    }
}
