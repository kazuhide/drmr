//! `PlayReady` record parsing, generated signaling bytes, and malformed input handling.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use drmr_core::playready::{
    Kid, PLAYREADY_SYSTEM_ID, ParseProError, ParsedPro, PlayReadyError, PlayReadyObjects,
    WIDEVINE_SYSTEM_ID, Wrmheader, build_playready_v43, build_pro, build_pssh_box,
    build_wrmheader_43, kid_uuid_to_playready_b64, parse_pro,
};
use serde_json::Value;
use std::path::PathBuf;

const KID: &str = "00010203-0405-0607-0809-0a0b0c0d0e0f";
const KID_VALUE: &str = "AwIBAAUEBwYICQoLDA0ODw==";
const LA_URL: &str = "https://license.example.invalid/rightsmanager.asmx";
const WRM_NS: &str = "http://schemas.microsoft.com/DRM/2007/03/PlayReadyHeader";

fn oracle() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/playready.oracle.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn text(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("not a string: {value}"))
}

fn b64(data: &[u8]) -> String {
    STANDARD.encode(data)
}

fn le16(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}

fn be32(data: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn le32(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]])
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A raw PRO from (record type, data) pairs: the test-only forge for multi-record and
/// unknown-record PROs that `parse_pro` must reject (`_make_pro_b64`).
fn make_pro_b64(records: &[(u16, &[u8])]) -> String {
    let mut body = u16::try_from(records.len()).unwrap().to_le_bytes().to_vec();
    for (kind, data) in records {
        body.extend(kind.to_le_bytes());
        body.extend(u16::try_from(data.len()).unwrap().to_le_bytes());
        body.extend(*data);
    }
    let mut pro = u32::try_from(4 + body.len())
        .unwrap()
        .to_le_bytes()
        .to_vec();
    pro.extend(body);
    b64(&pro)
}

fn wrap(xml: &str) -> String {
    b64(&build_pro(xml).unwrap())
}

/// A parse result in the reference fixture's JSON shape.
fn parsed_json(parsed: &ParsedPro) -> Value {
    serde_json::json!({
        "version": parsed.version,
        "kid_values_b64": parsed.kid_values_b64,
        "la_url": parsed.la_url,
        "algid": parsed.algid,
        "wrmheader_raw_hex": hex::encode(&parsed.wrmheader_raw),
        "wrmheader_xml": parsed.wrmheader_xml,
        "has_custom_attributes": parsed.has_custom_attributes,
    })
}

fn objects_json(objects: &PlayReadyObjects) -> Value {
    serde_json::json!({
        "wrmheader_xml": objects.wrmheader_xml,
        "pro_hex": hex::encode(&objects.pro_bytes),
        "pssh_box_hex": hex::encode(&objects.pssh_box),
        "pro_b64": objects.pro_b64,
        "pssh_b64": objects.pssh_b64,
        "pssh_hex": objects.pssh_hex,
    })
}

// --- system id constants -----------------------------------------------------------------

#[test]
fn system_id_constants() {
    let oracle = oracle();
    assert_eq!(
        hex::encode(PLAYREADY_SYSTEM_ID),
        text(&oracle["constants"]["playready_system_id"])
    );
    assert_eq!(
        hex::encode(WIDEVINE_SYSTEM_ID),
        text(&oracle["constants"]["widevine_system_id"])
    );
    assert_eq!(PLAYREADY_SYSTEM_ID.len(), 16);
    assert_eq!(WIDEVINE_SYSTEM_ID.len(), 16);
}

// ---  --------------------------------------------------------------------------

#[test]
fn kid_transform_matches_the_worked_example() {
    // Invariant: KID VALUE = base64 of the little-endian (mixed-endian) GUID (synthetic KID; the
    // The same mixed-endian GUID transform applies to every valid UUID.
    assert_eq!(kid_uuid_to_playready_b64(KID).unwrap(), KID_VALUE);
}

#[test]
fn kid_transform_accepts_uuid_bytes_and_hyphenless() {
    let bytes: [u8; 16] = hex::decode(KID.replace('-', ""))
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(kid_uuid_to_playready_b64(bytes).unwrap(), KID_VALUE);
    assert_eq!(
        kid_uuid_to_playready_b64(KID.replace('-', "").as_str()).unwrap(),
        KID_VALUE
    );
}

#[test]
fn kid_transform_is_the_mixed_endian_swap() {
    let be = hex::decode(KID.replace('-', "")).unwrap();
    let mut mixed = vec![be[3], be[2], be[1], be[0], be[5], be[4], be[7], be[6]];
    mixed.extend(&be[8..]);
    let value = kid_uuid_to_playready_b64(KID).unwrap();
    assert_eq!(STANDARD.decode(value).unwrap(), mixed);
}

#[test]
fn kid_text_forms_parse_as_uuid_uuid_parses_them() {
    // Every spelling `uuid.UUID(str(kid))` accepts or rejects, as Python treats it (braces,
    // `urn:uuid:`, `0x`, `_`, padding whitespace, full-width digits), with its message.
    let oracle = oracle();
    for case in oracle["kid_forms"].as_array().unwrap() {
        let outcome = &case[1];
        let expected = match outcome.get("ok") {
            Some(value) => Ok(text(value).to_string()),
            None => Err(text(&outcome["error"]).to_string()),
        };
        let actual = kid_uuid_to_playready_b64(text(&case[0])).map_err(|error| error.to_string());
        assert_eq!(actual, expected, "{case}");
    }
}

// --- build_pssh_box (supports ) -------------------------------------------------

#[test]
fn build_pssh_box_structure_is_version0() {
    let data = [0xde, 0xad, 0xbe, 0xef].repeat(4);
    let pssh = build_pssh_box(&PLAYREADY_SYSTEM_ID, &data).unwrap();
    assert_eq!(be32(&pssh, 0) as usize, pssh.len());
    assert_eq!(&pssh[4..8], b"pssh");
    assert_eq!(
        &pssh[8..12],
        [0, 0, 0, 0],
        "version 0 + flags 0 (no per-KID list)"
    );
    assert_eq!(&pssh[12..28], PLAYREADY_SYSTEM_ID);
    assert_eq!(be32(&pssh, 28) as usize, data.len());
    assert_eq!(&pssh[32..], data.as_slice());
    assert_eq!(pssh.len(), 32 + data.len());
    let oracle = oracle();
    assert_eq!(
        hex::encode(build_pssh_box(&WIDEVINE_SYSTEM_ID, &[]).unwrap()),
        text(&oracle["pssh_box"]["empty"])
    );
}

#[test]
fn build_pssh_box_rejects_bad_system_id() {
    let oracle = oracle();
    let error = build_pssh_box(&[0u8; 15], b"data").unwrap_err();
    assert_eq!(
        error.to_string(),
        text(&oracle["pssh_box"]["bad_system_id"]["error"])
    );
}

// --- build_pro framing (supports ) ----------------------------------------------

#[test]
fn build_pro_str_path_utf16le_single_record() {
    let pro = build_pro("<X></X>").unwrap();
    assert_eq!(le32(&pro, 0) as usize, pro.len());
    assert_eq!(le16(&pro, 4), 1, "record count");
    assert_eq!(le16(&pro, 6), 0x0001, "WRMHEADER record type");
    let payload = &pro[10..];
    assert_eq!(usize::from(le16(&pro, 8)), payload.len());
    assert_eq!(payload, utf16le("<X></X>").as_slice());
    assert_eq!(&payload[..2], b"\x3c\x00", "starts with '<': no UTF-16 BOM");
    let oracle = oracle();
    assert_eq!(hex::encode(&pro), text(&oracle["build_pro"]["text_x"]));
    assert_eq!(
        hex::encode(build_pro("<X>\u{1F600}</X>").unwrap()),
        text(&oracle["build_pro"]["text_non_bmp"]),
        "a character beyond the BMP is a surrogate pair"
    );
}

#[test]
fn build_pro_bytes_path_is_verbatim() {
    let raw = utf16le("<WRMHEADER></WRMHEADER>");
    let pro = build_pro(raw.as_slice()).unwrap();
    assert_eq!(
        &pro[10..],
        raw.as_slice(),
        "bytes wrapped verbatim, no re-encode"
    );
}

// The PRO per-record data length is a 16-bit field: a record past 0xFFFF bytes must FAIL LOUD
// as a PlayReadyError (in Python: not a raw struct.error, which is no ValueError).
const OVER_LIMIT_BYTES: usize = 0xFFFF + 1;

#[test]
fn build_pro_oversized_record_is_a_playready_error() {
    let oracle = oracle();
    let error = build_pro(vec![0u8; OVER_LIMIT_BYTES].as_slice()).unwrap_err();
    assert!(error.to_string().contains("record-length"));
    assert_eq!(
        error.to_string(),
        text(&oracle["build_pro"]["over_bytes"]["error"])
    );
    let error = build_pro("A".repeat(40_000).as_str()).unwrap_err();
    assert_eq!(
        error.to_string(),
        text(&oracle["build_pro"]["over_text"]["error"])
    );
}

#[test]
fn build_pro_oversized_record_honors_the_error_contract() {
    // Python's "PlayReadyError subclasses ValueError" contract: here the oversize is an
    // `Err(PlayReadyError)`, never a panic.
    let result: Result<Vec<u8>, PlayReadyError> = build_pro(vec![0u8; OVER_LIMIT_BYTES].as_slice());
    assert!(result.is_err());
}

#[test]
fn build_pro_accepts_record_at_exact_limit() {
    let at_limit = vec![0u8; 0xFFFF];
    let pro = build_pro(at_limit.as_slice()).unwrap();
    assert_eq!((le16(&pro, 6), le16(&pro, 8)), (0x0001, 0xFFFF));
    assert_eq!(&pro[10..], at_limit.as_slice());
}

#[test]
fn build_playready_v43_oversized_la_url_is_a_playready_error() {
    let oracle = oracle();
    let huge = format!("https://license.example.invalid/{}", "a".repeat(40_000));
    let error = build_playready_v43(KID, &huge).unwrap_err();
    assert_eq!(
        error.to_string(),
        text(&oracle["build_pro"]["over_la_url"]["error"])
    );
    let header = build_wrmheader_43(KID, &huge).unwrap();
    assert!(
        build_pro(header.as_str())
            .unwrap_err()
            .to_string()
            .contains("record-length")
    );
}

#[test]
fn build_pro_normal_wrmheader_still_round_trips_after_guard() {
    let xml = build_wrmheader_43(KID, LA_URL).unwrap();
    let pro = build_pro(xml.as_str()).unwrap();
    let parsed = parse_pro(&b64(&pro)).unwrap();
    assert_eq!(parsed.version, "4.3.0.0");
    assert_eq!(parsed.algid.as_deref(), Some("AESCBC"));
    assert_eq!(parsed.la_url.as_deref(), Some(LA_URL));
    assert_eq!(parsed.kid_values_b64, [KID_VALUE]);
    assert_eq!(build_pro(parsed.wrmheader_raw.as_slice()).unwrap(), pro);
}

#[test]
fn build_wrmheader_43_rejects_empty_la_url() {
    let error = build_wrmheader_43(KID, "").unwrap_err();
    assert_eq!(error.to_string(), "LA_URL must be non-empty");
}

#[test]
fn build_wrmheader_43_matches_python_byte_for_byte() {
    let oracle = oracle();
    for case in oracle["wrmheader_43"].as_array().unwrap() {
        let outcome = &case[2];
        let expected = match outcome.get("ok") {
            Some(value) => Ok(text(value).to_string()),
            None => Err(text(&outcome["error"]).to_string()),
        };
        let actual =
            build_wrmheader_43(text(&case[0]), text(&case[1])).map_err(|error| error.to_string());
        assert_eq!(actual, expected, "{case}");
    }
}

// ---  ----------------------------------------------------------------------

#[test]
fn v43_generated_header_shape_and_self_round_trip() {
    let xml = build_wrmheader_43(KID, LA_URL).unwrap();
    assert!(xml.contains("version=\"4.3.0.0\""));
    assert!(xml.contains(&format!("xmlns=\"{WRM_NS}\"")));
    assert!(xml.contains("<KIDS>") && xml.contains("</KIDS>"));
    assert!(xml.contains("</KID>"), "explicit closing tag, never <KID/>");
    assert!(!xml.contains("<KID/>") && !xml.contains("/>"));
    assert!(xml.contains("ALGID=\"AESCBC\""));
    assert!(xml.contains(&format!("VALUE=\"{KID_VALUE}\"")));
    assert!(
        xml.find("ALGID=\"AESCBC\"").unwrap() < xml.find("VALUE=\"").unwrap(),
        "attributes alphabetical: ALGID before VALUE"
    );
    assert!(!xml.contains("CHECKSUM"), "AESCBC forbids CHECKSUM");
    assert!(xml.contains("<LA_URL>"));

    let parsed = parse_pro(&wrap(&xml)).unwrap();
    assert_eq!(parsed.version, "4.3.0.0");
    assert_eq!(parsed.algid.as_deref(), Some("AESCBC"));
    assert_eq!(parsed.kid_values_b64, [KID_VALUE]);
    assert_eq!(parsed.la_url.as_deref(), Some(LA_URL), "LA_URL verbatim");
    assert!(!parsed.wrmheader_xml.contains("CHECKSUM"));
    assert!(!parsed.has_custom_attributes);
}

#[test]
fn v43_la_url_is_xml_escaped_and_round_trips() {
    let la_url = format!("{LA_URL}?a=1&b=2&c=<x>");
    let xml = build_wrmheader_43(KID, &la_url).unwrap();
    assert!(xml.contains("&amp;") && xml.contains("&lt;"));
    let parsed = parse_pro(&wrap(&xml)).unwrap();
    assert_eq!(parsed.la_url.as_deref(), Some(la_url.as_str()));
}

#[test]
fn build_playready_v43_bundle_is_internally_consistent() {
    let objects = build_playready_v43(KID, LA_URL).unwrap();
    assert_eq!(
        objects.pro_bytes,
        build_pro(objects.wrmheader_xml.as_str()).unwrap()
    );
    assert_eq!(
        objects.pssh_box,
        build_pssh_box(&PLAYREADY_SYSTEM_ID, &objects.pro_bytes).unwrap()
    );
    assert_eq!(objects.pro_b64, b64(&objects.pro_bytes));
    assert_eq!(objects.pssh_b64, b64(&objects.pssh_box));
    assert_eq!(objects.pssh_hex, hex::encode(&objects.pssh_box));
    assert_eq!(
        hex::decode(&objects.pssh_hex).unwrap(),
        STANDARD.decode(&objects.pssh_b64).unwrap(),
        "the --pssh init path and the manifest carry the same box"
    );
}

#[test]
fn ctr_header_checksum_uses_guid_byte_order_and_the_content_key() {
    // FIPS 197 AES-128 example: plaintext 001122...eeff encrypts to
    // 69c4e0d86a7b0430d8cdb78070b4c55a with key 000102...0f.
    let kid = "33221100-5544-7766-8899-aabbccddeeff";
    let key: [u8; 16] = std::array::from_fn(|index| u8::try_from(index).unwrap());
    let objects = drmr_core::playready::build_playready_v41(kid, LA_URL, &key).unwrap();
    let document = roxmltree::Document::parse(&objects.wrmheader_xml).unwrap();
    assert_eq!(
        document.root_element().attribute("version"),
        Some("4.1.0.0")
    );
    let node = document
        .descendants()
        .find(|node| {
            node.has_tag_name((
                "http://schemas.microsoft.com/DRM/2007/03/PlayReadyHeader",
                "KID",
            ))
        })
        .unwrap();
    assert_eq!(node.attribute("ALGID"), Some("AESCTR"));
    assert_eq!(node.attribute("CHECKSUM"), Some("acTg2Gp7BDA="));
    assert_eq!(node.attribute("VALUE"), Some("ABEiM0RVZneImaq7zN3u/w=="));
    assert_eq!(node.parent().unwrap().tag_name().name(), "PROTECTINFO");
    let parsed = parse_pro(&objects.pro_b64).unwrap();
    assert_eq!(parsed.version, "4.1.0.0");
    assert_eq!(parsed.algid.as_deref(), Some("AESCTR"));
    assert_eq!(parsed.la_url.as_deref(), Some(LA_URL));
    assert_eq!(
        objects.pro_bytes,
        build_pro(objects.wrmheader_xml.as_str()).unwrap()
    );
    assert_eq!(
        objects.pssh_box,
        build_pssh_box(&PLAYREADY_SYSTEM_ID, &objects.pro_bytes).unwrap()
    );
    let changed = drmr_core::playready::build_playready_v41(kid, LA_URL, &[0; 16]).unwrap();
    assert_ne!(objects.wrmheader_xml, changed.wrmheader_xml);
    assert!(drmr_core::playready::build_playready_v41(kid, "", &key).is_err());
}

#[test]
fn build_playready_v43_matches_python_byte_for_byte() {
    // One generation supplies identical signaling bytes to every representation.
    let oracle = oracle();
    let objects = build_playready_v43(KID, LA_URL).unwrap();
    assert_eq!(objects_json(&objects), oracle["objects"]);
}

// ---  (external anchor: the public Microsoft spec example) -----------------------

#[test]
fn v43_parses_public_microsoft_spec_example() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/playready/wrmheader_v4_3_0_0_aescbc_example.xml");
    let xml = std::fs::read_to_string(path).unwrap();
    let parsed = parse_pro(&wrap(&xml)).unwrap();
    assert_eq!(parsed.version, "4.3.0.0");
    assert_eq!(parsed.algid.as_deref(), Some("AESCBC"));
    assert_eq!(
        parsed.kid_values_b64,
        ["PV1LM/VEVk+kEOB8qqcWDg==", "tuhDoKUN7EyxDPtMRNmhyA=="]
    );
    assert_eq!(
        parsed.la_url.as_deref(),
        Some("http://rm.contoso.com/rightsmanager.asmx")
    );
    assert!(parsed.has_custom_attributes, "DS_ID present");
    assert_eq!(
        parsed_json(&parsed),
        oracle()["parse_xml"]["microsoft-example"]["ok"]
    );
}

// ---  ------------------------------------------------------------------------

fn parse_error(pro_b64: &str) -> String {
    match parse_pro(pro_b64) {
        Err(ParseProError::PlayReady(error)) => error.to_string(),
        other => panic!("expected a PlayReadyError, got {other:?}"),
    }
}

#[test]
fn parse_pro_fails_loud_on_multi_record() {
    let wrm = utf16le(&build_wrmheader_43(KID, LA_URL).unwrap());
    parse_error(&make_pro_b64(&[(1, &wrm), (1, &wrm)]));
}

#[test]
fn parse_pro_fails_loud_on_unknown_record_type() {
    parse_error(&make_pro_b64(&[(3, b"embedded-license-store")]));
}

#[test]
fn parse_pro_fails_loud_on_zero_records() {
    parse_error(&make_pro_b64(&[]));
}

#[test]
fn parse_pro_fails_loud_on_length_mismatch() {
    let wrm = utf16le(&build_wrmheader_43(KID, LA_URL).unwrap());
    let mut raw = STANDARD.decode(make_pro_b64(&[(1, &wrm)])).unwrap();
    raw.extend([0, 0]);
    parse_error(&b64(&raw));
}

#[test]
fn parse_pro_custom_attributes_flag_false_when_absent() {
    let parsed = parse_pro(&wrap(&build_wrmheader_43(KID, LA_URL).unwrap())).unwrap();
    assert!(!parsed.has_custom_attributes);
}

const WRM_BODY: &str = concat!(
    "<WRMHEADER xmlns=\"http://schemas.microsoft.com/DRM/2007/03/PlayReadyHeader\"",
    " version=\"4.3.0.0\"><DATA><PROTECTINFO><KIDS>",
    "<KID ALGID=\"AESCBC\" VALUE=\"AwIBAAUEBwYICQoLDA0ODw==\"></KID></KIDS></PROTECTINFO>",
    "<LA_URL>https://license.example.invalid/rightsmanager.asmx</LA_URL></DATA></WRMHEADER>"
);

#[test]
fn parse_pro_custom_attributes_flag_true_on_customattributes() {
    let xml = WRM_BODY.replace(
        "</DATA>",
        "<CUSTOMATTRIBUTES><Foo>bar</Foo></CUSTOMATTRIBUTES></DATA>",
    );
    assert!(parse_pro(&wrap(&xml)).unwrap().has_custom_attributes);
}

// --- : the XML entity-expansion (billion laughs) guard -------------------------

#[test]
fn parse_pro_rejects_doctype() {
    let malicious = format!("<!DOCTYPE WRMHEADER [<!ENTITY lol \"lol\">]>{WRM_BODY}");
    assert!(parse_error(&wrap(&malicious)).contains("DOCTYPE"));
}

#[test]
fn parse_pro_rejects_doctype_past_a_large_leading_comment() {
    let malicious = format!(
        "<!--{}--><!DOCTYPE WRMHEADER [<!ENTITY lol \"lol\">]>{WRM_BODY}",
        "A".repeat(8192)
    );
    assert!(parse_error(&wrap(&malicious)).contains("DOCTYPE"));
}

#[test]
fn parse_pro_matches_python_on_every_parse_case() {
    let oracle = oracle();
    for (name, outcome) in oracle["parse_xml"].as_object().unwrap() {
        let xml = match name.as_str() {
            "microsoft-example" => continue, // its own test above
            _ => text(&outcome["ok"]["wrmheader_xml"]).to_string(),
        };
        let parsed = parse_pro(&wrap(&xml)).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(parsed_json(&parsed), outcome["ok"], "{name}");
    }
}

#[test]
fn parse_pro_errors_carry_python_messages() {
    // The expat detail of a malformed-XML message is approximated from the Rust parser's
    // position (documented divergence); every other message is Python's, and the codec errors
    // stay apart from PlayReadyError as they do in Python (`UnicodeDecodeError`).
    let oracle = oracle();
    for (name, case) in oracle["parse_errors"].as_object().unwrap() {
        let expected = text(&case["error"]);
        let actual = parse_pro(text(&case["input"])).unwrap_err();
        match text(&case["type"]) {
            "UnicodeDecodeError" => {
                assert_eq!(
                    actual,
                    ParseProError::UnicodeDecode(expected.to_string()),
                    "{name}"
                );
            }
            "PlayReadyError" => {
                let ParseProError::PlayReady(error) = &actual else {
                    panic!("{name}: {actual:?}");
                };
                assert_eq!(error.to_string(), expected, "{name}");
            }
            other => panic!("{name}: unexpected oracle type {other}"),
        }
    }
}

// ---  ------------------------------------------------------------------------

#[test]
fn cbcs_generation_functions_have_no_key_input_path() {
    // Python inspects the parameter names; here the signatures themselves are pinned: the
    // generators take a KID and an LA_URL (or framed bytes), never a content key. Adding a
    // parameter breaks this test at compile time.
    type Generated<T> = Result<T, PlayReadyError>;
    let _: fn(Kid<'static>, &'static str) -> Generated<String> = build_wrmheader_43;
    let _: fn(Kid<'static>, &'static str) -> Generated<PlayReadyObjects> = build_playready_v43;
    let _: fn(Kid<'static>) -> Generated<String> = kid_uuid_to_playready_b64;
    let _: fn(Wrmheader<'static>) -> Generated<Vec<u8>> = build_pro;
    let _: fn(&'static [u8], &'static [u8]) -> Generated<Vec<u8>> = build_pssh_box;
}

#[test]
fn parsed_pro_exposes_no_content_key_field() {
    // The parser recovers KID + LA_URL only. The exhaustive pattern pins the field set: a new
    // field (say, a key) breaks this test at compile time.
    let parsed = parse_pro(&wrap(WRM_BODY)).unwrap();
    let ParsedPro {
        version,
        kid_values_b64,
        la_url,
        algid,
        wrmheader_raw,
        wrmheader_xml,
        has_custom_attributes,
    } = parsed;
    assert_eq!(
        (version.as_str(), kid_values_b64.len(), la_url.is_some()),
        ("4.3.0.0", 1, true)
    );
    assert!(algid.is_some() && !wrmheader_raw.is_empty() && !wrmheader_xml.is_empty());
    assert!(!has_custom_attributes);
}

// ---  --------------------------------------------------------------------------

#[test]
fn generation_is_deterministic() {
    let first = build_playready_v43(KID, LA_URL).unwrap();
    let second = build_playready_v43(KID, LA_URL).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.pro_bytes, second.pro_bytes);
    assert_eq!(first.pssh_box, second.pssh_box);
    assert_eq!(first.pssh_hex, second.pssh_hex);
    assert_eq!(first.wrmheader_xml, second.wrmheader_xml);
}

// --- synthetic v4.0.0.0 PRO parsing and regeneration --------------------------------------

#[test]
fn p2_pr_rt_01_v40_pro_byte_exact_round_trip() {
    // build_pro(parse_pro(src).wrmheader_raw) reproduces the source PRO exactly -- built on the
    // verbatim payload BYTES, so a BOM survives too.
    let oracle = oracle();
    for key in ["pro_b64", "bom_pro_b64"] {
        let source_b64 = text(&oracle["v40"][key]);
        let source = STANDARD.decode(source_b64).unwrap();
        let parsed = parse_pro(source_b64).unwrap();
        assert_eq!(parsed.version, "4.0.0.0");
        let rebuilt = build_pro(parsed.wrmheader_raw.as_slice()).unwrap();
        assert_eq!(rebuilt, source, "{key}");
        assert_eq!(b64(&rebuilt), source_b64, "{key}");
    }
    assert_eq!(
        parsed_json(&parse_pro(text(&oracle["v40"]["pro_b64"])).unwrap()),
        oracle["v40"]["parsed"]
    );
    assert_eq!(
        parsed_json(&parse_pro(text(&oracle["v40"]["bom_pro_b64"])).unwrap()),
        oracle["v40"]["bom_parsed"]
    );
}

#[test]
fn p2_pr_rt_02_playready_pssh_byte_exact_round_trip() {
    let oracle = oracle();
    let source_pssh = STANDARD.decode(text(&oracle["v40"]["pssh_b64"])).unwrap();
    let source_pro = STANDARD.decode(text(&oracle["v40"]["pro_b64"])).unwrap();
    assert_eq!(
        build_pssh_box(&PLAYREADY_SYSTEM_ID, &source_pro).unwrap(),
        source_pssh
    );
}

#[test]
fn p2_pr_kid_01_transform_against_the_default_kid() {
    let oracle = oracle();
    let parsed = parse_pro(text(&oracle["v40"]["pro_b64"])).unwrap();
    assert_eq!(
        kid_uuid_to_playready_b64(KID).unwrap(),
        parsed.kid_values_b64[0]
    );
}

#[test]
fn p2_pr_laurl_01_la_url_parsed_from_source_and_carried_forward() {
    // Invariant: the LA_URL comes from THIS PRO and is carried into v4.3.0.0 verbatim.
    let oracle = oracle();
    let parsed = parse_pro(text(&oracle["v40"]["pro_b64"])).unwrap();
    let la_url = parsed.la_url.unwrap();
    assert!(la_url.starts_with("https://"));
    let regenerated = build_playready_v43(KID, &la_url).unwrap();
    let reparsed = parse_pro(&regenerated.pro_b64).unwrap();
    assert_eq!(reparsed.la_url.as_deref(), Some(la_url.as_str()));
    assert_eq!(reparsed.version, "4.3.0.0");
    assert_eq!(reparsed.algid.as_deref(), Some("AESCBC"));
    assert!(!reparsed.wrmheader_xml.contains("CHECKSUM"));
    assert_eq!(objects_json(&regenerated), oracle["v40"]["regenerated"]);
}

// --- secret hygiene of the Rust types -----------------------------------------------------------

#[test]
fn debug_output_never_carries_the_la_url() {
    // A clear-HLS LA_URL is an operator secret : no `{:?}` may print it, in any form
    // the objects hold it (text, UTF-16LE inside the PRO, base64, hex).
    let objects = build_playready_v43(KID, LA_URL).unwrap();
    let parsed = parse_pro(&objects.pro_b64).unwrap();
    let header = build_wrmheader_43(KID, LA_URL).unwrap();
    let bytes = utf16le(&header);
    let shown = format!(
        "{objects:?} {parsed:?} {:?} {:?}",
        Wrmheader::from(header.as_str()),
        Wrmheader::from(bytes.as_slice())
    );
    assert!(!shown.contains("license.example.invalid"), "{shown}");
    assert!(!shown.contains(&objects.pro_b64) && !shown.contains(&objects.pssh_hex));
    assert!(!shown.contains(&hex::encode(utf16le("license"))));
}
