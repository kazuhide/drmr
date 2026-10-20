use crate::Limits;
use std::collections::BTreeSet;

/// Only UTF-8 (and its ASCII subset) is decoded by the borrowed-input parser.
/// Recognized other encodings remain unresolved, not invalid UTF-8 documents.
pub(super) fn unsupported_encoding(bytes: &[u8]) -> bool {
    if [
        b"\xff\xfe".as_slice(),
        b"\xfe\xff".as_slice(),
        b"\0\0\xfe\xff".as_slice(),
    ]
    .iter()
    .any(|prefix| bytes.starts_with(prefix))
    {
        return true;
    }
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    if !bytes.starts_with(b"<?xml") || bytes.get(5).is_none_or(|byte| !space(*byte)) {
        return false;
    }
    let Some(end) = bytes.windows(2).position(|value| value == b"?>") else {
        return false;
    };
    let Ok(declaration) = std::str::from_utf8(&bytes[5..end]) else {
        return false;
    };
    let Some((_, value)) = declaration.split_once("encoding") else {
        return false;
    };
    let Some(value) = value
        .trim_start_matches([' ', '\t', '\r', '\n'])
        .strip_prefix('=')
    else {
        return false;
    };
    let value = value.trim_start_matches([' ', '\t', '\r', '\n']);
    let Some(quote @ (b'\'' | b'"')) = value.as_bytes().first().copied() else {
        return false;
    };
    let Some(end) = value[1..].find(char::from(quote)) else {
        return false;
    };
    let encoding = &value[1..=end];
    if encoding.eq_ignore_ascii_case("us-ascii") {
        !bytes.is_ascii()
    } else {
        !encoding.eq_ignore_ascii_case("utf-8")
    }
}

/// A lexical admission scan, not an XML validator. Invalid syntax remains the
/// DOM parser's responsibility. Quotes, comments, CDATA and PIs are skipped
/// without interpreting or expanding entities.
pub(super) fn within_limits(text: &str, limits: &Limits) -> bool {
    let bytes = text.as_bytes();
    let mut position = 0;
    let mut depth = 1_usize;
    let mut namespaces = BTreeSet::new();
    while position < bytes.len() {
        let Some(offset) = bytes[position..].iter().position(|byte| *byte == b'<') else {
            break;
        };
        position += offset;
        let remaining = &bytes[position..];
        let marker = special_marker(remaining);
        if let Some((prefix, terminator)) = marker {
            let start = position + prefix;
            let Some(offset) = bytes[start..]
                .windows(terminator.len())
                .position(|value| value == terminator)
            else {
                return true;
            };
            position = start + offset + terminator.len();
            continue;
        }
        if remaining.starts_with(b"</") || remaining.starts_with(b"<!") {
            if remaining.starts_with(b"</") && depth > 1 {
                depth -= 1;
            }
            let Some(offset) = remaining.iter().position(|byte| *byte == b'>') else {
                return true;
            };
            position += offset + 1;
            continue;
        }
        position += 1;
        while position < bytes.len() && !space(bytes[position]) && !b"/>".contains(&bytes[position])
        {
            position += 1;
        }
        if depth > limits.max_depth {
            return false;
        }
        let mut attributes = 0;
        loop {
            skip_space(bytes, &mut position);
            if bytes.get(position) == Some(&b'>') {
                position += 1;
                depth += 1;
                break;
            }
            if bytes.get(position..position + 2) == Some(b"/>") {
                position += 2;
                break;
            }
            if position >= bytes.len() {
                return true;
            }
            let start = position;
            while position < bytes.len()
                && !space(bytes[position])
                && !b"=/>".contains(&bytes[position])
            {
                position += 1;
            }
            if position == start {
                return true;
            }
            attributes += 1;
            if attributes > limits.max_xml_attributes_per_element {
                return false;
            }
            let name = &bytes[start..position];
            skip_space(bytes, &mut position);
            if bytes.get(position) != Some(&b'=') {
                return true;
            }
            position += 1;
            skip_space(bytes, &mut position);
            let Some(quote @ (b'\'' | b'"')) = bytes.get(position).copied() else {
                return true;
            };
            position += 1;
            let Some(offset) = bytes[position..].iter().position(|byte| *byte == quote) else {
                return true;
            };
            if name == b"xmlns" || name.starts_with(b"xmlns:") {
                namespaces.insert((name, &bytes[position..position + offset]));
                if namespaces.len() > limits.max_xml_namespace_bindings {
                    return false;
                }
            }
            position += offset + 1;
        }
    }
    true
}
fn space(byte: u8) -> bool {
    b" \t\r\n".contains(&byte)
}
fn skip_space(bytes: &[u8], position: &mut usize) {
    while bytes.get(*position).is_some_and(|byte| space(*byte)) {
        *position += 1;
    }
}

fn special_marker(remaining: &[u8]) -> Option<(usize, &'static [u8])> {
    if remaining.starts_with(b"<!--") {
        Some((4, b"-->".as_slice()))
    } else if remaining.starts_with(b"<![CDATA[") {
        Some((9, b"]]>".as_slice()))
    } else if remaining.starts_with(b"<?") {
        Some((2, b"?>".as_slice()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_limits_ignore_attribute_text_inside_non_tag_regions() {
        let limits = Limits {
            max_xml_attributes_per_element: 1,
            max_depth: 2,
            ..Limits::default()
        };
        for text in [
            "<a x='a=b > <fake a=1 b=2'/>",
            "<!-- <a x='1' y='2'> --><a/>",
            "<a><![CDATA[<x a='1' b='2'>]]></a>",
            "<?pi a='1' b='2'?><a/>",
            "<a x='1'><b/></a>",
        ] {
            assert!(within_limits(text, &limits), "{text}");
        }
        assert!(!within_limits("<a x='1' y='2'/>", &limits));
        assert!(!within_limits("<a><b><c/></b></a>", &limits));
        let limits = Limits {
            max_xml_attributes_per_element: 4,
            max_xml_namespace_bindings: 1,
            ..limits
        };
        assert!(within_limits("<a xmlns:a='a'><b/></a>", &limits));
        assert!(!within_limits(
            "<a xmlns:a='a'><b xmlns:b='b'/></a>",
            &limits
        ));
        let repeated = format!("<a>{}</a>", "<b xmlns:p='urn:x'/>".repeat(512));
        assert!(within_limits(&repeated, &limits));
        let limits = Limits {
            max_depth: 8,
            max_xml_namespace_bindings: 2,
            ..limits
        };
        assert!(within_limits(
            "<a xmlns:p='a'><b xmlns:p='b'><c xmlns:p='a'/></b></a>",
            &limits
        ));
        assert!(!within_limits(
            "<a xmlns:p='a'><b xmlns:p='b'><c xmlns:p='c'/></b></a>",
            &limits
        ));
    }
}
