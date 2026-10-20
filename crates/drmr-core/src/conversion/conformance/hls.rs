use super::ConversionError;
use crate::m3u8::parse_attributes;
use std::{
    collections::BTreeSet,
    path::{Component, Path, PathBuf},
};

pub(super) struct Manifest {
    pub original: PathBuf,
    pub relative: PathBuf,
    pub bytes: Vec<u8>,
    pub version: Option<u32>,
    pub minimum: u32,
    pub media: bool,
    pub segments: usize,
}

pub(super) fn capture(entrypoint: &Path) -> Result<Vec<Manifest>, ConversionError> {
    if std::fs::symlink_metadata(entrypoint)
        .map_err(|_| invalid())?
        .file_type()
        .is_symlink()
    {
        return Err(invalid());
    }
    let entrypoint = std::fs::canonicalize(entrypoint).map_err(|_| invalid())?;
    let root = entrypoint.parent().ok_or_else(invalid)?;
    let first = entrypoint
        .strip_prefix(root)
        .map_err(|_| invalid())?
        .to_path_buf();
    let mut seen = BTreeSet::new();
    let mut pending = vec![first];
    let mut result = Vec::new();
    let mut total = 0_usize;
    while let Some(relative) = pending.pop() {
        if !seen.insert(relative.clone()) {
            continue;
        }
        if seen.len() > 1024 {
            return Err(ConversionError("manifest-validation-graph-limit"));
        }
        let path = root.join(&relative);
        let mut ancestor = root.to_path_buf();
        for component in relative.components() {
            ancestor.push(component);
            if std::fs::symlink_metadata(&ancestor)
                .map_err(|_| invalid())?
                .file_type()
                .is_symlink()
            {
                return Err(invalid());
            }
        }
        let bytes = super::files::read_manifest(&path)?;
        total = total.checked_add(bytes.len()).ok_or_else(invalid)?;
        if total > 64 * 1024 * 1024 {
            return Err(ConversionError("manifest-validation-graph-limit"));
        }
        let text = std::str::from_utf8(&bytes).map_err(|_| invalid())?;
        let parsed = parse(text)?;
        for child in parsed.children {
            pending.push(resolve(&relative, &child)?);
        }
        result.push(Manifest {
            original: path,
            relative,
            bytes,
            version: parsed.version,
            minimum: parsed.minimum,
            media: parsed.media,
            segments: parsed.segments,
        });
    }
    Ok(result)
}

#[derive(Debug)]
pub(super) struct Parsed {
    pub version: Option<u32>,
    pub minimum: u32,
    pub media: bool,
    pub segments: usize,
    pub children: Vec<String>,
}

pub(super) fn parse(text: &str) -> Result<Parsed, ConversionError> {
    if text.lines().next() != Some("#EXTM3U") || text.contains('\0') {
        return Err(invalid());
    }
    let mut version = None;
    let mut minimum = 1;
    let mut map = false;
    let mut iframe = false;
    let mut media = false;
    let mut segments = 0;
    let mut children = Vec::new();
    let mut next_variant = false;
    for line in text.lines().skip(1) {
        admit_tag(line)?;
        if let Some(value) = line.strip_prefix("#EXT-X-VERSION:") {
            let number = value.parse::<u32>().map_err(|_| invalid())?;
            if number == 0 || version.replace(number).is_some() {
                return Err(invalid());
            }
        } else if line == "#EXT-X-I-FRAMES-ONLY" {
            minimum = minimum.max(4);
            iframe = true;
        } else if line.starts_with("#EXT-X-BYTERANGE:") {
            minimum = minimum.max(4);
        } else if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
            let attrs = parse_attributes(value).map_err(|_| invalid())?;
            local_reference(attrs.get("URI").ok_or_else(invalid)?)?;
            map = true;
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            minimum = minimum.max(duration_minimum(value)?);
            media = true;
        } else if let Some(value) = line
            .strip_prefix("#EXT-X-KEY:")
            .or_else(|| line.strip_prefix("#EXT-X-SESSION-KEY:"))
        {
            let attrs = parse_attributes(value).map_err(|_| invalid())?;
            if attrs.get("IV").is_some() {
                minimum = minimum.max(2);
            }
            if attrs.get("KEYFORMAT").is_some() || attrs.get("KEYFORMATVERSIONS").is_some() {
                minimum = minimum.max(5);
            }
        } else if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = parse_attributes(value).map_err(|_| invalid())?;
            if attrs
                .get("BANDWIDTH")
                .and_then(|v| v.parse::<u64>().ok())
                .is_none_or(|v| v == 0)
            {
                return Err(invalid());
            }
            if next_variant {
                return Err(invalid());
            }
            next_variant = true;
        } else if let Some(value) = line
            .strip_prefix("#EXT-X-MEDIA:")
            .or_else(|| line.strip_prefix("#EXT-X-I-FRAME-STREAM-INF:"))
        {
            let attrs = parse_attributes(value).map_err(|_| invalid())?;
            if attrs
                .get("INSTREAM-ID")
                .is_some_and(|value| value.starts_with("SERVICE"))
            {
                minimum = minimum.max(7);
            }
            if let Some(uri) = attrs.get("URI") {
                local_reference(uri)?;
                children.push(uri.to_owned());
            }
        } else if line.starts_with("#EXT-X-SESSION-DATA:")
            || line.starts_with("#EXT-X-CONTENT-STEERING:")
            || line.starts_with("#EXT-X-DEFINE:")
        {
            // These can introduce further fetches or variable expansion. Generated
            // output profiles do not emit them; they require a separate admission.
            return Err(ConversionError("manifest-validation-feature-unsupported"));
        } else if !line.is_empty() && !line.starts_with('#') {
            local_reference(line)?;
            if next_variant {
                children.push(line.to_owned());
                next_variant = false;
            } else {
                segments += 1;
            }
        }
    }
    if map {
        minimum = minimum.max(if iframe { 5 } else { 6 });
    }
    if next_variant || (media && !children.is_empty()) || (segments == 0 && children.is_empty()) {
        return Err(invalid());
    }
    if version.unwrap_or(1) < minimum {
        return Err(ConversionError("hls-protocol-version-too-low"));
    }
    Ok(Parsed {
        version,
        minimum,
        media,
        segments,
        children,
    })
}

fn admit_tag(line: &str) -> Result<(), ConversionError> {
    const TAGS: &[&str] = &[
        "#EXT-X-VERSION",
        "#EXT-X-INDEPENDENT-SEGMENTS",
        "#EXT-X-MEDIA",
        "#EXT-X-STREAM-INF",
        "#EXT-X-I-FRAME-STREAM-INF",
        "#EXT-X-SESSION-KEY",
        "#EXT-X-TARGETDURATION",
        "#EXT-X-MEDIA-SEQUENCE",
        "#EXT-X-PLAYLIST-TYPE",
        "#EXT-X-KEY",
        "#EXTINF",
        "#EXT-X-BYTERANGE",
        "#EXT-X-MAP",
        "#EXT-X-DISCONTINUITY",
        "#EXT-X-DISCONTINUITY-SEQUENCE",
        "#EXT-X-PROGRAM-DATE-TIME",
        "#EXT-X-ENDLIST",
        "#EXT-X-I-FRAMES-ONLY",
    ];
    let tag = line.split_once(':').map_or(line, |(name, _)| name);
    if line.starts_with("#EXT") && !TAGS.contains(&tag) {
        return Err(ConversionError("manifest-validation-feature-unsupported"));
    }
    Ok(())
}

fn duration_minimum(value: &str) -> Result<u32, ConversionError> {
    let (duration, _) = value.split_once(',').ok_or_else(invalid)?;
    let minimum = if duration.contains('.') { 3 } else { 1 };
    let duration = duration.parse::<f64>().map_err(|_| invalid())?;
    if !duration.is_finite() || duration < 0.0 {
        return Err(invalid());
    }
    Ok(minimum)
}

fn local_reference(value: &str) -> Result<(), ConversionError> {
    if value.is_empty()
        || value.contains([':', '?', '#', '%', '\\'])
        || value.chars().any(char::is_control)
        || value.starts_with('/')
    {
        return Err(ConversionError("manifest-validation-reference-unsupported"));
    }
    Ok(())
}

fn resolve(parent: &Path, reference: &str) -> Result<PathBuf, ConversionError> {
    local_reference(reference)?;
    let mut result = parent.parent().unwrap_or(Path::new("")).to_path_buf();
    for component in Path::new(reference).components() {
        match component {
            Component::Normal(value) => result.push(value),
            Component::CurDir => {}
            Component::ParentDir if result.pop() => {}
            _ => return Err(invalid()),
        }
    }
    if result.as_os_str().is_empty() {
        return Err(invalid());
    }
    Ok(result)
}

fn invalid() -> ConversionError {
    ConversionError("manifest-validation-hls-invalid")
}
