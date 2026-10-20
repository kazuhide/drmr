//! The generated single-key `CommonSystem` record is not provider-specific DRM metadata.
use super::{CENC, COMMON, ConversionError, DASH, MISMATCH, OutputKey, TargetProtection, mp4};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use roxmltree::Node;
use zeroize::Zeroizing;

const SYSTEM: [u8; 16] = [
    0x10, 0x77, 0xef, 0xec, 0xc0, 0xb2, 0x4d, 0x02, 0xac, 0xe3, 0x3c, 0x1e, 0x52, 0xe2, 0xfb, 0x4b,
];
const URI: &str = "urn:uuid:1077efec-c0b2-4d02-ace3-3c1e52e2fb4b";

pub(super) fn key(
    target: TargetProtection,
    key: Option<&OutputKey>,
) -> Result<&OutputKey, ConversionError> {
    if matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs) {
        key.ok_or(MISMATCH)
    } else {
        Err(MISMATCH)
    }
}

// W3C cenc initialization data, section 4.1: version 1, common SystemID, and no data.
// https://www.w3.org/TR/eme-initdata-cenc/
// Generated outputs use exactly the one operator-selected output KID.
pub(super) fn payload(bytes: &[u8], key: &OutputKey) -> Result<(), ConversionError> {
    if bytes.len() != 44
        || bytes[..4] != [1, 0, 0, 0]
        || bytes[4..20] != SYSTEM
        || bytes[20..24] != [0, 0, 0, 1]
        || bytes[24..40] != key.0.id
        || bytes[40..44] != [0; 4]
    {
        return Err(MISMATCH);
    }
    Ok(())
}

pub(super) fn dash(
    text: &str,
    target: TargetProtection,
    expected: Option<&OutputKey>,
) -> Result<(), ConversionError> {
    let document = roxmltree::Document::parse(text).map_err(|_| MISMATCH)?;
    for node in document.descendants().filter(Node::is_element) {
        if node.tag_name().name() == "pro" {
            return Err(MISMATCH);
        }
        if node.tag_name().name() == "pssh"
            && (!node.has_tag_name((CENC, "pssh"))
                || !node.parent().is_some_and(|parent| {
                    parent.has_tag_name((DASH, "ContentProtection"))
                        && parent.attribute("schemeIdUri") == Some(URI)
                }))
        {
            return Err(MISMATCH);
        }
        if node.tag_name().name() != "ContentProtection" {
            continue;
        }
        if !node.has_tag_name((DASH, "ContentProtection"))
            || !node.parent().is_some_and(|parent| {
                parent.has_tag_name((DASH, "AdaptationSet"))
                    || parent.has_tag_name((DASH, "Representation"))
            })
        {
            return Err(MISMATCH);
        }
        let key = key(target, expected)?;
        match node.attribute("schemeIdUri") {
            Some(COMMON) => scheme(node, target, key)?,
            Some(URI) => record(node, target, key)?,
            _ => return Err(MISMATCH),
        }
    }
    Ok(())
}

fn scheme(
    node: Node<'_, '_>,
    target: TargetProtection,
    key: &OutputKey,
) -> Result<(), ConversionError> {
    if node.attribute("value") != Some(target.as_str())
        || node.attribute((CENC, "default_KID")).and_then(uuid) != Some(key.0.id)
        || node.children().any(|node| {
            node.is_element()
                || (node.is_text() && node.text().is_some_and(|text| !text.trim().is_empty()))
        })
        || node.attributes().any(|attribute| {
            !matches!(
                (attribute.namespace(), attribute.name()),
                (None, "value" | "schemeIdUri") | (Some(CENC), "default_KID")
            )
        })
        || siblings(node, COMMON) != 1
    {
        return Err(MISMATCH);
    }
    Ok(())
}

fn record(
    node: Node<'_, '_>,
    target: TargetProtection,
    key: &OutputKey,
) -> Result<(), ConversionError> {
    if node
        .attributes()
        .any(|attribute| attribute.namespace().is_some() || attribute.name() != "schemeIdUri")
        || node
            .children()
            .any(|node| node.is_text() && node.text().is_some_and(|text| !text.trim().is_empty()))
        || siblings(node, URI) != 1
    {
        return Err(MISMATCH);
    }
    let parent = node.parent().ok_or(MISMATCH)?;
    let scheme_node = parent
        .children()
        .find(|child| {
            child.has_tag_name((DASH, "ContentProtection"))
                && child.attribute("schemeIdUri") == Some(COMMON)
        })
        .ok_or(MISMATCH)?;
    scheme(scheme_node, target, key)?;
    let children: Vec<_> = node.children().filter(Node::is_element).collect();
    if children.len() != 1
        || !children[0].has_tag_name((CENC, "pssh"))
        || children[0].attributes().len() != 0
        || children[0].children().any(|node| node.is_element())
    {
        return Err(MISMATCH);
    }
    let text = Zeroizing::new(
        children[0]
            .children()
            .filter(Node::is_text)
            .filter_map(|node| node.text())
            .collect::<String>(),
    );
    if text.is_empty() || text.len() > 65_536 {
        return Err(MISMATCH);
    }
    let encoded = Zeroizing::new(
        text.chars()
            .filter(|value| !value.is_ascii_whitespace())
            .collect::<String>(),
    );
    let bytes = Zeroizing::new(STANDARD.decode(encoded.as_bytes()).map_err(|_| MISMATCH)?);
    let boxes = mp4::parse_boxes(&bytes).map_err(|_| MISMATCH)?;
    if boxes.len() != 1 || boxes[0].kind() != *b"pssh" || boxes[0].header.size != bytes.len() {
        return Err(MISMATCH);
    }
    payload(boxes[0].header.payload(&bytes), key)
}

#[cfg(test)]
mod tests;
fn siblings(node: Node<'_, '_>, uri: &str) -> usize {
    node.parent().map_or(0, |parent| {
        parent
            .children()
            .filter(|child| {
                child.has_tag_name((DASH, "ContentProtection"))
                    && child.attribute("schemeIdUri") == Some(uri)
            })
            .count()
    })
}
fn uuid(value: &str) -> Option<[u8; 16]> {
    if value.len() != 36
        || value.bytes().enumerate().any(|(index, value)| {
            if [8, 13, 18, 23].contains(&index) {
                value != b'-'
            } else {
                !value.is_ascii_hexdigit()
            }
        })
    {
        return None;
    }
    hex::decode(value.replace('-', "")).ok()?.try_into().ok()
}
