//! `PlayReady` PRO, WRMHEADER, and PSSH generation and parsing.
//!
//! One generated object supplies the manifest PRO, manifest PSSH, and initialization
//! segment PSSH, keeping their bytes consistent. Key identifiers use little-endian
//! GUID encoding: `00010203-0405-0607-0809-0a0b0c0d0e0f` becomes
//! `AwIBAAUEBwYICQoLDA0ODw==`. CBCS headers use version 4.3.0.0, `ALGID="AESCBC"`,
//! and no checksum. Generation accepts no content key. Existing license addresses
//! are carried forward from the manifest rather than substituted from encoder settings.
//!
//! Payload bytes and license addresses never appear in errors or debug output.
//! Malformed, oversized, or unsupported records are rejected instead of truncated.

use crate::pycompat::{
    b64decode, decode_utf16le, etree_get, etree_parse, etree_text, int16_u128, repr_str, strip,
    xml_line_ends,
};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use std::borrow::Cow;
use std::fmt;

/// The `PlayReady` DRM system id (`9a04f079-9840-4286-ab92-e65be0885f95`), as the 16 big-endian
/// bytes of the `pssh` box's `SystemID` field.
pub const PLAYREADY_SYSTEM_ID: [u8; 16] = [
    0x9a, 0x04, 0xf0, 0x79, 0x98, 0x40, 0x42, 0x86, 0xab, 0x92, 0xe6, 0x5b, 0xe0, 0x88, 0x5f, 0x95,
];
/// The Widevine DRM system id (`edef8ba9-79d6-4ace-a3c8-27dcd51d21ed`).
pub const WIDEVINE_SYSTEM_ID: [u8; 16] = [
    0xed, 0xef, 0x8b, 0xa9, 0x79, 0xd6, 0x4a, 0xce, 0xa3, 0xc8, 0x27, 0xdc, 0xd5, 0x1d, 0x21, 0xed,
];

/// The WRMHEADER namespace, the same from v4.0.0.0 to v4.3.0.0.
const WRM_NS: &str = "http://schemas.microsoft.com/DRM/2007/03/PlayReadyHeader";
/// The PRO record type of a WRMHEADER (spec §2.1).
const RECORD_TYPE_WRMHEADER: u16 = 0x0001;
/// A sanity bound on a parsed PRO; the spec recommends at most 15 KB.
const MAX_PRO_BYTES: usize = 64 * 1024;
/// PRO framing: a record's data length is a 16-bit field.
const MAX_RECORD_DATA_BYTES: usize = 0xFFFF;

/// Malformed or unsupported PRO input, or an invalid argument (`PlayReadyError`, a
/// `ValueError` in Python). The message describes the structural problem only — never the PRO
/// payload bytes.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct PlayReadyError(String);

impl PlayReadyError {
    fn new(message: impl Into<String>) -> Self {
        PlayReadyError(message.into())
    }

    /// The message, as Python's `str(error)`.
    pub fn message(&self) -> &str {
        &self.0
    }
}

/// Structural or decoding failure while parsing a `PlayReady` Object.
/// Error payloads contain classifications and lengths rather than raw signaling bytes.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseProError {
    /// `PlayReadyError`.
    #[error(transparent)]
    PlayReady(#[from] PlayReadyError),
    /// `UnicodeDecodeError` from decoding the WRMHEADER record (the codec's message).
    #[error("{0}")]
    UnicodeDecode(String),
}

/// The result of parsing an existing PRO (`ParsedPro`): the KID values and the `LA_URL` plus
/// metadata — never key material (a PRO carries none).
#[derive(Clone, PartialEq, Eq)]
pub struct ParsedPro {
    /// The WRMHEADER `version` attribute (`""` when absent).
    pub version: String,
    /// Every KID value, attribute form (v4.1+) or element text (v4.0.0.0), stripped.
    pub kid_values_b64: Vec<String>,
    /// The first non-blank `LA_URL`, stripped.
    pub la_url: Option<String>,
    /// The first KID's `ALGID` attribute, else the first non-blank `ALGID` element.
    pub algid: Option<String>,
    /// The record payload verbatim: the byte-exact round trip is built on these
    /// bytes, so any BOM or encoding quirk survives.
    pub wrmheader_raw: Vec<u8>,
    /// The payload decoded as UTF-16-LE (BOM kept), for assertions — never re-encoded.
    pub wrmheader_xml: String,
    /// Whether the header carries `CUSTOMATTRIBUTES` or `DS_ID`: if ever true on a real PRO,
    /// the carry-or-drop decision for v4.3.0.0 is escalated, not defaulted.
    pub has_custom_attributes: bool,
}

impl fmt::Debug for ParsedPro {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParsedPro")
            .field("version", &self.version)
            .field("kid_values_b64", &self.kid_values_b64)
            .field(
                "la_url_chars",
                &self.la_url.as_ref().map(|url| url.chars().count()),
            )
            .field("algid", &self.algid)
            .field("wrmheader_raw_bytes", &self.wrmheader_raw.len())
            .field("has_custom_attributes", &self.has_custom_attributes)
            .finish_non_exhaustive()
    }
}

/// The single source ( decision 3): every downstream consumer reads from one of these.
#[derive(Clone, PartialEq, Eq)]
pub struct PlayReadyObjects {
    /// The v4.3.0.0 WRMHEADER.
    pub wrmheader_xml: String,
    /// `build_pro(wrmheader_xml)`.
    pub pro_bytes: Vec<u8>,
    /// `build_pssh_box(PLAYREADY_SYSTEM_ID, pro_bytes)`.
    pub pssh_box: Vec<u8>,
    /// → DASH `<mspr:pro>`.
    pub pro_b64: String,
    /// → DASH `<cenc:pssh>`.
    pub pssh_b64: String,
    /// → Shaka `--pssh` (the init segment).
    pub pssh_hex: String,
}

impl fmt::Debug for PlayReadyObjects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlayReadyObjects")
            .field("pro_bytes", &self.pro_bytes.len())
            .field("pssh_box_bytes", &self.pssh_box.len())
            .finish_non_exhaustive()
    }
}

/// A KID as Python's functions accept it: a `uuid.UUID` (its 16 big-endian bytes) or text that
/// `uuid.UUID(text)` parses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kid<'a> {
    /// A parsed UUID: the 16 bytes in big-endian (RFC 4122) order.
    Uuid([u8; 16]),
    /// Text in any form `uuid.UUID` accepts (hyphenated or not, braces, `urn:uuid:`).
    Text(&'a str),
}

impl<'a> From<&'a str> for Kid<'a> {
    fn from(text: &'a str) -> Self {
        Kid::Text(text)
    }
}

impl<'a> From<&'a String> for Kid<'a> {
    fn from(text: &'a String) -> Self {
        Kid::Text(text)
    }
}

impl From<[u8; 16]> for Kid<'_> {
    fn from(bytes: [u8; 16]) -> Self {
        Kid::Uuid(bytes)
    }
}

/// A WRMHEADER as `build_pro` accepts it: text (generation, UTF-16-LE encoded without a BOM)
/// or the verbatim payload bytes (the round trip of an existing PRO).
///
/// `Debug` prints the form and its size only: the header holds the `LA_URL`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Wrmheader<'a> {
    /// A header to encode as UTF-16-LE.
    Text(&'a str),
    /// A payload wrapped verbatim.
    Bytes(&'a [u8]),
}

impl fmt::Debug for Wrmheader<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Wrmheader::Text(text) => write!(f, "Wrmheader::Text({} chars)", text.chars().count()),
            Wrmheader::Bytes(bytes) => write!(f, "Wrmheader::Bytes({} bytes)", bytes.len()),
        }
    }
}

impl<'a> From<&'a str> for Wrmheader<'a> {
    fn from(text: &'a str) -> Self {
        Wrmheader::Text(text)
    }
}

impl<'a> From<&'a String> for Wrmheader<'a> {
    fn from(text: &'a String) -> Self {
        Wrmheader::Text(text)
    }
}

impl<'a> From<&'a [u8]> for Wrmheader<'a> {
    fn from(bytes: &'a [u8]) -> Self {
        Wrmheader::Bytes(bytes)
    }
}

impl<'a> From<&'a Vec<u8>> for Wrmheader<'a> {
    fn from(bytes: &'a Vec<u8>) -> Self {
        Wrmheader::Bytes(bytes)
    }
}

// ------------------------------------------------------------------------------------------
// KID transform
// ------------------------------------------------------------------------------------------

/// `uuid.UUID(str(kid))`: `urn:` and `uuid:` removed, braces stripped from both ends, hyphens
/// removed, exactly 32 characters left, read as `int(hex, 16)` (so `0x`, `_` and padding
/// whitespace are accepted exactly as Python accepts them).
fn coerce_uuid(kid: Kid<'_>) -> Result<[u8; 16], PlayReadyError> {
    let text = match kid {
        Kid::Uuid(bytes) => return Ok(bytes),
        Kid::Text(text) => text,
    };
    let invalid = || PlayReadyError::new(format!("not a valid KID/UUID: {}", repr_str(text)));
    let hex = text.replace("urn:", "").replace("uuid:", "");
    let hex = hex.trim_matches(['{', '}']).replace('-', "");
    if hex.chars().count() != 32 {
        return Err(invalid());
    }
    int16_u128(&hex).map(u128::to_be_bytes).ok_or_else(invalid)
}

/// KID (a UUID) → base64 of the little-endian GUID: the PRO `KID VALUE`.
pub fn kid_uuid_to_playready_b64<'a>(kid: impl Into<Kid<'a>>) -> Result<String, PlayReadyError> {
    let be = coerce_uuid(kid.into())?;
    // GUID encoding reverses the first three fields (4/2/2 bytes) and preserves
    // the final eight bytes, matching uuid.UUID.bytes_le. The synthetic UUID
    // 00010203-0405-0607-0809-0a0b0c0d0e0f encodes as AwIBAAUEBwYICQoLDA0ODw==.
    let mut le = be;
    le[0..4].reverse();
    le[4..6].reverse();
    le[6..8].reverse();
    Ok(STANDARD.encode(le))
}

// ------------------------------------------------------------------------------------------
// v4.3.0.0 WRMHEADER generation (AESCBC, no CHECKSUM)
// ------------------------------------------------------------------------------------------

/// `xml.sax.saxutils.escape`: `&`, `<`, `>` (quotes untouched).
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('>', "&gt;")
        .replace('<', "&lt;")
}

/// Generate a **v4.3.0.0** WRMHEADER. Takes **no content key**.
///
/// The shape (Microsoft `PlayReady` Header Specification §3.3): `xmlns` before `version`; the
/// KID under `<DATA><PROTECTINFO><KIDS>` with its attributes in alphabetical order (`ALGID`
/// before `VALUE`); an explicit `</KID>`, never `<KID/>`; compact (no whitespace between
/// elements, no XML declaration, no BOM), so the output is deterministic.
pub fn build_wrmheader_43<'a>(
    kid: impl Into<Kid<'a>>,
    la_url: &str,
) -> Result<String, PlayReadyError> {
    // Non-emptiness only — deliberately not a URL-shape check: the LA_URL is carried forward
    // VERBATIM from the source manifest PRO (hard invariant), so re-validating it could reject
    // a legitimate source asset. Checked before the KID.
    if la_url.is_empty() {
        return Err(PlayReadyError::new("LA_URL must be non-empty"));
    }
    let value = kid_uuid_to_playready_b64(kid)?; // the base64 alphabet is attribute-safe
    // HARD INVARIANT (WRMHEADER v4.3.0.0 / ALGID AESCBC / no CHECKSUM): spec §3.3.1 / §5 forbid
    // the CHECKSUM attribute with AESCBC (it is AESCTR / COCKTAIL-only), and KEYLEN was removed
    // in v4.1 — so neither is ever written, and no content key is needed or accepted.
    Ok(format!(
        "<WRMHEADER xmlns=\"{WRM_NS}\" version=\"4.3.0.0\"><DATA><PROTECTINFO><KIDS>\
         <KID ALGID=\"AESCBC\" VALUE=\"{value}\"></KID></KIDS></PROTECTINFO>\
         <LA_URL>{}</LA_URL></DATA></WRMHEADER>",
        xml_escape(la_url)
    ))
}

// ------------------------------------------------------------------------------------------
// PRO binary wrapper
// ------------------------------------------------------------------------------------------

/// Wrap a WRMHEADER in the PRO binary framing (spec §2): LE `u32` total length
/// (self-inclusive) | LE `u16` record count (1) | LE `u16` record type (`0x0001`) | LE `u16`
/// record data length | data.
///
/// Bytes are wrapped **verbatim** (the byte-exact round trip of an existing PRO, fed
/// [`ParsedPro::wrmheader_raw`]); text is UTF-16-LE encoded first, with no BOM (as the real
/// asset's payload starts directly with `<`).
pub fn build_pro<'a>(wrmheader: impl Into<Wrmheader<'a>>) -> Result<Vec<u8>, PlayReadyError> {
    let data: Cow<'_, [u8]> = match wrmheader.into() {
        Wrmheader::Text(text) => {
            Cow::Owned(text.encode_utf16().flat_map(u16::to_le_bytes).collect())
        }
        Wrmheader::Bytes(bytes) => Cow::Borrowed(bytes),
    };
    // PRO framing: the record data length is a 16-bit field. FAIL LOUD past
    // 0xFFFF — the untrusted-input choke point, where an oversized LA_URL out of an untrusted
    // manifest lands as an oversized UTF-16-LE WRMHEADER. The message carries a length only.
    let Ok(length) = u16::try_from(data.len()) else {
        return Err(PlayReadyError::new(format!(
            "WRMHEADER record is {} bytes; exceeds the PRO 16-bit record-length field maximum \
             ({MAX_RECORD_DATA_BYTES} bytes)",
            data.len()
        )));
    };
    let total = u32::from(length) + 10;
    let mut pro = Vec::with_capacity(data.len() + 10);
    pro.extend(total.to_le_bytes());
    pro.extend(1u16.to_le_bytes()); // exactly one WRMHEADER record
    pro.extend(RECORD_TYPE_WRMHEADER.to_le_bytes());
    pro.extend(length.to_le_bytes());
    pro.extend_from_slice(&data);
    Ok(pro)
}

// ------------------------------------------------------------------------------------------
// pssh box wrapper
// ------------------------------------------------------------------------------------------

/// An ISO-BMFF `pssh` box, **version 0** (no per-KID list) — the form the source presentation uses and
/// the form Shaka writes from `--pssh`: size (4, BE) | `pssh` | version 0 + flags 0 |
/// system id (16) | data size (4, BE) | data. Round-trips the existing `PlayReady` pssh box
/// byte-exactly.
pub fn build_pssh_box(system_id: &[u8], data: &[u8]) -> Result<Vec<u8>, PlayReadyError> {
    if system_id.len() != 16 {
        return Err(PlayReadyError::new(format!(
            "system_id must be 16 bytes, got {}",
            system_id.len()
        )));
    }
    // Python's `struct.pack(">I",...)` raises `struct.error` (not a `PlayReadyError`) past
    // 32 bits; a 4 GiB pssh is not a real input, so the same text comes back as this type.
    let too_big = || PlayReadyError::new("'I' format requires 0 <= number <= 4294967295");
    let data_size = u32::try_from(data.len()).map_err(|_| too_big())?;
    let box_size = u32::try_from(data.len() + 32).map_err(|_| too_big())?;
    let mut pssh = Vec::with_capacity(data.len() + 32);
    pssh.extend(box_size.to_be_bytes());
    pssh.extend(b"pssh");
    pssh.extend([0, 0, 0, 0]);
    pssh.extend_from_slice(system_id);
    pssh.extend(data_size.to_be_bytes());
    pssh.extend_from_slice(data);
    Ok(pssh)
}

// ------------------------------------------------------------------------------------------
// The single-source entry
// ------------------------------------------------------------------------------------------

/// The single-source entry ( decision 3) that the DASH manifest patch and the `--pssh`
/// init path both consume: one call, one set of bytes in every representation, so the
/// manifest `<cenc:pssh>` / `<mspr:pro>` and the init-segment pssh are identical by
/// construction. Takes **no content key**.
pub fn build_playready_v43<'a>(
    kid: impl Into<Kid<'a>>,
    la_url: &str,
) -> Result<PlayReadyObjects, PlayReadyError> {
    let wrmheader_xml = build_wrmheader_43(kid, la_url)?;
    let pro_bytes = build_pro(wrmheader_xml.as_str())?;
    let pssh_box = build_pssh_box(&PLAYREADY_SYSTEM_ID, &pro_bytes)?;
    Ok(PlayReadyObjects {
        pro_b64: STANDARD.encode(&pro_bytes),
        pssh_b64: STANDARD.encode(&pssh_box),
        pssh_hex: hex::encode(&pssh_box),
        wrmheader_xml,
        pro_bytes,
        pssh_box,
    })
}

// ------------------------------------------------------------------------------------------
// Parser (read-only; recovers KID + LA_URL only; FAIL-loud)
// ------------------------------------------------------------------------------------------

fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

/// Parse an existing PRO (base64) into its KID values, `LA_URL` and metadata. Read-only.
///
/// FAILS LOUD on anything it cannot represent faithfully — a multi-record PRO, a
/// non-WRMHEADER record (an Embedded License Store), a truncated record, a length that
/// disagrees with the payload — rather than silently dropping a record a license server may
/// depend on. It never invents or drops data.
pub fn parse_pro(pro_b64: &str) -> Result<ParsedPro, ParseProError> {
    let fail = |message: String| ParseProError::PlayReady(PlayReadyError::new(message));
    let raw = b64decode(pro_b64, true).map_err(|_| fail("PRO is not valid base64".into()))?;
    if raw.len() < 6 {
        return Err(fail(
            "PRO too short to contain a length + record count".into(),
        ));
    }
    if raw.len() > MAX_PRO_BYTES {
        return Err(fail(format!(
            "PRO exceeds sane bound ({} bytes)",
            raw.len()
        )));
    }
    let declared = u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]);
    if usize::try_from(declared).ok() != Some(raw.len()) {
        // Trailing bytes or truncation: corrupt, or something not modelled. Escalate.
        return Err(fail(format!(
            "PRO length field {declared} != actual {} bytes (unsupported/corrupt)",
            raw.len()
        )));
    }
    let records = u16_at(&raw, 4);
    if records != 1 {
        // A multi-record (or empty) PRO may carry data (an ELS) a license server needs;
        // regenerating from a WRMHEADER alone would drop it silently. Escalate.
        return Err(fail(format!(
            "PRO has {records} records; only a single WRMHEADER record is supported \
             (multi-record requires escalation, never a silent drop)"
        )));
    }
    if raw.len() < 10 {
        return Err(fail("PRO record header truncated".into()));
    }
    let (kind, length) = (u16_at(&raw, 6), usize::from(u16_at(&raw, 8)));
    if kind != RECORD_TYPE_WRMHEADER {
        return Err(fail(format!(
            "unsupported PRO record type 0x{kind:04x} (only 0x0001 WRMHEADER supported)"
        )));
    }
    if 10 + length != raw.len() {
        return Err(fail(
            "PRO record length does not span the remaining bytes".into(),
        ));
    }
    let wrmheader_raw = raw[10..].to_vec();
    let wrmheader_xml = decode_utf16le(&wrmheader_raw).map_err(ParseProError::UnicodeDecode)?;
    let fields = parse_wrmheader(&wrmheader_xml)?;
    Ok(ParsedPro {
        version: fields.version,
        kid_values_b64: fields.kid_values_b64,
        la_url: fields.la_url,
        algid: fields.algid,
        wrmheader_raw,
        wrmheader_xml,
        has_custom_attributes: fields.has_custom_attributes,
    })
}

struct HeaderFields {
    version: String,
    kid_values_b64: Vec<String>,
    la_url: Option<String>,
    algid: Option<String>,
    has_custom_attributes: bool,
}

/// The fields of a decoded WRMHEADER, read as `xml.etree.ElementTree` reads them: elements
/// matched by local name (robust to the default namespace and to `xmlns=""`), `.text` being
/// the text before the first child element.
fn parse_wrmheader(xml: &str) -> Result<HeaderFields, PlayReadyError> {
    // A UTF-16 BOM decodes to leading U+FEFF: kept in `wrmheader_xml`, stripped for the parse.
    let for_parse = xml.trim_start_matches('\u{feff}');
    // SECURITY: the WRMHEADER arrived base64-embedded in an UNTRUSTED manifest. Reject a
    // DOCTYPE before parsing (the entity-expansion "billion laughs" guard, as `config`'s MPD
    // guard), scanning the WHOLE payload so a DOCTYPE behind a large comment cannot slip past.
    if for_parse.contains("<!DOCTYPE") {
        return Err(PlayReadyError::new(
            "DOCTYPE is not permitted in a WRMHEADER (entity-expansion DoS guard)",
        ));
    }
    // Parsed as `ET.fromstring` parses it: line ends normalized first, expat's well-formedness
    // checks, any nesting depth up to the supported one (pycompat::etree_parse).
    let normalized = xml_line_ends(for_parse);
    let document = etree_parse(&normalized).map_err(|error| {
        PlayReadyError::new(format!("WRMHEADER is not well-formed XML: {error}"))
    })?;
    let header = document.root_element();
    let elements: Vec<roxmltree::Node<'_, '_>> = header
        .descendants()
        .filter(roxmltree::Node::is_element)
        .collect();
    let named = |local: &'static str| {
        elements
            .iter()
            .filter(move |element| element.tag_name().name() == local)
    };
    let mut kid_values_b64 = Vec::new();
    let mut algids = Vec::new();
    for kid in named("KID") {
        if let Some(value) = etree_get(*kid, "VALUE") {
            // v4.1+ attribute form: <KID ALGID=".." VALUE=".."></KID>
            kid_values_b64.push(strip(value).to_string());
            if let Some(algid) = etree_get(*kid, "ALGID").filter(|algid| !algid.is_empty()) {
                algids.push(strip(algid).to_string());
            }
        } else if let Some(text) = element_text(kid) {
            // v4.0.0.0 element-text form: <KID>..</KID>
            kid_values_b64.push(text);
        }
    }
    // v4.0.0.0 carries the algorithm as <PROTECTINFO><ALGID>..</ALGID>.
    let algid = algids
        .into_iter()
        .next()
        .or_else(|| named("ALGID").find_map(|element| element_text(element)));
    let la_url = named("LA_URL").find_map(|element| element_text(element));
    let has_custom_attributes =
        named("CUSTOMATTRIBUTES").next().is_some() || named("DS_ID").next().is_some();
    Ok(HeaderFields {
        version: etree_get(header, "version").unwrap_or("").to_string(),
        kid_values_b64,
        la_url,
        algid,
        has_custom_attributes,
    })
}

/// `element.text.strip` when that is not empty.
fn element_text(element: &roxmltree::Node<'_, '_>) -> Option<String> {
    let text = etree_text(*element);
    let stripped = strip(&text);
    (!stripped.is_empty()).then(|| stripped.to_string())
}
