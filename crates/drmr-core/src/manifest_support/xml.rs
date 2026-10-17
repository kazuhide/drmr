//! `xml.etree.ElementTree` as the manifest modules use it: `ET.fromstring` on expat (namespace
//! mode, separator `}`), and the serializer behind `ET.indent` + `ElementTree.write(…,
//! encoding="utf-8", xml_declaration=True)`.
//!
//! The reader reproduces what the Python code can observe: the element tree (`tag` as
//! `{uri}local`, attributes in document order, `text` / `tail` as `TreeBuilder` assembles them —
//! comments and processing instructions dropped, CDATA and references merged, newlines
//! normalized) and, for a malformed document, expat's message and position (`<message>: line L,
//! column C`, the column counted in characters from 0). The tokenizer follows expat's
//! (`xmltok_impl.c`): which character a "not well-formed (invalid token)" points at, which
//! errors point at the start tag, `unclosed token` at end of input. Two documented limits:
//! non-ASCII name characters use the Unicode categories of XML 1.0 Appendix B instead of
//! expat's own tables, and the prolog and epilog garbage that no manifest carries is tokenized
//! in the common cases only. A document type declaration is not supported: every caller
//! rejects `<!DOCTYPE` before parsing, and so do the Rust ones.

use super::py::{PyErr, strip};
use std::fmt::Write as _;
use std::sync::LazyLock;

/// The XML namespace (`xml:` is bound to it implicitly).
pub const XML_NAMESPACE: &str = "http://www.w3.org/XML/1998/namespace";
/// The namespace of `xmlns` declarations.
pub const XMLNS_NAMESPACE: &str = "http://www.w3.org/2000/xmlns/";

/// An `ElementTree` element.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    /// `{uri}local`, or `local` outside any namespace.
    pub tag: String,
    /// Attributes in document order; a namespaced key is `{uri}local`.
    pub attrib: Vec<(String, String)>,
    /// Character data before the first child.
    pub text: Option<String>,
    /// Character data after this element's end tag, before the next sibling.
    pub tail: Option<String>,
    /// Child elements.
    pub children: Vec<Element>,
}

impl Element {
    /// `ET.Element(tag, attrib)`.
    pub fn new(tag: &str, attrib: &[(&str, &str)]) -> Self {
        Element {
            tag: tag.to_string(),
            attrib: attrib
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
            ..Element::default()
        }
    }

    /// `ET.SubElement(self, tag, attrib)`.
    pub fn sub_element(&mut self, tag: &str, attrib: &[(&str, &str)]) -> &mut Element {
        self.children.push(Element::new(tag, attrib));
        self.children.last_mut().expect("just pushed")
    }

    /// The local name: `tag.rsplit("}", 1)[-1]`.
    pub fn local_name(&self) -> &str {
        self.tag.rsplit('}').next().unwrap_or(&self.tag)
    }

    /// `elem.get(key)`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.attrib
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }

    /// `ElementTree`'s `elem.iter`: this element and every descendant, in document order (named
    /// apart from Rust's `iter`, which would suggest the children only).
    pub fn iter_tree(&self) -> Descendants<'_> {
        Descendants { stack: vec![self] }
    }

    /// Every element of `iter_tree` whose local name is `local` (the implementations' `_iter_local`).
    pub fn iter_local(&self, local: &str) -> Vec<&Element> {
        self.iter_tree()
            .filter(|element| element.local_name() == local)
            .collect()
    }
}

/// The pre-order walk of [`Element::iter_tree`].
pub struct Descendants<'a> {
    stack: Vec<&'a Element>,
}

impl<'a> Iterator for Descendants<'a> {
    type Item = &'a Element;

    fn next(&mut self) -> Option<&'a Element> {
        let element = self.stack.pop()?;
        self.stack.extend(element.children.iter().rev());
        Some(element)
    }
}

/// `ET.ParseError` (expat's `ExpatError`): `str` is `<message>: line L, column C`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}: line {line}, column {column}")]
pub struct ParseError {
    /// expat's `XML_ErrorString`.
    pub message: &'static str,
    /// 1-based line.
    pub line: usize,
    /// 0-based column, in characters.
    pub column: usize,
}

const SYNTAX: &str = "syntax error";
const NO_ELEMENTS: &str = "no element found";
const INVALID_TOKEN: &str = "not well-formed (invalid token)";
const UNCLOSED_TOKEN: &str = "unclosed token";
const TAG_MISMATCH: &str = "mismatched tag";
const DUPLICATE_ATTRIBUTE: &str = "duplicate attribute";
const JUNK_AFTER_DOC_ELEMENT: &str = "junk after document element";
const UNDEFINED_ENTITY: &str = "undefined entity";
const BAD_CHAR_REF: &str = "reference to invalid character number";
const MISPLACED_XML_PI: &str = "XML or text declaration not at start of entity";
const XML_DECL: &str = "XML declaration not well-formed";
const UNCLOSED_CDATA_SECTION: &str = "unclosed CDATA section";
const UNBOUND_PREFIX: &str = "unbound prefix";
const UNDECLARING_PREFIX: &str = "must not undeclare prefix";
const RESERVED_PREFIX_XML: &str =
    "reserved prefix (xml) must not be undeclared or bound to another namespace name";
const RESERVED_PREFIX_XMLNS: &str = "reserved prefix (xmlns) must not be declared or undeclared";
const RESERVED_NAMESPACE_URI: &str =
    "prefix must not be bound to one of the reserved namespace names";

/// An error at a byte offset, before it is turned into a line and column.
struct Fault {
    message: &'static str,
    at: usize,
}

type Scan<T> = Result<T, Fault>;

fn fault<T>(message: &'static str, at: usize) -> Scan<T> {
    Err(Fault { message, at })
}

/// `ET.fromstring(text)`.
pub fn fromstring(text: &str) -> Result<Element, ParseError> {
    let mut parser = Parser::new(text);
    parser.document().map_err(|fault| {
        let (line, column) = line_column(text, fault.at);
        ParseError {
            message: fault.message,
            line,
            column,
        }
    })
}

/// expat's position: lines broken by `\n`, `\r\n` or `\r`; the column in characters.
fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 0;
    let mut chars = text[..offset.min(text.len())].chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                line += 1;
                column = 0;
            }
            '\n' => {
                line += 1;
                column = 0;
            }
            _ => column += 1,
        }
    }
    (line, column)
}

static NAME_START: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[\p{Ll}\p{Lu}\p{Lo}\p{Lt}\p{Nl}]$").expect("static pattern")
});
static NAME_CHAR: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[\p{Ll}\p{Lu}\p{Lo}\p{Lt}\p{Nl}\p{Mc}\p{Me}\p{Mn}\p{Lm}\p{Nd}\u{B7}]$")
        .expect("static pattern")
});

/// A character that may start a name (`:` excluded: namespace mode).
fn is_name_start(character: char) -> bool {
    match character {
        'a'..='z' | 'A'..='Z' | '_' => true,
        _ if u32::from(character) < 0x80 || u32::from(character) >= 0x1_0000 => false,
        _ => NAME_START.is_match(character.encode_utf8(&mut [0u8; 4])),
    }
}

/// A character that may continue a name (`:` excluded: namespace mode).
fn is_name_char(character: char) -> bool {
    match character {
        'a'..='z' | 'A'..='Z' | '0'..='9' | '_' | '.' | '-' => true,
        _ if u32::from(character) < 0x80 || u32::from(character) >= 0x1_0000 => false,
        _ => NAME_CHAR.is_match(character.encode_utf8(&mut [0u8; 4])),
    }
}

/// XML white space (expat's `BT_S`, `BT_CR`, `BT_LF`).
fn is_xml_space(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\r' | '\n')
}

/// A character expat rejects anywhere (`BT_NONXML`): C0 controls other than tab, LF and CR,
/// and U+FFFE / U+FFFF.
fn is_non_xml(character: char) -> bool {
    let code = u32::from(character);
    (code < 0x20 && !matches!(character, '\t' | '\n' | '\r')) || code == 0xFFFE || code == 0xFFFF
}

/// A tokenized attribute of a start tag.
struct RawAttribute {
    name: (usize, usize),
    value: (usize, usize),
}

/// A tokenized start tag.
struct StartTag {
    start: usize,
    name: (usize, usize),
    attributes: Vec<RawAttribute>,
    empty: bool,
    end: usize,
}

/// One element on the open stack: the tree node, its raw name and its namespace bindings.
struct Open {
    element: Element,
    raw_name: String,
    bindings: Vec<(Option<String>, String)>,
}

struct Parser<'a> {
    text: &'a str,
    open: Vec<Open>,
    root: Option<Element>,
    data: String,
    tail: bool,
}

impl<'a> Parser<'a> {
    fn new(text: &'a str) -> Self {
        Parser {
            text,
            open: Vec::new(),
            root: None,
            data: String::new(),
            tail: false,
        }
    }

    fn char_at(&self, at: usize) -> Option<char> {
        self.text.get(at..).and_then(|rest| rest.chars().next())
    }

    fn starts_with(&self, at: usize, prefix: &str) -> bool {
        self.text
            .get(at..)
            .is_some_and(|rest| rest.starts_with(prefix))
    }

    fn document(&mut self) -> Scan<Element> {
        let start = if self.text.starts_with('\u{feff}') {
            3
        } else {
            0
        };
        let root_start = self.prolog(start)?;
        let end = self.content(root_start)?;
        self.epilog(end)?;
        Ok(self.root.take().expect("the root element closed"))
    }

    // ---------------------------------------------------------------- prolog and epilog

    fn prolog(&mut self, document_start: usize) -> Scan<usize> {
        let mut at = document_start;
        loop {
            at = self.skip_space(at);
            let Some(character) = self.char_at(at) else {
                return fault(NO_ELEMENTS, at);
            };
            if character == '<' {
                match self.char_at(at + 1) {
                    None => return fault(UNCLOSED_TOKEN, at),
                    Some('!') => {
                        at = self.markup_declaration(at, true)?;
                        continue;
                    }
                    Some('?') => {
                        let (end, is_xml) = self.scan_pi(at)?;
                        if is_xml {
                            if at != document_start {
                                return fault(MISPLACED_XML_PI, at);
                            }
                            parse_xml_decl(self.text, at, end)?;
                        }
                        at = end;
                        continue;
                    }
                    Some(next) if is_name_start(next) => return Ok(at),
                    Some(_) => return fault(INVALID_TOKEN, at + 1),
                }
            }
            return self.misc_token(at, SYNTAX);
        }
    }

    fn epilog(&mut self, from: usize) -> Scan<()> {
        let mut at = from;
        loop {
            at = self.skip_space(at);
            let Some(character) = self.char_at(at) else {
                return Ok(());
            };
            if character == '<' {
                match self.char_at(at + 1) {
                    None => return fault(UNCLOSED_TOKEN, at),
                    Some('!') => {
                        at = self.markup_declaration(at, false)?;
                        continue;
                    }
                    Some('?') => {
                        let (end, is_xml) = self.scan_pi(at)?;
                        if is_xml {
                            return fault(JUNK_AFTER_DOC_ELEMENT, at);
                        }
                        at = end;
                        continue;
                    }
                    Some(next) if is_name_start(next) => {
                        return fault(JUNK_AFTER_DOC_ELEMENT, at);
                    }
                    Some(_) => return fault(INVALID_TOKEN, at + 1),
                }
            }
            return self.misc_token(at, JUNK_AFTER_DOC_ELEMENT);
        }
    }

    fn skip_space(&self, mut at: usize) -> usize {
        while self.char_at(at).is_some_and(is_xml_space) {
            at += 1;
        }
        at
    }

    /// `<!--…-->` (returns the position after it) or another `<!` declaration, which outside
    /// a DTD is `syntax error` in the prolog and junk in the epilog.
    fn markup_declaration(&mut self, at: usize, in_prolog: bool) -> Scan<usize> {
        match self.char_at(at + 2) {
            None => fault(UNCLOSED_TOKEN, at),
            Some('-') => self.scan_comment(at),
            Some('[') => fault(
                if in_prolog {
                    SYNTAX
                } else {
                    JUNK_AFTER_DOC_ELEMENT
                },
                at,
            ),
            Some(next) if is_name_start(next) => fault(
                if in_prolog {
                    SYNTAX
                } else {
                    JUNK_AFTER_DOC_ELEMENT
                },
                at,
            ),
            Some(_) => fault(INVALID_TOKEN, at + 2),
        }
    }

    /// Anything else outside the root element, tokenized as expat's `prologTok` does in the
    /// common cases: a name or name token followed by a delimiter is a token (`role` error),
    /// followed by anything else an invalid token.
    fn misc_token<T>(&self, at: usize, role_error: &'static str) -> Scan<T> {
        let character = self.char_at(at).expect("not at the end");
        if is_name_char(character) {
            let mut end = at + character.len_utf8();
            while let Some(next) = self.char_at(end) {
                if is_name_char(next) || next == ':' {
                    end += next.len_utf8();
                } else {
                    break;
                }
            }
            return match self.char_at(end) {
                None => fault(role_error, at),
                Some(next) if is_xml_space(next) || ">),|[%+*?".contains(next) => {
                    fault(role_error, at)
                }
                Some(_) => fault(INVALID_TOKEN, end),
            };
        }
        match character {
            '>' | '[' | ']' | '(' | ')' | '|' | ',' | '%' | '#' => fault(role_error, at),
            '"' | '\'' => {
                let body = at + 1;
                match self.text[body..].find(character) {
                    None => fault(UNCLOSED_TOKEN, at),
                    Some(offset) => {
                        let after = body + offset + 1;
                        match self.char_at(after) {
                            None => fault(role_error, at),
                            Some(next) if is_xml_space(next) || ">%[".contains(next) => {
                                fault(role_error, at)
                            }
                            Some(_) => fault(INVALID_TOKEN, after),
                        }
                    }
                }
            }
            _ => fault(INVALID_TOKEN, at),
        }
    }

    // ---------------------------------------------------------------- content

    /// The root element and everything inside it; returns the position after its end tag.
    fn content(&mut self, root_start: usize) -> Scan<usize> {
        let mut at = root_start;
        loop {
            let Some(character) = self.char_at(at) else {
                return fault(NO_ELEMENTS, at);
            };
            match character {
                '<' => match self.char_at(at + 1) {
                    None => return fault(UNCLOSED_TOKEN, at),
                    Some('!') => at = self.content_declaration(at)?,
                    Some('?') => {
                        let (end, is_xml) = self.scan_pi(at)?;
                        if is_xml {
                            return fault(MISPLACED_XML_PI, at);
                        }
                        at = end;
                    }
                    Some('/') => {
                        at = self.end_tag(at)?;
                        if self.open.is_empty() {
                            return Ok(at);
                        }
                    }
                    Some(next) if is_name_start(next) => {
                        let tag = self.scan_start_tag(at)?;
                        at = tag.end;
                        self.start_element(&tag)?;
                        if tag.empty {
                            self.end_element();
                            if self.open.is_empty() {
                                return Ok(at);
                            }
                        }
                    }
                    Some(_) => return fault(INVALID_TOKEN, at + 1),
                },
                '&' => at = self.content_reference(at)?,
                _ => at = self.character_data(at)?,
            }
        }
    }

    fn content_declaration(&mut self, at: usize) -> Scan<usize> {
        match self.char_at(at + 2) {
            None => fault(UNCLOSED_TOKEN, at),
            Some('-') => self.scan_comment(at),
            Some('[') => self.cdata_section(at),
            Some(_) => fault(INVALID_TOKEN, at + 2),
        }
    }

    fn character_data(&mut self, from: usize) -> Scan<usize> {
        let mut at = from;
        while let Some(character) = self.char_at(at) {
            match character {
                '<' | '&' => break,
                ']' if self.starts_with(at, "]]>") => return fault(INVALID_TOKEN, at + 2),
                '\r' => {
                    self.data.push('\n');
                    at += if self.starts_with(at, "\r\n") { 2 } else { 1 };
                    continue;
                }
                _ if is_non_xml(character) => return fault(INVALID_TOKEN, at),
                _ => self.data.push(character),
            }
            at += character.len_utf8();
        }
        Ok(at)
    }

    fn content_reference(&mut self, at: usize) -> Scan<usize> {
        let (end, reference) = self.scan_reference(at, at)?;
        let character = match reference {
            Reference::Char(Some(character)) | Reference::Entity(Some(character)) => character,
            Reference::Char(None) => return fault(BAD_CHAR_REF, at),
            Reference::Entity(None) => return fault(UNDEFINED_ENTITY, at),
        };
        self.data.push(character);
        Ok(end)
    }

    fn scan_comment(&self, at: usize) -> Scan<usize> {
        // `<!-` is known; the fourth character must be `-`.
        let mut cursor = at + 3;
        match self.char_at(cursor) {
            None => return fault(UNCLOSED_TOKEN, at),
            Some('-') => cursor += 1,
            Some(_) => return fault(INVALID_TOKEN, cursor),
        }
        while let Some(character) = self.char_at(cursor) {
            if is_non_xml(character) {
                return fault(INVALID_TOKEN, cursor);
            }
            cursor += character.len_utf8();
            if character == '-' {
                match self.char_at(cursor) {
                    None => return fault(UNCLOSED_TOKEN, at),
                    Some('-') => {
                        cursor += 1;
                        return match self.char_at(cursor) {
                            None => fault(UNCLOSED_TOKEN, at),
                            Some('>') => Ok(cursor + 1),
                            Some(_) => fault(INVALID_TOKEN, cursor),
                        };
                    }
                    Some(_) => {}
                }
            }
        }
        fault(UNCLOSED_TOKEN, at)
    }

    /// `<?target …?>`; returns the end and whether the target is exactly `xml` (an XML
    /// declaration token). A target spelled `xml` in another case is an invalid token.
    fn scan_pi(&self, at: usize) -> Scan<(usize, bool)> {
        let target_start = at + 2;
        match self.char_at(target_start) {
            None => return fault(UNCLOSED_TOKEN, at),
            Some(character) if is_name_start(character) => {}
            Some(_) => return fault(INVALID_TOKEN, target_start),
        }
        let mut cursor = target_start;
        while let Some(character) = self.char_at(cursor) {
            if is_name_char(character) {
                cursor += character.len_utf8();
                continue;
            }
            let target = &self.text[target_start..cursor];
            let is_xml = target == "xml";
            if target.len() == 3 && target.eq_ignore_ascii_case("xml") && !is_xml {
                return fault(INVALID_TOKEN, cursor);
            }
            if is_xml_space(character) {
                cursor += 1;
                while let Some(next) = self.char_at(cursor) {
                    if is_non_xml(next) {
                        return fault(INVALID_TOKEN, cursor);
                    }
                    cursor += next.len_utf8();
                    if next == '?' {
                        match self.char_at(cursor) {
                            None => return fault(UNCLOSED_TOKEN, at),
                            Some('>') => return Ok((cursor + 1, is_xml)),
                            Some(_) => {}
                        }
                    }
                }
                return fault(UNCLOSED_TOKEN, at);
            }
            if character == '?' {
                return match self.char_at(cursor + 1) {
                    None => fault(UNCLOSED_TOKEN, at),
                    Some('>') => Ok((cursor + 2, is_xml)),
                    Some(_) => fault(INVALID_TOKEN, cursor + 1),
                };
            }
            return fault(INVALID_TOKEN, cursor);
        }
        fault(UNCLOSED_TOKEN, at)
    }

    fn cdata_section(&mut self, at: usize) -> Scan<usize> {
        // `<![` is known; then `CDATA[`.
        let mut cursor = at + 3;
        for expected in "CDATA[".chars() {
            match self.char_at(cursor) {
                None => return fault(UNCLOSED_TOKEN, at),
                Some(character) if character == expected => cursor += 1,
                Some(_) => return fault(INVALID_TOKEN, cursor),
            }
        }
        loop {
            let Some(character) = self.char_at(cursor) else {
                return fault(UNCLOSED_CDATA_SECTION, cursor);
            };
            match character {
                ']' => {
                    if self.starts_with(cursor, "]]>") {
                        return Ok(cursor + 3);
                    }
                    let partial = self.text[cursor..] == *"]" || self.text[cursor..] == *"]]";
                    if partial {
                        return fault(UNCLOSED_CDATA_SECTION, cursor);
                    }
                    self.data.push(']');
                    cursor += 1;
                }
                '\r' => {
                    if cursor + 1 == self.text.len() {
                        return fault(UNCLOSED_CDATA_SECTION, cursor);
                    }
                    self.data.push('\n');
                    cursor += if self.starts_with(cursor, "\r\n") {
                        2
                    } else {
                        1
                    };
                }
                _ if is_non_xml(character) => return fault(INVALID_TOKEN, cursor),
                _ => {
                    self.data.push(character);
                    cursor += character.len_utf8();
                }
            }
        }
    }

    fn end_tag(&mut self, at: usize) -> Scan<usize> {
        let name_start = at + 2;
        match self.char_at(name_start) {
            None => return fault(UNCLOSED_TOKEN, at),
            Some(character) if is_name_start(character) => {}
            Some(_) => return fault(INVALID_TOKEN, name_start),
        }
        let mut cursor = name_start;
        let name_end;
        loop {
            let Some(character) = self.char_at(cursor) else {
                return fault(UNCLOSED_TOKEN, at);
            };
            if is_name_char(character) || character == ':' {
                cursor += character.len_utf8();
            } else if character == '>' {
                name_end = cursor;
                cursor += 1;
                break;
            } else if is_xml_space(character) {
                name_end = cursor;
                cursor = self.skip_space(cursor);
                match self.char_at(cursor) {
                    None => return fault(UNCLOSED_TOKEN, at),
                    Some('>') => {
                        cursor += 1;
                        break;
                    }
                    Some(_) => return fault(INVALID_TOKEN, cursor),
                }
            } else {
                return fault(INVALID_TOKEN, cursor);
            }
        }
        let name = &self.text[name_start..name_end];
        if self.open.last().map(|open| open.raw_name.as_str()) != Some(name) {
            return fault(TAG_MISMATCH, name_start);
        }
        self.end_element();
        Ok(cursor)
    }

    // ---------------------------------------------------------------- start tags

    /// Tokenize a start tag as expat's `scanLt` / `scanAtts` do (namespace mode: one `:` in a
    /// name, followed by a name start).
    fn scan_start_tag(&self, at: usize) -> Scan<StartTag> {
        let name_start = at + 1;
        let (name_end, mut cursor) = self.scan_qname(at, name_start)?;
        let mut tag = StartTag {
            start: at,
            name: (name_start, name_end),
            attributes: Vec::new(),
            empty: false,
            end: 0,
        };
        loop {
            let Some(character) = self.char_at(cursor) else {
                return fault(UNCLOSED_TOKEN, at);
            };
            match character {
                '>' => {
                    tag.end = cursor + 1;
                    return Ok(tag);
                }
                '/' => {
                    return match self.char_at(cursor + 1) {
                        None => fault(UNCLOSED_TOKEN, at),
                        Some('>') => {
                            tag.empty = true;
                            tag.end = cursor + 2;
                            Ok(tag)
                        }
                        Some(_) => fault(INVALID_TOKEN, cursor + 1),
                    };
                }
                _ if is_xml_space(character) => {
                    cursor = self.skip_space(cursor);
                    match self.char_at(cursor) {
                        None => return fault(UNCLOSED_TOKEN, at),
                        Some('>' | '/') => {}
                        Some(next) if is_name_start(next) => {
                            let attribute;
                            (attribute, cursor) = self.scan_attribute(at, cursor)?;
                            tag.attributes.push(attribute);
                        }
                        Some(_) => return fault(INVALID_TOKEN, cursor),
                    }
                }
                _ => return fault(INVALID_TOKEN, cursor),
            }
        }
    }

    /// A qualified name from `start` (a name start); returns its end and the cursor after it.
    fn scan_qname(&self, token: usize, start: usize) -> Scan<(usize, usize)> {
        let mut cursor = start;
        let mut had_colon = false;
        loop {
            let Some(character) = self.char_at(cursor) else {
                return fault(UNCLOSED_TOKEN, token);
            };
            if is_name_char(character) {
                cursor += character.len_utf8();
            } else if character == ':' {
                if had_colon {
                    return fault(INVALID_TOKEN, cursor);
                }
                had_colon = true;
                cursor += 1;
                match self.char_at(cursor) {
                    None => return fault(UNCLOSED_TOKEN, token),
                    Some(next) if is_name_start(next) => cursor += next.len_utf8(),
                    Some(_) => return fault(INVALID_TOKEN, cursor),
                }
            } else {
                return Ok((cursor, cursor));
            }
        }
    }

    /// One attribute from its name start; returns it and the cursor after the closing quote,
    /// where white space, `>` or `/` must follow.
    fn scan_attribute(&self, token: usize, start: usize) -> Scan<(RawAttribute, usize)> {
        let (name_end, mut cursor) = self.scan_qname(token, start)?;
        cursor = self.skip_space(cursor);
        match self.char_at(cursor) {
            None => return fault(UNCLOSED_TOKEN, token),
            Some('=') => cursor += 1,
            Some(_) => return fault(INVALID_TOKEN, cursor),
        }
        cursor = self.skip_space(cursor);
        let quote = match self.char_at(cursor) {
            None => return fault(UNCLOSED_TOKEN, token),
            Some(quote @ ('"' | '\'')) => quote,
            Some(_) => return fault(INVALID_TOKEN, cursor),
        };
        let value_start = cursor + 1;
        cursor = value_start;
        loop {
            let Some(character) = self.char_at(cursor) else {
                return fault(UNCLOSED_TOKEN, token);
            };
            if character == quote {
                break;
            }
            match character {
                '<' => return fault(INVALID_TOKEN, cursor),
                '&' => cursor = self.scan_reference(cursor, token)?.0,
                _ if is_non_xml(character) => return fault(INVALID_TOKEN, cursor),
                _ => cursor += character.len_utf8(),
            }
        }
        let attribute = RawAttribute {
            name: (start, name_end),
            value: (value_start, cursor),
        };
        cursor += 1;
        match self.char_at(cursor) {
            None => fault(UNCLOSED_TOKEN, token),
            Some(next) if is_xml_space(next) || next == '>' || next == '/' => {
                Ok((attribute, cursor))
            }
            Some(_) => fault(INVALID_TOKEN, cursor),
        }
    }

    /// `&name;` / `&#n;` / `&#xh;` at `at` (expat's `scanRef`): its end and value. A partial
    /// reference is an unclosed `token` (the enclosing token: the start tag in a value).
    fn scan_reference(&self, at: usize, token: usize) -> Scan<(usize, Reference)> {
        let mut cursor = at + 1;
        let Some(first) = self.char_at(cursor) else {
            return fault(UNCLOSED_TOKEN, token);
        };
        if first == '#' {
            cursor += 1;
            let hex = self.char_at(cursor) == Some('x');
            if hex {
                cursor += 1;
            }
            let digits_start = cursor;
            loop {
                let Some(character) = self.char_at(cursor) else {
                    return fault(UNCLOSED_TOKEN, token);
                };
                let digit = if hex {
                    character.is_ascii_hexdigit()
                } else {
                    character.is_ascii_digit()
                };
                if digit {
                    cursor += 1;
                } else if character == ';' && cursor > digits_start {
                    let digits = &self.text[digits_start..cursor];
                    return Ok((cursor + 1, Reference::Char(char_reference(digits, hex))));
                } else {
                    return fault(INVALID_TOKEN, cursor);
                }
            }
        }
        if !is_name_start(first) {
            return fault(INVALID_TOKEN, cursor);
        }
        cursor += first.len_utf8();
        loop {
            let Some(character) = self.char_at(cursor) else {
                return fault(UNCLOSED_TOKEN, token);
            };
            if is_name_char(character) {
                cursor += character.len_utf8();
            } else if character == ';' {
                let name = &self.text[at + 1..cursor];
                let value = match name {
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "amp" => Some('&'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => None,
                };
                return Ok((cursor + 1, Reference::Entity(value)));
            } else {
                return fault(INVALID_TOKEN, cursor);
            }
        }
    }

    /// expat's `storeAtts` for a tokenized start tag, then `TreeBuilder.start`.
    fn start_element(&mut self, tag: &StartTag) -> Scan<()> {
        let mut seen: Vec<&str> = Vec::new();
        let mut bindings: Vec<(Option<String>, String)> = Vec::new();
        let mut plain: Vec<(String, String)> = Vec::new();
        let mut prefixed: Vec<(&str, &str, String)> = Vec::new();
        for attribute in &tag.attributes {
            let name = &self.text[attribute.name.0..attribute.name.1];
            if seen.contains(&name) {
                return fault(DUPLICATE_ATTRIBUTE, attribute.name.0);
            }
            seen.push(name);
            let value = self.attribute_value(attribute.value, tag.start)?;
            if name == "xmlns" {
                bind(None, &value, tag.start)?;
                bindings.push((None, value));
            } else if let Some(prefix) = name.strip_prefix("xmlns:") {
                bind(Some(prefix), &value, tag.start)?;
                bindings.push((Some(prefix.to_string()), value));
            } else if let Some((prefix, local)) = name.split_once(':') {
                prefixed.push((prefix, local, value));
            } else {
                plain.push((name.to_string(), value));
            }
        }
        let lookup = |prefix: Option<&str>| -> Option<String> {
            for (bound, uri) in bindings.iter().rev() {
                if bound.as_deref() == prefix {
                    return Some(uri.clone());
                }
            }
            self.lookup(prefix)
        };
        let mut attrib = Vec::new();
        let mut expanded: Vec<(String, String)> = Vec::new();
        for (prefix, local, value) in prefixed {
            let Some(uri) = lookup(Some(prefix)).filter(|uri| !uri.is_empty()) else {
                return fault(UNBOUND_PREFIX, tag.start);
            };
            let key = (uri, local.to_string());
            if expanded.contains(&key) {
                return fault(DUPLICATE_ATTRIBUTE, tag.start);
            }
            expanded.push(key.clone());
            attrib.push((format!("{{{}}}{}", key.0, key.1), value));
        }
        let raw_name = &self.text[tag.name.0..tag.name.1];
        let tag_name = match raw_name.split_once(':') {
            Some((prefix, local)) => match lookup(Some(prefix)).filter(|uri| !uri.is_empty()) {
                Some(uri) => format!("{{{uri}}}{local}"),
                None => return fault(UNBOUND_PREFIX, tag.start),
            },
            None => match lookup(None).filter(|uri| !uri.is_empty()) {
                Some(uri) => format!("{{{uri}}}{raw_name}"),
                None => raw_name.to_string(),
            },
        };
        // expat reports the attributes in document order; the prefixed ones were collected
        // apart only to resolve them after every declaration of the tag.
        let mut ordered = Vec::with_capacity(attrib.len() + plain.len());
        let mut plain_iter = plain.into_iter();
        let mut prefixed_iter = attrib.into_iter();
        for attribute in &tag.attributes {
            let name = &self.text[attribute.name.0..attribute.name.1];
            if name == "xmlns" || name.starts_with("xmlns:") {
                continue;
            }
            let next = if name.contains(':') {
                prefixed_iter.next()
            } else {
                plain_iter.next()
            };
            ordered.push(next.expect("one entry per attribute"));
        }
        self.flush();
        self.open.push(Open {
            element: Element {
                tag: tag_name,
                attrib: ordered,
                ..Element::default()
            },
            raw_name: raw_name.to_string(),
            bindings,
        });
        self.tail = false;
        Ok(())
    }

    fn lookup(&self, prefix: Option<&str>) -> Option<String> {
        for open in self.open.iter().rev() {
            for (bound, uri) in open.bindings.iter().rev() {
                if bound.as_deref() == prefix {
                    return Some(uri.clone());
                }
            }
        }
        (prefix == Some("xml")).then(|| XML_NAMESPACE.to_string())
    }

    /// A CDATA attribute value as expat normalizes it: tab, CR, LF and CR LF become one space;
    /// references are replaced. A bad character reference points at its `&`, an undefined
    /// entity at the start tag.
    fn attribute_value(&self, range: (usize, usize), tag_start: usize) -> Scan<String> {
        let mut value = String::new();
        let mut cursor = range.0;
        while cursor < range.1 {
            let character = self.char_at(cursor).expect("inside the value");
            match character {
                '&' => {
                    let (end, reference) = self.scan_reference(cursor, tag_start)?;
                    match reference {
                        Reference::Char(Some(resolved)) | Reference::Entity(Some(resolved)) => {
                            value.push(resolved);
                        }
                        Reference::Char(None) => return fault(BAD_CHAR_REF, cursor),
                        Reference::Entity(None) => return fault(UNDEFINED_ENTITY, tag_start),
                    }
                    cursor = end;
                }
                '\r' => {
                    value.push(' ');
                    cursor += if self.starts_with(cursor, "\r\n") {
                        2
                    } else {
                        1
                    };
                }
                '\t' | '\n' => {
                    value.push(' ');
                    cursor += 1;
                }
                _ => {
                    value.push(character);
                    cursor += character.len_utf8();
                }
            }
        }
        Ok(value)
    }

    // ---------------------------------------------------------------- tree building

    /// `TreeBuilder._flush`: pending character data becomes the text of the element just
    /// started or the tail of the element just closed.
    fn flush(&mut self) {
        if self.data.is_empty() {
            return;
        }
        let data = std::mem::take(&mut self.data);
        if let Some(open) = self.open.last_mut() {
            if self.tail {
                if let Some(last) = open.element.children.last_mut() {
                    last.tail = Some(data);
                }
            } else {
                open.element.text = Some(data);
            }
        }
    }

    fn end_element(&mut self) {
        self.flush();
        let closed = self.open.pop().expect("an open element").element;
        match self.open.last_mut() {
            Some(parent) => parent.element.children.push(closed),
            None => self.root = Some(closed),
        }
        self.tail = true;
    }
}

enum Reference {
    Char(Option<char>),
    Entity(Option<char>),
}

/// expat's `XmlCharRefNumber` + `checkCharRefNumber`: `None` for a value XML forbids.
fn char_reference(digits: &str, hex: bool) -> Option<char> {
    let radix = if hex { 16 } else { 10 };
    let mut value: u32 = 0;
    for digit in digits.chars() {
        value = value * radix + digit.to_digit(radix).expect("validated digit");
        if value >= 0x11_0000 {
            return None;
        }
    }
    let character = char::from_u32(value)?;
    (!is_non_xml(character)).then_some(character)
}

/// expat's `addBinding` checks for one namespace declaration of a start tag.
fn bind(prefix: Option<&str>, uri: &str, tag_start: usize) -> Scan<()> {
    if uri.is_empty() && prefix.is_some() {
        return fault(UNDECLARING_PREFIX, tag_start);
    }
    if prefix == Some("xmlns") {
        return fault(RESERVED_PREFIX_XMLNS, tag_start);
    }
    if uri.contains('}') {
        // ElementTree's namespace separator: expat refuses a URI that would make the expanded
        // name ambiguous.
        return fault(SYNTAX, tag_start);
    }
    let must_be_xml = prefix == Some("xml");
    let is_xml = uri == XML_NAMESPACE;
    if must_be_xml != is_xml {
        return fault(
            if must_be_xml {
                RESERVED_PREFIX_XML
            } else {
                RESERVED_NAMESPACE_URI
            },
            tag_start,
        );
    }
    if uri == XMLNS_NAMESPACE {
        return fault(RESERVED_NAMESPACE_URI, tag_start);
    }
    Ok(())
}

/// One pseudo-attribute of an XML declaration (`name="value"`).
struct Pseudo {
    name: (usize, usize),
    value: (usize, usize),
    next: usize,
}

/// expat's `parsePseudoAttribute` over `bytes[cursor..stop]`: `Ok(None)` when only white space
/// is left (the caller's cursor stays where it was, as in expat), the attribute, or the
/// position expat reports.
fn pseudo_attribute(bytes: &[u8], cursor: usize, stop: usize) -> Result<Option<Pseudo>, usize> {
    let ascii = |at: usize| (at < stop && bytes[at].is_ascii()).then(|| bytes[at]);
    let space = |at: usize| matches!(ascii(at), Some(b' ' | b'\t' | b'\r' | b'\n'));
    let mut at = cursor;
    if at == stop {
        return Ok(None);
    }
    if !space(at) {
        return Err(at);
    }
    while space(at) {
        at += 1;
    }
    if at == stop {
        return Ok(None);
    }
    let name_start = at;
    let name_end;
    loop {
        match ascii(at) {
            None => return Err(at),
            Some(b'=') => {
                name_end = at;
                break;
            }
            Some(_) if space(at) => {
                name_end = at;
                while space(at) {
                    at += 1;
                }
                if ascii(at) != Some(b'=') {
                    return Err(at);
                }
                break;
            }
            Some(_) => at += 1,
        }
    }
    if at == name_start {
        return Err(at);
    }
    at += 1;
    while space(at) {
        at += 1;
    }
    let Some(open @ (b'"' | b'\'')) = ascii(at) else {
        return Err(at);
    };
    at += 1;
    let value_start = at;
    loop {
        match ascii(at) {
            Some(byte) if byte == open => break,
            Some(byte) if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_') => {
                at += 1;
            }
            _ => return Err(at),
        }
    }
    Ok(Some(Pseudo {
        name: (name_start, name_end),
        value: (value_start, at),
        next: at + 1,
    }))
}

/// expat's `doParseXmlDecl` over the token `[start, end)` (`<?xml … ?>`).
fn parse_xml_decl(text: &str, start: usize, end: usize) -> Scan<()> {
    let bytes = text.as_bytes();
    let stop = end - 2;
    let bad = |at: usize| fault(XML_DECL, at);
    let name_of = |range: (usize, usize)| &text[range.0..range.1];
    let mut cursor = start + 5;
    let version = match pseudo_attribute(bytes, cursor, stop) {
        Err(at) => return bad(at),
        Ok(None) => return bad(cursor),
        Ok(Some(version)) => version,
    };
    if name_of(version.name) != "version" {
        return bad(version.name.0);
    }
    cursor = version.next;
    let mut current = match pseudo_attribute(bytes, cursor, stop) {
        Err(at) => return bad(at),
        Ok(None) => return Ok(()),
        Ok(Some(next)) => next,
    };
    if name_of(current.name) == "encoding" {
        if !bytes[current.value.0].is_ascii_alphabetic() {
            return bad(current.value.0);
        }
        cursor = current.next;
        current = match pseudo_attribute(bytes, cursor, stop) {
            Err(at) => return bad(at),
            Ok(None) => return Ok(()),
            Ok(Some(next)) => next,
        };
    }
    if name_of(current.name) != "standalone" {
        return bad(current.name.0);
    }
    if !matches!(name_of(current.value), "yes" | "no") {
        return bad(current.value.0);
    }
    cursor = current.next;
    while cursor < stop && matches!(bytes[cursor], b' ' | b'\t' | b'\r' | b'\n') {
        cursor += 1;
    }
    if cursor != stop {
        return bad(cursor);
    }
    Ok(())
}

// -------------------------------------------------------------------- serializer

/// `ElementTree`'s `_namespace_map` after `register_namespace` calls: URI → prefix, in insertion
/// order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamespaceMap(Vec<(String, String)>);

impl NamespaceMap {
    /// The map `ElementTree` starts with (its "well-known" prefixes).
    pub fn elementtree_default() -> Self {
        NamespaceMap(
            [
                (XML_NAMESPACE, "xml"),
                ("http://www.w3.org/1999/xhtml", "html"),
                ("http://www.w3.org/1999/02/22-rdf-syntax-ns#", "rdf"),
                ("http://schemas.xmlsoap.org/wsdl/", "wsdl"),
                ("http://www.w3.org/2001/XMLSchema", "xs"),
                ("http://www.w3.org/2001/XMLSchema-instance", "xsi"),
                ("http://purl.org/dc/elements/1.1/", "dc"),
            ]
            .iter()
            .map(|(uri, prefix)| ((*uri).to_string(), (*prefix).to_string()))
            .collect(),
        )
    }

    /// `ET.register_namespace(prefix, uri)`.
    pub fn register(&mut self, prefix: &str, uri: &str) -> Result<(), PyErr> {
        let reserved = prefix.len() > 2
            && prefix.starts_with("ns")
            && prefix[2..].bytes().all(|byte| byte.is_ascii_digit());
        if reserved {
            return Err(PyErr::Value(
                "Prefix format reserved for internal use".to_string(),
            ));
        }
        self.0
            .retain(|(known_uri, known_prefix)| known_uri != uri && known_prefix != prefix);
        self.0.push((uri.to_string(), prefix.to_string()));
        Ok(())
    }

    fn get(&self, uri: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(known, _)| known == uri)
            .map(|(_, prefix)| prefix.as_str())
    }
}

/// `ET.indent(tree)` (two spaces, level 0).
pub fn indent(root: &mut Element) {
    if root.children.is_empty() {
        return;
    }
    let mut indentations = vec!["\n".to_string()];
    indent_children(root, 0, &mut indentations);
}

fn indent_children(element: &mut Element, level: usize, indentations: &mut Vec<String>) {
    let child_level = level + 1;
    if indentations.len() <= child_level {
        let next = format!("{}  ", indentations[level]);
        indentations.push(next);
    }
    let child_indentation = indentations[child_level].clone();
    if element
        .text
        .as_deref()
        .is_none_or(|text| strip(text).is_empty())
    {
        element.text = Some(child_indentation.clone());
    }
    for child in &mut element.children {
        if !child.children.is_empty() {
            indent_children(child, child_level, indentations);
        }
        if child
            .tail
            .as_deref()
            .is_none_or(|tail| strip(tail).is_empty())
        {
            child.tail = Some(child_indentation.clone());
        }
    }
    let last = element
        .children
        .last_mut()
        .expect("indent only elements with children");
    if last
        .tail
        .as_deref()
        .is_some_and(|tail| strip(tail).is_empty())
    {
        last.tail = Some(indentations[level].clone());
    }
}

/// The bytes `ElementTree(root).write(path, encoding="utf-8", xml_declaration=True)` writes.
pub fn tostring_with_declaration(root: &Element, map: &NamespaceMap) -> String {
    let (qnames, namespaces) = namespaces(root, map);
    let mut out = String::from("<?xml version='1.0' encoding='utf-8'?>\n");
    serialize(&mut out, root, &qnames, Some(&namespaces));
    out
}

/// Name → serialized form, or URI → prefix, in insertion order.
type Pairs = Vec<(String, String)>;

/// `_namespaces`: the serialized form of every tag and attribute key, and the URI → prefix
/// declarations the root carries (in first-use order).
fn namespaces(root: &Element, map: &NamespaceMap) -> (Pairs, Pairs) {
    let mut qnames: Pairs = Vec::new();
    let mut declared: Pairs = Vec::new();
    let mut add = |qname: &str, qnames: &mut Pairs| {
        if qnames.iter().any(|(known, _)| known == qname) {
            return;
        }
        let serialized = if let Some((uri, local)) = qname
            .strip_prefix('{')
            .and_then(|rest| rest.rsplit_once('}'))
        {
            let known = declared.iter().find(|(known, _)| known == uri);
            let prefix = if let Some((_, prefix)) = known {
                prefix.clone()
            } else {
                let prefix = map
                    .get(uri)
                    .map_or_else(|| format!("ns{}", declared.len()), str::to_string);
                if prefix != "xml" {
                    declared.push((uri.to_string(), prefix.clone()));
                }
                prefix
            };
            if prefix.is_empty() {
                local.to_string()
            } else {
                format!("{prefix}:{local}")
            }
        } else {
            qname.to_string()
        };
        qnames.push((qname.to_string(), serialized));
    };
    for element in root.iter_tree() {
        add(&element.tag, &mut qnames);
        for (key, _) in &element.attrib {
            add(key, &mut qnames);
        }
    }
    (qnames, declared)
}

fn qname<'a>(qnames: &'a [(String, String)], name: &'a str) -> &'a str {
    qnames
        .iter()
        .find(|(known, _)| known == name)
        .map_or(name, |(_, serialized)| serialized.as_str())
}

/// `_serialize_xml` with `short_empty_elements=True`.
fn serialize(
    out: &mut String,
    element: &Element,
    qnames: &[(String, String)],
    namespaces: Option<&[(String, String)]>,
) {
    let tag = qname(qnames, &element.tag);
    out.push('<');
    out.push_str(tag);
    if let Some(declarations) = namespaces {
        let mut sorted: Vec<&(String, String)> = declarations.iter().collect();
        sorted.sort_by(|left, right| left.1.cmp(&right.1));
        for (uri, prefix) in sorted {
            let colon = if prefix.is_empty() { "" } else { ":" };
            write!(out, " xmlns{colon}{prefix}=\"{}\"", escape_attrib(uri))
                .expect("writing to a String");
        }
    }
    for (key, value) in &element.attrib {
        write!(out, " {}=\"{}\"", qname(qnames, key), escape_attrib(value))
            .expect("writing to a String");
    }
    let text = element.text.as_deref().filter(|text| !text.is_empty());
    if text.is_some() || !element.children.is_empty() {
        out.push('>');
        if let Some(text) = text {
            out.push_str(&escape_cdata(text));
        }
        for child in &element.children {
            serialize(out, child, qnames, None);
        }
        out.push_str("</");
        out.push_str(tag);
        out.push('>');
    } else {
        out.push_str(" />");
    }
    if let Some(tail) = element.tail.as_deref().filter(|tail| !tail.is_empty()) {
        out.push_str(&escape_cdata(tail));
    }
}

/// `_escape_cdata`.
pub fn escape_cdata(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// `_escape_attrib` (`CPython` 3.11).
pub fn escape_attrib(text: &str) -> String {
    escape_cdata(text)
        .replace('"', "&quot;")
        .replace('\r', "&#13;")
        .replace('\n', "&#10;")
        .replace('\t', "&#09;")
}
