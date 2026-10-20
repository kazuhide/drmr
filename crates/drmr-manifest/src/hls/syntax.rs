use crate::{Limits, Span};
use unicode_normalization::is_nfc;

pub(super) enum TextError {
    Encoding(Span),
    Layout(Span),
    Normalization(Span),
    Header(Span),
    Limit(Span),
}

pub(super) fn lines<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Vec<Line<'a>>, TextError> {
    let whole = Span {
        start: 0,
        end: bytes.len(),
    };
    if bytes.len() > limits.max_document_bytes {
        return Err(TextError::Limit(whole));
    }
    let text = std::str::from_utf8(bytes).map_err(|error| {
        TextError::Encoding(Span {
            start: error.valid_up_to(),
            end: (error.valid_up_to() + 1).min(bytes.len()),
        })
    })?;
    if text.starts_with('\u{feff}') {
        return Err(TextError::Layout(Span { start: 0, end: 3 }));
    }
    if let Some((at, value)) = text
        .char_indices()
        .find(|(_, value)| value.is_control() && !matches!(value, '\n' | '\r'))
    {
        return Err(TextError::Layout(Span {
            start: at,
            end: at + value.len_utf8(),
        }));
    }
    let mut result = Vec::new();
    let mut offset = 0;
    for raw in text.split_inclusive('\n') {
        let body = raw.strip_suffix('\n').unwrap_or(raw);
        let body = if raw.ends_with('\n') {
            body.strip_suffix('\r').unwrap_or(body)
        } else {
            body
        };
        let span = Span {
            start: offset,
            end: offset + body.len(),
        };
        if result.len() >= limits.max_lines || body.len() > limits.max_line_bytes {
            return Err(TextError::Limit(span));
        }
        if body.contains('\r') {
            return Err(TextError::Layout(span));
        }
        result.push(Line { text: body, span });
        offset += raw.len();
    }
    if result.first().is_none_or(|line| line.text != "#EXTM3U") {
        return Err(TextError::Header(
            result.first().map_or(whole, |line| line.span),
        ));
    }
    if !is_nfc(text) {
        return Err(TextError::Normalization(whole));
    }
    Ok(result)
}

#[derive(Clone, Copy)]
pub(super) struct Line<'a> {
    pub text: &'a str,
    pub span: Span,
}

impl<'a> Line<'a> {
    pub fn tag(self) -> (&'a str, Option<&'a str>) {
        self.text
            .split_once(':')
            .map_or((self.text, None), |(name, value)| (name, Some(value)))
    }
    pub fn value_start(self) -> usize {
        self.span.start + self.text.find(':').map_or(self.text.len(), |at| at + 1)
    }
}

#[derive(Clone, Copy)]
pub(super) struct Attribute<'a> {
    pub name: &'a str,
    pub value: &'a str,
    pub quoted: bool,
    pub span: Span,
}

pub(super) enum AttributeError {
    Syntax(Span),
    Duplicate(Span),
}

pub(super) fn attributes(value: &str, offset: usize) -> Result<Vec<Attribute<'_>>, AttributeError> {
    let bytes = value.as_bytes();
    let mut result = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    let mut at = 0;
    let invalid = |start: usize, end: usize| {
        AttributeError::Syntax(Span {
            start: offset + start,
            end: offset + end,
        })
    };
    if value.is_empty() {
        return Err(invalid(0, 0));
    }
    while at < bytes.len() {
        let start = at;
        while bytes
            .get(at)
            .is_some_and(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || *byte == b'-')
        {
            at += 1;
        }
        if at == start || bytes.get(at) != Some(&b'=') {
            return Err(invalid(start, at));
        }
        let name = &value[start..at];
        if !names.insert(name) {
            return Err(AttributeError::Duplicate(Span {
                start: offset + start,
                end: offset + at,
            }));
        }
        at += 1;
        let quoted = bytes.get(at) == Some(&b'"');
        let begin = at + usize::from(quoted);
        let end = if quoted {
            bytes[begin..]
                .iter()
                .position(|byte| *byte == b'"')
                .map(|length| begin + length)
                .ok_or_else(|| invalid(begin, bytes.len()))?
        } else {
            bytes[begin..]
                .iter()
                .position(|byte| *byte == b',')
                .map_or(bytes.len(), |length| begin + length)
        };
        let item = &value[begin..end];
        if (!quoted && (item.is_empty() || item.chars().any(|c| c.is_whitespace() || c == '"')))
            || item.contains(['\r', '\n'])
        {
            return Err(invalid(begin, end));
        }
        result.push(Attribute {
            name,
            value: item,
            quoted,
            span: Span {
                start: offset + begin,
                end: offset + end,
            },
        });
        at = end + usize::from(quoted);
        if at < bytes.len() {
            if bytes[at] != b',' || at + 1 == bytes.len() {
                return Err(invalid(at, bytes.len()));
            }
            at += 1;
        }
    }
    Ok(result)
}

pub(super) fn get<'a>(attributes: &[Attribute<'a>], name: &str) -> Option<Attribute<'a>> {
    attributes
        .iter()
        .find(|attribute| attribute.name == name)
        .copied()
}

pub(super) fn integer(value: &str) -> Option<u64> {
    (!value.is_empty() && value.len() <= 20 && value.bytes().all(|byte| byte.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

pub(super) fn uri(value: &str) -> bool {
    super::uri::valid(value)
}

#[derive(Clone, Copy)]
pub(super) struct Decimal<'a> {
    pub whole: &'a str,
    pub fraction: &'a str,
    pub floating: bool,
}

impl<'a> Decimal<'a> {
    pub fn parse(value: &'a str) -> Option<Self> {
        let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
        let floating = value.contains('.');
        if !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
            || whole.is_empty() && fraction.is_empty()
            || !floating && whole.is_empty()
        {
            return None;
        }
        Some(Self {
            whole: whole.trim_start_matches('0'),
            fraction,
            floating,
        })
    }
    pub fn rounds_above(self, target: &str) -> bool {
        let whole = if self.whole.is_empty() {
            "0"
        } else {
            self.whole
        };
        let order = whole
            .len()
            .cmp(&target.len())
            .then_with(|| whole.cmp(target));
        order.is_gt()
            || order.is_eq()
                && self
                    .fraction
                    .as_bytes()
                    .first()
                    .is_some_and(|byte| *byte >= b'5')
    }
}
