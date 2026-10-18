//! The byte-level MP4 readers (`mp4`): the framing split of `media::split_fragmented_mp4`
//! on hand-built boxes, `pssh_box_present`, the box walker, and the native readers against what
//! `media_inspect` reads from mp4dump's tree — recorded with Bento4 1.6 for hand-built cases
//! (`fixtures/media/mp4dump.cases.json`, CI) and live where mp4dump, ffmpeg and mp4encrypt are
//! on PATH (skipped otherwise). Synthetic KID / key / IV only.

mod common;

use drmr_core::events::NoSink;
use drmr_core::media::{
    FragmentBoundary, HexSecret, MediaEngine, MediaError, Subprocess, parse_fragment_boundary,
    parse_init_probe, parse_tenc_constant_iv_hex,
};
use drmr_core::mp4::{
    self, Mp4Box, Pssh, Schm, Tenc, Tfdt, Trun, fourcc_text, parse_boxes, pssh_box_present,
    split_fragmented, split_fragmented_mp4, walk,
};
use drmr_core::toolrun::{Secrets, ToolRunner};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "10101010101010101010101010101010";
const IV: &str = "20202020202020202020202020202020";

fn bx(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(8 + payload.len())
        .unwrap()
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(&kind);
    out.extend_from_slice(payload);
    out
}

fn full(kind: [u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut body = vec![version];
    body.extend_from_slice(&flags.to_be_bytes()[1..]);
    body.extend_from_slice(payload);
    bx(kind, &body)
}

fn large(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = 1u32.to_be_bytes().to_vec();
    out.extend_from_slice(&kind);
    out.extend_from_slice(&u64::try_from(16 + payload.len()).unwrap().to_be_bytes());
    out.extend_from_slice(payload);
    out
}

fn cat(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn on_path(tool: &str) -> bool {
    let found = std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .any(|dir| drmr_test_support::program_path(&dir.join(tool)).is_file())
    });
    if !found {
        eprintln!("skipped: {tool} not on PATH");
    }
    found
}

fn engine() -> Subprocess {
    Subprocess::new(Arc::new(ToolRunner::new(
        Arc::new(Secrets::new()),
        Arc::new(NoSink),
    )))
}

fn token(error: &MediaError) -> &str {
    error.hls_token().expect("an HlsMediaError")
}

// ---- split_fragmented_mp4: Python's framing rules, byte for byte ----

#[test]
fn split_keeps_every_box_byte_and_groups_each_moof_with_its_mdats() {
    let ftyp = bx(*b"ftyp", b"iso6\0\0\0\0iso6");
    let moov = bx(*b"moov", &bx(*b"mvhd", &[7; 20]));
    let moof1 = bx(*b"moof", &bx(*b"mfhd", &[1; 8]));
    let mdat1 = bx(*b"mdat", b"first");
    let moof2 = bx(*b"moof", &bx(*b"mfhd", &[2; 8]));
    let mdat2 = large(*b"mdat", b"second, with a 64-bit size");
    let mdat3 = bx(*b"mdat", b"third");
    let data = cat(&[
        &ftyp,
        &bx(*b"free", b"x"),
        &moov,
        &moof1,
        &mdat1,
        &bx(*b"skip", b""),
        &moof2,
        &mdat2,
        &mdat3,
        &bx(*b"mfra", &[0; 8]),
    ]);
    let (init, media) = split_fragmented(&data).unwrap();
    assert_eq!(init, cat(&[&ftyp, &moov]), "free / skip / mfra are dropped");
    assert_eq!(
        media,
        vec![cat(&[&moof1, &mdat1]), cat(&[&moof2, &mdat2, &mdat3])]
    );

    let dir = common::private_tempdir().unwrap();
    let (source, init_path) = (
        dir.path().join("normalized.mp4"),
        dir.path().join("init.mp4"),
    );
    std::fs::write(&source, &data).unwrap();
    assert_eq!(split_fragmented_mp4(&source, &init_path).unwrap(), media);
    assert_eq!(std::fs::read(&init_path).unwrap(), init);
}

#[test]
fn split_rejects_malformed_framing_with_pythons_tokens() {
    let ftyp = bx(*b"ftyp", b"iso6");
    let moov = bx(*b"moov", b"");
    let fragment = cat(&[&bx(*b"moof", b""), &bx(*b"mdat", b"x")]);
    // A box claiming 4 bytes. Read past its header, the next "box" would be an unknown type
    // (unsupported-normalized-mp4-box), so the check that rejects it first is observable.
    let size_too_small = cat(&[&4u32.to_be_bytes(), &8u32.to_be_bytes(), b"mdat"]);
    let mut size_zero = bx(*b"mdat", b"abc");
    size_zero[..4].copy_from_slice(&[0; 4]);
    let mut beyond = bx(*b"mdat", b"abc");
    beyond[3] = 200;
    let mut large_short = large(*b"mdat", b"");
    large_short[15] = 8;
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        (
            "seven bytes left",
            cat(&[&ftyp, &moov, &fragment, &[0; 7]]),
            "invalid-normalized-mp4",
        ),
        (
            "size below the header",
            cat(&[&ftyp, &moov, &size_too_small]),
            "invalid-normalized-mp4",
        ),
        (
            "size zero",
            cat(&[&ftyp, &moov, &fragment, &size_zero]),
            "invalid-normalized-mp4",
        ),
        (
            "size beyond the file",
            cat(&[&ftyp, &moov, &fragment, &beyond]),
            "invalid-normalized-mp4",
        ),
        (
            "64-bit size cut",
            cat(&[&ftyp, &moov, &fragment, &large(*b"mdat", b"")[..12]]),
            "invalid-normalized-mp4",
        ),
        (
            "64-bit size below 16",
            cat(&[&ftyp, &moov, &fragment, &large_short]),
            "invalid-normalized-mp4",
        ),
        (
            "styp",
            cat(&[&ftyp, &moov, &bx(*b"styp", b"msdh"), &fragment]),
            "unsupported-normalized-mp4-box",
        ),
        (
            "sidx",
            cat(&[&ftyp, &moov, &fragment, &bx(*b"sidx", &[0; 8])]),
            "unsupported-normalized-mp4-box",
        ),
        (
            "mdat before any moof",
            cat(&[&ftyp, &moov, &bx(*b"mdat", b"x"), &fragment]),
            "unsupported-normalized-mp4-box",
        ),
        (
            "no media",
            cat(&[&ftyp, &moov]),
            "text-segment-correspondence-mismatch",
        ),
        (
            "one init box",
            cat(&[&moov, &fragment]),
            "text-segment-correspondence-mismatch",
        ),
        (
            "three init boxes",
            cat(&[&ftyp, &moov, &ftyp, &fragment]),
            "text-segment-correspondence-mismatch",
        ),
        (
            "two ftyps count as two",
            cat(&[&ftyp, &ftyp, &fragment]),
            "",
        ),
    ];
    for (name, data, expected) in cases {
        let result = split_fragmented(&data);
        if expected.is_empty() {
            assert!(
                result.is_ok(),
                "{name}: Python counts any two of ftyp / moov"
            );
        } else {
            assert_eq!(result.unwrap_err().token(), expected, "{name}");
        }
    }

    // A failed split writes no init; an unreadable source is an OSError.
    let dir = common::private_tempdir().unwrap();
    let (source, init) = (dir.path().join("bad.mp4"), dir.path().join("init.mp4"));
    std::fs::write(&source, cat(&[&ftyp, &moov])).unwrap();
    let error = split_fragmented_mp4(&source, &init).unwrap_err();
    assert_eq!(token(&error), "text-segment-correspondence-mismatch");
    assert!(!init.exists());
    let missing = split_fragmented_mp4(&dir.path().join("absent.mp4"), &init).unwrap_err();
    assert_eq!(missing.python_class(), "OSError");
}

#[test]
fn pssh_box_present_is_an_exact_byte_containment() {
    let pssh = full(
        *b"pssh",
        0,
        0,
        &cat(&[&[0xed; 16], &3u32.to_be_bytes(), b"abc"]),
    );
    let dir = common::private_tempdir().unwrap();
    let init = dir.path().join("init.mp4");
    std::fs::write(&init, cat(&[&bx(*b"ftyp", b"iso6"), &bx(*b"moov", &pssh)])).unwrap();
    assert!(pssh_box_present(&init, &pssh).unwrap());
    let mut other = pssh.clone();
    *other.last_mut().unwrap() = b'd';
    assert!(!pssh_box_present(&init, &other).unwrap());
    assert!(pssh_box_present(&init, b"").unwrap(), "b'' in data is True");
    assert_eq!(
        pssh_box_present(&dir.path().join("absent.mp4"), &pssh)
            .unwrap_err()
            .python_class(),
        "OSError"
    );
}

// ---- the box walker ----

fn names(boxes: &[Mp4Box]) -> Vec<String> {
    walk(boxes)
        .iter()
        .map(|found| fourcc_text(&found.kind()))
        .collect()
}

fn visual_entry(kind: [u8; 4], children: &[u8]) -> Vec<u8> {
    let mut fields = vec![0u8; 78];
    fields[7] = 1;
    bx(kind, &cat(&[&fields, children]))
}

/// A hand-built encrypted init (`ftyp` with a 64-bit size, `moov` with an `encv` entry and a
/// version 1 `pssh`, a `uuid` box, an `mdat` of size 0) and its `moov`.
fn encrypted_init() -> (Vec<u8>, Vec<u8>) {
    let tenc = full(
        *b"tenc",
        1,
        0,
        &cat(&[
            &[0, 0x19, 1, 0],
            &hex::decode(KID).unwrap(),
            &[16],
            &hex::decode(IV).unwrap(),
        ]),
    );
    let sinf = bx(
        *b"sinf",
        &cat(&[
            &bx(*b"frma", b"avc1"),
            &full(*b"schm", 0, 0, b"cbcs\x00\x01\x00\x00"),
            &bx(*b"schi", &tenc),
        ]),
    );
    let entry = visual_entry(*b"encv", &cat(&[&bx(*b"avcC", &[1, 0x64, 0, 0x1f]), &sinf]));
    let stsd = full(*b"stsd", 0, 0, &cat(&[&1u32.to_be_bytes(), &entry]));
    let pssh = full(
        *b"pssh",
        1,
        0,
        &cat(&[
            &[7; 16],
            &1u32.to_be_bytes(),
            &hex::decode(KID).unwrap(),
            &2u32.to_be_bytes(),
            b"xy",
        ]),
    );
    let moov = bx(
        *b"moov",
        &cat(&[
            &bx(*b"trak", &bx(*b"mdia", &bx(*b"minf", &bx(*b"stbl", &stsd)))),
            &pssh,
        ]),
    );
    let uuid = bx(*b"uuid", &cat(&[&[9; 16], b"abc"]));
    let mut tail = bx(*b"mdat", b"to the end");
    tail[..4].copy_from_slice(&[0; 4]);
    (cat(&[&large(*b"ftyp", b"iso6"), &moov, &uuid, &tail]), moov)
}

#[test]
fn the_walker_opens_the_containers_and_sample_entries_in_file_order() {
    let (data, moov) = encrypted_init();
    let boxes = parse_boxes(&data).unwrap();
    assert_eq!(
        names(&boxes),
        [
            "ftyp", "moov", "trak", "mdia", "minf", "stbl", "stsd", "encv", "avcC", "sinf", "frma",
            "schm", "schi", "tenc", "pssh", "uuid", "mdat"
        ]
    );
    assert_eq!(
        (boxes[0].header.header_size, boxes[0].header.size),
        (16, 20)
    );
    assert_eq!(
        (boxes[2].header.header_size, boxes[2].header.size),
        (24, 27),
        "uuid"
    );
    assert_eq!(boxes[3].header.size, 18, "a zero size reaches the end");
    let mut broken = moov.clone();
    broken[11] = 0xff; // the trak's size now passes its parent's end
    assert_eq!(
        parse_boxes(&broken).unwrap_err(),
        mp4::Mp4Error("mp4-box-size-invalid")
    );
    assert_eq!(
        parse_boxes(&[0, 0, 0]).unwrap_err(),
        mp4::Mp4Error("mp4-box-truncated")
    );
}

#[test]
fn sample_entries_and_pssh_boxes_are_read_from_the_tree() {
    let (data, _) = encrypted_init();
    let entries = mp4::sample_entries(&data).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(&entries[0].format, b"encv");
    assert_eq!(entries[0].original_format.as_ref(), Some(b"avc1"));
    assert_eq!(
        entries[0].scheme.as_ref().map(|s| s.scheme_type),
        Some(*b"cbcs")
    );
    let parsed_tenc = entries[0].tenc.as_ref().unwrap();
    assert_eq!(
        (
            parsed_tenc.default_crypt_byte_block,
            parsed_tenc.default_skip_byte_block
        ),
        (1, 9)
    );
    assert_eq!(hex::encode(parsed_tenc.default_kid), KID);
    let debug = format!("{parsed_tenc:?}");
    assert!(
        !debug.contains("32, 32") && !debug.contains(IV),
        "Debug never prints the IV: {debug}"
    );
    assert_eq!(
        mp4::pssh_boxes(&data).unwrap(),
        vec![Pssh {
            version: 1,
            system_id: [7; 16],
            kids: vec![hex::decode(KID).unwrap().try_into().unwrap()],
            data: b"xy".to_vec()
        }]
    );
}

#[test]
fn typed_boxes_read_like_bento4() {
    let kid = hex::decode(KID).unwrap();
    let iv = hex::decode(IV).unwrap();
    let tenc = |version: u8, rest: &[u8]| full(*b"tenc", version, 0, rest)[8..].to_vec();
    let cbcs = Tenc::parse(&tenc(1, &cat(&[&[0, 0x19, 1, 0], &kid, &[16], &iv]))).unwrap();
    assert_eq!(
        cbcs.default_constant_iv.as_ref().map(|v| v.to_vec()),
        Some(iv.clone())
    );
    let cenc = Tenc::parse(&tenc(
        0,
        &cat(&[&[0, 0x77, 1, 8], &kid, b"trailing bytes ignored"]),
    ))
    .unwrap();
    assert_eq!(
        (
            cenc.default_crypt_byte_block,
            cenc.default_skip_byte_block,
            cenc.default_constant_iv.is_none()
        ),
        (0, 0, true)
    );
    assert!(
        Tenc::parse(&tenc(1, &cat(&[&[0, 0x19, 1, 0], &kid]))).is_none(),
        "no IV size: opaque"
    );
    assert!(
        Tenc::parse(&tenc(1, &cat(&[&[0, 0x19, 1, 0], &kid, &[17], &[0; 17]]))).is_none(),
        "IV above 16"
    );
    assert!(
        Tenc::parse(&tenc(1, &cat(&[&[0, 0x19, 1, 0], &kid, &[16], &[0; 4]]))).is_none(),
        "IV cut"
    );
    let empty = Tenc::parse(&tenc(1, &cat(&[&[0, 0x19, 1, 0], &kid, &[0]]))).unwrap();
    assert_eq!(
        empty.default_constant_iv.as_ref().map(|v| v.len()),
        Some(0),
        "an empty constant IV"
    );

    let schm =
        Schm::parse(&full(*b"schm", 0, 1, b"cbcs\0\x01\0\0https://x.invalid/\0junk")[8..]).unwrap();
    assert_eq!((schm.scheme_type, schm.scheme_version), (*b"cbcs", 0x10000));
    assert_eq!(schm.scheme_uri.as_deref(), Some(&b"https://x.invalid/"[..]));
    assert!(Schm::parse(&full(*b"schm", 0, 0, b"cbcs")[8..]).is_none());
    assert_eq!(fourcc_text(b"cb\0s"), "cb");
    assert_eq!(fourcc_text(b"c\xa9bs"), "c");
    assert_eq!(fourcc_text(b"c\"\\s"), "c\"\\s");

    // Native compatibility reads can continue past a short box. At EOF the native
    // reader returns zero; an external malformed-atom dump need not define that value.
    let data = cat(&[
        &full(*b"tfdt", 1, 0, &9u32.to_be_bytes()),
        &full(
            *b"trun",
            0,
            0x100,
            &cat(&[&3u32.to_be_bytes(), &1u32.to_be_bytes()]),
        ),
    ]);
    let boxes = parse_boxes(&data).unwrap();
    let tfdt = Tfdt::read(&data, &boxes[0].header).unwrap();
    assert_eq!(
        tfdt.base_media_decode_time,
        (9 << 32) | 0x14,
        "the next box's size continues the field"
    );
    let trun = Trun::read(&data, &boxes[1].header).unwrap();
    assert_eq!(trun.sample_count, 0, "three duration records do not fit");
    let at_eof = full(*b"trun", 0, 0x1, b"");
    let eof_boxes = parse_boxes(&at_eof).unwrap();
    let trun = Trun::read(&at_eof, &eof_boxes[0].header).unwrap();
    assert_eq!((trun.sample_count, trun.data_offset), (0, Some(0)));
    let records = full(
        *b"trun",
        1,
        0x305,
        &cat(&[
            &2u32.to_be_bytes(),
            &100i32.to_be_bytes(),
            &7u32.to_be_bytes(),
            &[0; 16],
        ]),
    );
    let record_boxes = parse_boxes(&records).unwrap();
    let trun = Trun::read(&records, &record_boxes[0].header).unwrap();
    assert_eq!(
        (trun.sample_count, trun.data_offset, trun.first_sample_flags),
        (2, Some(100), Some(7))
    );
}

// ---- the native readers against what media_inspect reads from mp4dump ----

fn fixture() -> Value {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/mp4dump.cases.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn probe_json(probe: &drmr_core::media::InitProbe) -> Value {
    json!([
        probe.scheme_type,
        probe.constant_iv_hex.as_ref().map(HexSecret::expose),
        probe.default_kid_hex,
        probe.per_sample_iv_size
    ])
}

fn boundary_json(boundary: Option<&FragmentBoundary>) -> Value {
    boundary.map_or(Value::Null, |b| {
        json!({"base_media_decode_time": b.base_media_decode_time, "sample_count": b.sample_count})
    })
}

/// `{"ok": value}` or `{"error": class}`, as recorded in the fixture.
fn outcome(result: Result<Value, MediaError>) -> Value {
    match result {
        Ok(value) => json!({"ok": value}),
        Err(error) => json!({"error": error.python_class()}),
    }
}

/// The four reads of one file, by the native readers and by the implementation of the parse functions
/// over an mp4dump tree; each must equal Python's recorded outcome.
fn assert_reads(name: &str, data: &[u8], tree: &Value, expected: &Value) {
    let native = [
        outcome(mp4::init_probe(data).map(|p| probe_json(&p))),
        outcome(
            mp4::tenc_constant_iv_hex(data).map(|iv| json!(iv.as_ref().map(HexSecret::expose))),
        ),
        outcome(mp4::fragment_boundary(data).map(|b| boundary_json(b.as_ref()))),
        outcome(mp4::tenc_pattern(data).map(|(crypt, skip)| json!([crypt, skip]))),
    ];
    let from_tree = [
        outcome(parse_init_probe(tree).map(|p| probe_json(&p))),
        outcome(
            parse_tenc_constant_iv_hex(tree).map(|iv| json!(iv.as_ref().map(HexSecret::expose))),
        ),
        outcome(Ok(boundary_json(parse_fragment_boundary(tree).as_ref()))),
        outcome(Ok(tree_pattern(tree))),
    ];
    let keys = [
        "init_probe",
        "tenc_constant_iv_hex",
        "fragment_boundary",
        "tenc_pattern",
    ];
    for (index, key) in keys.iter().enumerate() {
        assert_eq!(
            from_tree[index], expected[key],
            "{name}: the parse of mp4dump's tree, {key}"
        );
        assert_eq!(
            native[index], expected[key],
            "{name}: the native reader, {key}"
        );
    }
}

/// `initialization segment validation`'s read of the tree (the first `tenc`'s pattern, 0 when absent).
fn tree_pattern(tree: &Value) -> Value {
    fn first_tenc(node: &Value) -> Option<&serde_json::Map<String, Value>> {
        match node {
            Value::Object(map) if map.get("name") == Some(&json!("tenc")) => Some(map),
            Value::Object(map) => map.values().find_map(first_tenc),
            Value::Array(items) => items.iter().find_map(first_tenc),
            _ => None,
        }
    }
    let tenc = first_tenc(tree);
    let field = |key: &str| {
        tenc.and_then(|map| map.get(key))
            .cloned()
            .unwrap_or(json!(0))
    };
    json!([
        field("default_crypt_byte_block"),
        field("default_skip_byte_block")
    ])
}

#[test]
fn native_readers_match_what_python_reads_from_bento4s_tree() {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().unwrap();
    let native_only = fixture["native_only_cases"].as_array().unwrap();
    assert_eq!(cases.len(), 26);
    assert_eq!(native_only.len(), 1);
    assert_eq!(native_only[0]["name"], "frag-trun-count-at-eof");
    // This recorded malformed tree defines a native compatibility result, not a
    // portable external-tool value: Bento4 leaves its optional offset uninitialized.
    for case in cases.iter().chain(native_only) {
        let name = case["name"].as_str().unwrap();
        let data = hex::decode(case["hex"].as_str().unwrap()).unwrap();
        assert_reads(name, &data, &case["tree"], case);
    }
}

#[test]
fn native_trun_eof_prefixes_define_count_and_optional_field_values() {
    for flags in [0_u32, 1, 4, 5] {
        let mut payload = 3_u32.to_be_bytes().to_vec();
        if flags & 1 != 0 {
            payload.extend(0x1234_5678_i32.to_be_bytes());
        }
        if flags & 4 != 0 {
            payload.extend(0x89ab_cdef_u32.to_be_bytes());
        }
        for end in 0..=payload.len() {
            let data = full(*b"trun", 0, flags, &payload[..end]);
            let boxes = parse_boxes(&data).unwrap();
            let actual = Trun::read(&data, &boxes[0].header).unwrap();
            let offset = (flags & 1 != 0).then_some(if end >= 8 { 0x1234_5678 } else { 0 });
            let first_end = 8 + usize::from(flags & 1 != 0) * 4;
            let first = (flags & 4 != 0).then_some(if end >= first_end { 0x89ab_cdef } else { 0 });
            assert_eq!(
                (
                    actual.sample_count,
                    actual.data_offset,
                    actual.first_sample_flags
                ),
                (if end >= 4 { 3 } else { 0 }, offset, first),
                "flags {flags:#x}, EOF after {end} payload bytes"
            );
        }
    }
}

#[test]
fn live_trun_fixtures_initialize_every_flag_selected_optional_field() {
    let fixture = fixture();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(
        cases.len(),
        26,
        "retain the complete live differential corpus"
    );
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let data = hex::decode(case["hex"].as_str().unwrap()).unwrap();
        let boxes = parse_boxes(&data).unwrap();
        for found in walk(&boxes)
            .into_iter()
            .filter(|node| node.kind() == *b"trun")
        {
            let payload = found.header.payload(&data);
            let flags = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
            if flags & 0xff != 0 {
                let required = 8 + 4 * usize::try_from((flags & 0xff).count_ones()).unwrap();
                assert!(
                    payload.len() >= required,
                    "{name}: truncated optional trun fields cannot be an external value oracle"
                );
            }
        }
    }
}

#[test]
fn live_mp4dump_trees_equal_the_recorded_ones_and_the_native_reads() {
    if !on_path("mp4dump") {
        return;
    }
    let fixture = fixture();
    // The trees (and Bento4's handling of the malformed cases) were recorded with one release;
    // another release is compared by the real-file test below, not against this record.
    let banner = std::process::Command::new("mp4dump").output().unwrap();
    let banner = String::from_utf8_lossy(&[banner.stdout, banner.stderr].concat()).into_owned();
    let recorded = fixture["bento4"].as_str().unwrap();
    if !banner.contains(recorded) {
        eprintln!("skipped: mp4dump is not {recorded}");
        return;
    }
    let engine = engine();
    let dir = common::private_tempdir().unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let data = hex::decode(case["hex"].as_str().unwrap()).unwrap();
        let path = dir.path().join(format!("{name}.mp4"));
        std::fs::write(&path, &data).unwrap();
        let tree = engine.box_tree(&path).unwrap();
        assert_eq!(tree, case["tree"], "{name}: mp4dump {recorded}");
        assert_reads(name, &data, &tree, case);
    }
}

fn run(argv: &[&str]) {
    let status = std::process::Command::new(argv[0])
        .args(&argv[1..])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "{argv:?}");
}

/// The init and segments of a synthetic A/V fMP4 (ffmpeg lavfi), split with the Python rules.
fn synthetic_fragments(dir: &Path) -> (PathBuf, Vec<PathBuf>) {
    let source = dir.join("av.mp4");
    run(&[
        "ffmpeg",
        "-v",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        "testsrc=duration=2:size=64x64:rate=5",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
        "-t",
        "2",
        "-c:v",
        "libx264",
        "-g",
        "5",
        "-c:a",
        "aac",
        "-movflags",
        "+frag_keyframe+empty_moov+default_base_moof",
        source.to_str().unwrap(),
    ]);
    let init = dir.join("init.mp4");
    let media = split_fragmented_mp4(&source, &init).unwrap();
    let mut segments = Vec::new();
    for (index, fragment) in media.iter().enumerate() {
        let path = dir.join(format!("seg_{index}.m4s"));
        std::fs::write(&path, fragment).unwrap();
        segments.push(path);
    }
    (init, segments)
}

#[test]
fn native_readers_match_mp4dump_on_ffmpeg_and_bento4_output() {
    if !(on_path("ffmpeg") && on_path("mp4dump")) {
        return;
    }
    let dir = common::private_tempdir().unwrap();
    let engine = engine();
    let (init, segments) = synthetic_fragments(dir.path());
    let mut files = vec![init.clone()];
    files.extend(segments.iter().cloned());
    let source = dir.path().join("av.mp4");
    let mut encrypted = Vec::new();
    if on_path("mp4encrypt") {
        for (method, iv) in [("MPEG-CBCS", IV), ("MPEG-CENC", &IV[..16])] {
            let out = dir.path().join(format!("{method}.mp4"));
            let key = |track: &str| format!("{track}:{KEY}:{iv}");
            let kid = |track: &str| format!("{track}:KID:{KID}");
            run(&[
                "mp4encrypt",
                "--method",
                method,
                "--key",
                &key("1"),
                "--property",
                &kid("1"),
                "--key",
                &key("2"),
                "--property",
                &kid("2"),
                source.to_str().unwrap(),
                out.to_str().unwrap(),
            ]);
            encrypted.push(out);
        }
    }
    files.extend(encrypted.iter().cloned());
    for path in &files {
        let data = std::fs::read(path).unwrap();
        let tree = engine.box_tree(path).unwrap();
        let expected = json!({
            "init_probe": outcome(parse_init_probe(&tree).map(|p| probe_json(&p))),
            "tenc_constant_iv_hex": outcome(parse_tenc_constant_iv_hex(&tree).map(|iv| json!(iv.as_ref().map(HexSecret::expose)))),
            "fragment_boundary": outcome(Ok(boundary_json(parse_fragment_boundary(&tree).as_ref()))),
            "tenc_pattern": outcome(Ok(tree_pattern(&tree))),
        });
        assert_reads(&path.display().to_string(), &data, &tree, &expected);
    }
    // What the readers found on the real files.
    let plain = mp4::init_probe(&std::fs::read(&init).unwrap()).unwrap();
    assert_eq!(
        plain,
        drmr_core::media::InitProbe::default(),
        "an unencrypted init"
    );
    let boundary = mp4::fragment_boundary(&std::fs::read(&segments[1]).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        boundary.base_media_decode_time,
        json!(10240),
        "the second video fragment starts at 1 s"
    );
    let formats: Vec<String> = mp4::sample_entries(&std::fs::read(&init).unwrap())
        .unwrap()
        .iter()
        .map(|entry| fourcc_text(&entry.format))
        .collect();
    assert_eq!(formats, ["avc1", "mp4a"]);
    if let Some(cbcs) = encrypted.first() {
        let data = std::fs::read(cbcs).unwrap();
        let probe = mp4::init_probe(&data).unwrap();
        assert_eq!(probe.scheme_type.as_deref(), Some("cbcs"));
        assert!(probe.constant_iv_hex.as_ref().is_some_and(|iv| *iv == IV));
        assert_eq!(probe.default_kid_hex.as_deref(), Some(KID));
        assert_eq!(mp4::tenc_pattern(&data).unwrap(), (1, 9), "video: 1:9");
        let entries = mp4::sample_entries(&data).unwrap();
        let originals: Vec<String> = entries
            .iter()
            .map(|entry| fourcc_text(entry.original_format.as_ref().unwrap()))
            .collect();
        assert_eq!(originals, ["avc1", "mp4a"]);
        assert_eq!(
            entries[1]
                .tenc
                .as_ref()
                .map(|t| (t.default_crypt_byte_block, t.default_skip_byte_block)),
            Some((0, 0)),
            "audio: full-sample"
        );
    }
}

#[test]
fn ffmpeg_fragments_split_back_into_the_original_bytes() {
    if !on_path("ffmpeg") {
        return;
    }
    let dir = common::private_tempdir().unwrap();
    let (init, segments) = synthetic_fragments(dir.path());
    let original = std::fs::read(dir.path().join("av.mp4")).unwrap();
    let mut rebuilt = std::fs::read(&init).unwrap();
    for segment in &segments {
        let bytes = std::fs::read(segment).unwrap();
        assert_eq!(&bytes[4..8], b"moof");
        rebuilt.extend(bytes);
    }
    // Only the trailing mfra (an index box) is dropped.
    let boxes = parse_boxes(&original).unwrap();
    let kept: Vec<u8> = boxes
        .iter()
        .filter(|found| !matches!(&found.kind(), b"mfra" | b"free" | b"skip"))
        .flat_map(|found| found.header.bytes(&original).to_vec())
        .collect();
    assert_eq!(rebuilt, kept);
    assert!(segments.len() >= 2);
}
