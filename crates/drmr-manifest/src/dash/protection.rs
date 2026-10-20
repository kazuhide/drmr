use super::{NS, check, not_checked, span};
use crate::{DiagnosticSink, NotCheckedReason};
use base64::{Engine, engine::general_purpose::STANDARD};
use roxmltree::Node;
use std::borrow::Cow;

const CENC: &str = "urn:mpeg:cenc:2013";
const PLAYREADY: &str = "urn:microsoft:playready";

pub(super) fn descriptor(node: Node<'_, '_>, sink: &mut DiagnosticSink<'_>) {
    if let Some(value) = node.attribute((CENC, "default_KID")) {
        let mut values = value
            .split([' ', '\t', '\r', '\n'])
            .filter(|value| !value.is_empty())
            .peekable();
        let valid = values.peek().is_some() && values.all(uuid);
        check(sink, "DASH-CENC-DEFAULT-KID", valid, span(node));
    }
}

pub(super) fn extension(node: Node<'_, '_>, sink: &mut DiagnosticSink<'_>) -> bool {
    let pssh = node.has_tag_name((CENC, "pssh"));
    if !pssh && !node.has_tag_name((PLAYREADY, "pro")) {
        return false;
    }
    let location = span(node);
    if node.attributes().next().is_some() {
        not_checked(
            sink,
            "DASH-ATTRIBUTE-COVERAGE",
            NotCheckedReason::UnsupportedFeature,
            location,
        );
    }
    check(
        sink,
        "DASH-PROTECTION-PLACEMENT",
        node.parent()
            .is_some_and(|parent| parent.has_tag_name((NS, "ContentProtection"))),
        location,
    );
    let mut value = String::new();
    for child in node.children() {
        if child.is_text() {
            value.push_str(child.text().unwrap_or(""));
        }
        if child.is_element() {
            check(sink, "DASH-PROTECTION-BASE64", false, span(child));
        }
    }
    let compact = if value.bytes().any(|byte| b" \t\r\n".contains(&byte)) {
        Cow::Owned(
            value
                .chars()
                .filter(|character| !matches!(character, ' ' | '\t' | '\r' | '\n'))
                .collect::<String>(),
        )
    } else {
        Cow::Borrowed(value.as_str())
    };
    match STANDARD.decode(compact.as_bytes()) {
        Ok(data) => {
            check(sink, "DASH-PROTECTION-BASE64", true, location);
            if pssh {
                match pssh_boxes(&data) {
                    Some(true) => check(sink, "DASH-CENC-PSSH-BOX", true, location),
                    Some(false) => check(sink, "DASH-CENC-PSSH-BOX", false, location),
                    None => not_checked(
                        sink,
                        "DASH-CENC-PSSH-BOX",
                        NotCheckedReason::UnsupportedFeature,
                        location,
                    ),
                }
            }
        }
        Err(_) => check(sink, "DASH-PROTECTION-BASE64", false, location),
    }
    let mut payload_rule = super::rule("DASH-DRM-PAYLOAD");
    payload_rule.scope = crate::RuleScope::OutsideScope;
    sink.check(
        payload_rule,
        crate::CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature),
        Some(location),
    );
    true
}

fn uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn integer(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(
        bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// Validate box framing and declared lengths, retaining DRM payloads as opaque.
/// Future PSSH versions are unresolved rather than rejected as malformed version 0.
fn pssh_boxes(mut bytes: &[u8]) -> Option<bool> {
    if bytes.is_empty() {
        return Some(false);
    }
    let mut supported = true;
    while !bytes.is_empty() {
        let Some(size) = integer(bytes, 0) else {
            return Some(false);
        };
        let (size, header) = match size {
            0 => (bytes.len(), 8),
            1 => {
                let Some(raw) = bytes
                    .get(8..16)
                    .and_then(|raw| <[u8; 8]>::try_from(raw).ok())
                else {
                    return Some(false);
                };
                let Ok(size) = usize::try_from(u64::from_be_bytes(raw)) else {
                    return Some(false);
                };
                (size, 16)
            }
            _ => (size as usize, 8),
        };
        let Some(data) = bytes.get(..size) else {
            return Some(false);
        };
        if data.get(4..8) != Some(b"pssh") || data.len() < header + 24 {
            return Some(false);
        }
        let version = data[header];
        if data[header + 1..header + 4] != [0, 0, 0] {
            return Some(false);
        }
        let payload = match version {
            0 => header + 20,
            1 => {
                let Some(count) = integer(data, header + 20) else {
                    return Some(false);
                };
                let Some(offset) = (count as usize)
                    .checked_mul(16)
                    .and_then(|size| size.checked_add(header + 24))
                else {
                    return Some(false);
                };
                offset
            }
            _ => {
                supported = false;
                bytes = &bytes[size..];
                continue;
            }
        };
        let Some(length) = integer(data, payload) else {
            return Some(false);
        };
        if payload
            .checked_add(4)
            .and_then(|offset| offset.checked_add(length as usize))
            != Some(size)
        {
            return Some(false);
        }
        bytes = &bytes[size..];
    }
    supported.then_some(true)
}
