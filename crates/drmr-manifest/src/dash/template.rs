/// Validate substitution tokens without expanding URLs or executing formatting.
use super::numeric::Parsed;

pub(super) fn validate(value: &str) -> Parsed<()> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Parsed::Invalid;
    }
    let mut unsupported = false;
    let mut remaining = value;
    while let Some(start) = remaining.find('$') {
        remaining = &remaining[start + 1..];
        if let Some(rest) = remaining.strip_prefix('$') {
            remaining = rest;
            continue;
        }
        let Some(end) = remaining.find('$') else {
            return Parsed::Invalid;
        };
        let token = &remaining[..end];
        remaining = &remaining[end + 1..];
        let (name, format) = token
            .split_once('%')
            .map_or((token, None), |(name, format)| (name, Some(format)));
        if name == "SubNumber" {
            unsupported = true;
        } else if !matches!(name, "RepresentationID" | "Number" | "Bandwidth" | "Time") {
            return Parsed::Invalid;
        }
        if let Some(format) = format {
            if name == "RepresentationID" || format.is_empty() {
                return Parsed::Invalid;
            }
            if !format.as_bytes().last().is_some_and(u8::is_ascii) {
                return Parsed::Invalid;
            }
            let (width, conversion) = format.split_at(format.len() - 1);
            if !matches!(conversion, "d" | "i" | "o" | "u" | "x" | "X")
                || !width.bytes().all(|byte| byte.is_ascii_digit())
            {
                return Parsed::Invalid;
            }
        }
    }
    if unsupported {
        Parsed::Unsupported
    } else {
        Parsed::Value(())
    }
}
