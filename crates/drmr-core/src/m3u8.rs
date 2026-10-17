//! HLS attribute-list syntax shared by source readers and manifest rewriting.

/// A source-side failure, carrying the Python `SourceError` token (e.g. `invalid-attributes`).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct SourceError(pub String);

impl SourceError {
    /// A `SourceError` with this token.
    pub fn new(token: &str) -> Self {
        SourceError(token.to_string())
    }

    /// The token.
    pub fn token(&self) -> &str {
        &self.0
    }
}

/// An RFC 8216 attribute list in document order, every key unique.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Attributes(Vec<(String, String)>);

impl Attributes {
    /// The value of `key`, as written (quotes removed).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// The pairs in document order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
    }

    /// Number of attributes.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// `source::parse_attributes`: an RFC 8216 attribute list, commas allowed inside quoted
/// strings; keys are `[A-Z0-9-]+`; an empty value, a duplicate key, an unterminated quote, a
/// trailing comma or a line break inside a value is `invalid-attributes`.
pub fn parse_attributes(value: &str) -> Result<Attributes, SourceError> {
    let bytes = value.as_bytes();
    let invalid = || SourceError::new("invalid-attributes");
    let mut result = Attributes::default();
    let mut pos = 0;
    while pos < bytes.len() {
        let start = pos;
        while pos < bytes.len()
            && (bytes[pos].is_ascii_uppercase()
                || bytes[pos].is_ascii_digit()
                || bytes[pos] == b'-')
        {
            pos += 1;
        }
        if pos == start || pos >= bytes.len() || bytes[pos] != b'=' {
            return Err(invalid());
        }
        let key = &value[start..pos];
        pos += 1;
        if result.get(key).is_some() {
            return Err(invalid());
        }
        let item = if pos < bytes.len() && bytes[pos] == b'"' {
            let Some(offset) = bytes[pos + 1..].iter().position(|byte| *byte == b'"') else {
                return Err(invalid());
            };
            let end = pos + 1 + offset;
            let item = &value[pos + 1..end];
            pos = end + 1;
            item
        } else {
            let end = bytes[pos..]
                .iter()
                .position(|byte| *byte == b',')
                .map_or(bytes.len(), |offset| pos + offset);
            let item = &value[pos..end];
            pos = end;
            item
        };
        if item.is_empty() || item.contains('\r') || item.contains('\n') {
            return Err(invalid());
        }
        result.0.push((key.to_string(), item.to_string()));
        if pos < bytes.len() {
            if bytes[pos] != b',' || pos + 1 == bytes.len() {
                return Err(invalid());
            }
            pos += 1;
        }
    }
    Ok(result)
}
