use super::syntax::{self, Attribute, Line, TextError};
use crate::{Limits, ManifestReference, ReferenceError};

pub(super) fn collect<'a>(
    bytes: &'a [u8],
    limits: &Limits,
) -> Result<Vec<ManifestReference<'a>>, ReferenceError> {
    let lines = syntax::lines(bytes, limits).map_err(|error| match error {
        TextError::Limit(_) => ReferenceError::ResourceLimit,
        _ => ReferenceError::InvalidSyntax,
    })?;
    from_lines(&lines, limits)
}

pub(super) fn from_lines<'a>(
    lines: &[Line<'a>],
    limits: &Limits,
) -> Result<Vec<ManifestReference<'a>>, ReferenceError> {
    let mut references = Vec::new();
    let mut pending = false;
    for &line in lines {
        let (name, value) = line.tag();
        if line.text.is_empty() || line.text.starts_with('#') && !line.text.starts_with("#EXT") {
            continue;
        }
        match name {
            "#EXT-X-DEFINE" | "#EXT-X-CONTENT-STEERING" | "#EXT-X-RENDITION-REPORT" => {
                return Err(ReferenceError::UnsupportedFeature);
            }
            "#EXT-X-STREAM-INF" => {
                if pending {
                    return Err(ReferenceError::InvalidSyntax);
                }
                syntax::attributes(
                    value.ok_or(ReferenceError::InvalidSyntax)?,
                    line.value_start(),
                )
                .map_err(|_| ReferenceError::InvalidSyntax)?;
                pending = true;
            }
            "#EXT-X-MEDIA" => {
                if pending {
                    return Err(ReferenceError::InvalidSyntax);
                }
                let attributes = syntax::attributes(
                    value.ok_or(ReferenceError::InvalidSyntax)?,
                    line.value_start(),
                )
                .map_err(|_| ReferenceError::InvalidSyntax)?;
                let kind = syntax::get(&attributes, "TYPE").ok_or(ReferenceError::InvalidSyntax)?;
                if kind.quoted {
                    return Err(ReferenceError::InvalidSyntax);
                }
                let uri = syntax::get(&attributes, "URI");
                match kind.value {
                    "AUDIO" | "VIDEO" => {
                        if let Some(uri) = uri {
                            add_attribute(&mut references, uri, limits)?;
                        }
                    }
                    "SUBTITLES" => add_attribute(
                        &mut references,
                        uri.ok_or(ReferenceError::InvalidSyntax)?,
                        limits,
                    )?,
                    "CLOSED-CAPTIONS" if uri.is_none() => {}
                    "CLOSED-CAPTIONS" => return Err(ReferenceError::InvalidSyntax),
                    _ => return Err(ReferenceError::UnsupportedFeature),
                }
            }
            "#EXT-X-I-FRAME-STREAM-INF" => {
                if pending {
                    return Err(ReferenceError::InvalidSyntax);
                }
                let attributes = syntax::attributes(
                    value.ok_or(ReferenceError::InvalidSyntax)?,
                    line.value_start(),
                )
                .map_err(|_| ReferenceError::InvalidSyntax)?;
                add_attribute(
                    &mut references,
                    syntax::get(&attributes, "URI").ok_or(ReferenceError::InvalidSyntax)?,
                    limits,
                )?;
            }
            _ if !line.text.starts_with('#') && pending => {
                add(&mut references, line.text, line.span, limits)?;
                pending = false;
            }
            _ if pending && is_media_tag(name) => return Err(ReferenceError::InvalidSyntax),
            _ => {}
        }
    }
    if pending {
        return Err(ReferenceError::InvalidSyntax);
    }
    Ok(references)
}

pub(super) fn is_master_tag(name: &str) -> bool {
    matches!(
        name,
        "#EXT-X-MEDIA"
            | "#EXT-X-STREAM-INF"
            | "#EXT-X-I-FRAME-STREAM-INF"
            | "#EXT-X-SESSION-DATA"
            | "#EXT-X-SESSION-KEY"
    )
}

pub(super) fn is_media_tag(name: &str) -> bool {
    matches!(
        name,
        "#EXTINF"
            | "#EXT-X-TARGETDURATION"
            | "#EXT-X-MEDIA-SEQUENCE"
            | "#EXT-X-DISCONTINUITY-SEQUENCE"
            | "#EXT-X-KEY"
            | "#EXT-X-MAP"
            | "#EXT-X-ENDLIST"
            | "#EXT-X-PLAYLIST-TYPE"
            | "#EXT-X-I-FRAMES-ONLY"
            | "#EXT-X-DISCONTINUITY"
            | "#EXT-X-BYTERANGE"
            | "#EXT-X-PROGRAM-DATE-TIME"
            | "#EXT-X-DATERANGE"
    )
}

fn add_attribute<'a>(
    references: &mut Vec<ManifestReference<'a>>,
    attribute: Attribute<'a>,
    limits: &Limits,
) -> Result<(), ReferenceError> {
    if !attribute.quoted {
        return Err(ReferenceError::InvalidSyntax);
    }
    add(references, attribute.value, attribute.span, limits)
}

fn add<'a>(
    references: &mut Vec<ManifestReference<'a>>,
    uri: &'a str,
    span: crate::Span,
    limits: &Limits,
) -> Result<(), ReferenceError> {
    if references.len() >= limits.max_references {
        return Err(ReferenceError::ResourceLimit);
    }
    if uri.contains("{$") {
        return Err(ReferenceError::UnsupportedFeature);
    }
    if !syntax::uri(uri) {
        return Err(ReferenceError::InvalidSyntax);
    }
    references.push(ManifestReference { uri, span });
    Ok(())
}
