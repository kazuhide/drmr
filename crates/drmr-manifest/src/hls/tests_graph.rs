use super::*;
use crate::{DocumentId, Format, ManifestLink, Options, Presentation, Profile, ReferenceError};
use std::collections::BTreeMap;

const MEDIA: &[u8] = b"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:2,\npart.ts\n#EXT-X-ENDLIST\n";

fn graph<'a>(
    bodies: &[&'a [u8]],
    bindings: &[(usize, &str, usize)],
    profile: Profile,
) -> Vec<DiagnosticSink<'a>> {
    graph_with_bases(bodies, &[], bindings, profile)
}

fn graph_with_bases<'a>(
    bodies: &[&'a [u8]],
    bases: &[Option<&'a str>],
    bindings: &[(usize, &str, usize)],
    profile: Profile,
) -> Vec<DiagnosticSink<'a>> {
    let documents = bodies
        .iter()
        .enumerate()
        .map(|(index, bytes)| Document {
            id: DocumentId(u32::try_from(index).unwrap()),
            format: Format::Hls,
            bytes,
            base_uri: bases.get(index).copied().flatten(),
        })
        .collect::<Vec<_>>();
    let links = bindings
        .iter()
        .map(|(from, reference, to)| ManifestLink {
            from: DocumentId(u32::try_from(*from).unwrap()),
            reference,
            to: DocumentId(u32::try_from(*to).unwrap()),
        })
        .collect::<Vec<_>>();
    let options = Options::new(profile);
    let context = LintContext::new(
        &options,
        Some(Presentation {
            entrypoint: DocumentId(0),
            documents: &documents,
            links: &links,
        }),
    );
    let mut facts = BTreeMap::new();
    let mut results = Vec::new();
    for document in &documents {
        let mut sink = DiagnosticSink::new(*document, options.limits.max_diagnostics);
        facts.insert(document.id, lint(document, &context, &mut sink));
        results.push(sink);
    }
    let index = GraphFacts::new(&facts);
    for (document, sink) in documents.iter().zip(&mut results) {
        lint_graph(
            document,
            &facts[&document.id],
            &facts,
            &index,
            &context,
            sink,
        );
    }
    results
}

fn master(tags: &str) -> String {
    format!("#EXTM3U\n#EXT-X-VERSION:7\n{tags}\n")
}

fn media_tags(tags: &str) -> String {
    std::str::from_utf8(MEDIA)
        .unwrap()
        .replace("#EXTINF:2,", &format!("{tags}\n#EXTINF:2,"))
}

#[test]
fn graph_start_inheritance_compares_typed_values_and_warns_on_redundant_declarations() {
    let value = master(&format!("#EXT-X-START:TIME-OFFSET=1.0\n{}", variant()));
    let child = media_tags("#EXT-X-START:PRECISE=NO,TIME-OFFSET=1.000");
    let links = [(0, "child.m3u8", 1)];
    let same = graph(
        &[value.as_bytes(), child.as_bytes()],
        &links,
        Profile::HlsBaseManifest,
    );
    assert_eq!(same[0].errors, 0);
    assert!(has(&same[0], "HLS-GRAPH-START-DUPLICATE"));
    let different = child.replace("TIME-OFFSET=1.000", "TIME-OFFSET=2");
    assert!(has(
        &graph(
            &[value.as_bytes(), different.as_bytes()],
            &links,
            Profile::HlsBaseManifest
        )[0],
        "HLS-GRAPH-START"
    ));
    let precise = child.replace("PRECISE=NO", "PRECISE=YES");
    assert!(has(
        &graph(
            &[value.as_bytes(), precise.as_bytes()],
            &links,
            Profile::HlsBaseManifest
        )[0],
        "HLS-GRAPH-START"
    ));
    let zero_master = value.replace("TIME-OFFSET=1.0", "TIME-OFFSET=-0.0");
    let zero_child = child.replace("TIME-OFFSET=1.000", "TIME-OFFSET=0");
    assert_eq!(
        graph(
            &[zero_master.as_bytes(), zero_child.as_bytes()],
            &links,
            Profile::HlsBaseManifest
        )[0]
        .errors,
        0
    );
    let inherited = graph(&[value.as_bytes(), MEDIA], &links, Profile::HlsBaseManifest);
    assert_eq!(inherited[0].errors, 0);
    assert!(!has(&inherited[0], "HLS-GRAPH-START-DUPLICATE"));
    let independent_master = value.replace(
        "#EXT-X-START:TIME-OFFSET=1.0",
        "#EXT-X-INDEPENDENT-SEGMENTS",
    );
    let independent_child = media_tags("#EXT-X-INDEPENDENT-SEGMENTS");
    let redundant = graph(
        &[independent_master.as_bytes(), independent_child.as_bytes()],
        &links,
        Profile::HlsBaseManifest,
    );
    assert_eq!(redundant[0].errors, 0);
    assert!(has(&redundant[0], "HLS-GRAPH-INDEPENDENT-DUPLICATE"));
}

#[test]
fn retained_facts_debug_omit_key_identifiers_payloads_and_retrieval_bases() {
    let body = media_tags(
        "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://private-key-identifier\"\n#EXT-X-PROGRAM-DATE-TIME:2024-03-01T00:00:00Z\n#EXT-X-DATERANGE:ID=\"private-date-id\",START-DATE=\"2024-03-01T00:00:00Z\",SCTE35-CMD=0xDEADBEEF0123456789",
    );
    let document = Document {
        id: DocumentId(0),
        format: Format::Hls,
        bytes: body.as_bytes(),
        base_uri: Some("https://example.invalid/private-retrieval-token/index.m3u8"),
    };
    let options = Options::new(Profile::HlsBaseManifest);
    let context = LintContext::new(&options, None);
    let mut sink = DiagnosticSink::new(document, options.limits.max_diagnostics);
    let facts = lint(&document, &context, &mut sink);
    let debug = format!("{facts:?}");
    for secret in [
        "private-key-identifier",
        "private-date-id",
        "DEADBEEF0123456789",
        "private-retrieval-token",
    ] {
        assert!(!debug.contains(secret));
    }
}

#[test]
fn graph_program_date_time_presence_requires_all_children_without_comparing_media_timestamps() {
    let value = master(
        "#EXT-X-STREAM-INF:BANDWIDTH=1000\nfirst.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000\nsecond.m3u8",
    );
    let dated = media_tags("#EXT-X-PROGRAM-DATE-TIME:2024-03-01T00:00:00Z");
    let links = [(0, "first.m3u8", 1), (0, "second.m3u8", 2)];
    let missing = graph(
        &[value.as_bytes(), dated.as_bytes(), MEDIA],
        &links,
        Profile::HlsBaseManifest,
    );
    assert!(has(&missing[0], "HLS-GRAPH-PDT-PRESENCE"));
    let another_clock = dated.replace("2024-03-01", "2025-03-01");
    let both = graph(
        &[value.as_bytes(), dated.as_bytes(), another_clock.as_bytes()],
        &links,
        Profile::HlsBaseManifest,
    );
    assert_eq!(both[0].errors, 0);
    assert_eq!(
        both[0].coverage["HLS-GRAPH-PDT-MAPPING"].scope,
        RuleScope::OutsideScope
    );
}

#[test]
fn graph_date_ranges_compare_merged_typed_sets_and_preserve_uncertain_time_coverage() {
    let value = master(
        "#EXT-X-STREAM-INF:BANDWIDTH=1000\nfirst.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000\nsecond.m3u8",
    );
    let pdt = "#EXT-X-PROGRAM-DATE-TIME:2024-03-01T00:00:00Z";
    let first = media_tags(&format!(
        "{pdt}\n#EXT-X-DATERANGE:ID=\"range\",START-DATE=\"2024-03-01T00:00:00Z\",DURATION=1.0\n#EXT-X-DATERANGE:ID=\"range\",END-DATE=\"2024-03-01T00:00:01Z\",X-NUMBER=1.0"
    ));
    let second = media_tags(&format!(
        "{pdt}\n#EXT-X-DATERANGE:X-NUMBER=1,END-DATE=\"2024-03-01T01:00:01.000+01:00\",DURATION=1.000,START-DATE=\"2024-03-01T01:00:00+01:00\",ID=\"range\""
    ));
    let links = [(0, "first.m3u8", 1), (0, "second.m3u8", 2)];
    let same = graph(
        &[value.as_bytes(), first.as_bytes(), second.as_bytes()],
        &links,
        Profile::HlsBaseManifest,
    );
    assert!(
        same.iter()
            .all(|result| result.errors == 0 && !result.incomplete)
    );
    assert_eq!(
        same[0].coverage["HLS-GRAPH-DATE-RANGES"].status,
        CheckStatus::Passed
    );
    let different = second.replace("ID=\"range\"", "ID=\"another\"");
    assert!(has(
        &graph(
            &[value.as_bytes(), first.as_bytes(), different.as_bytes()],
            &links,
            Profile::HlsBaseManifest
        )[0],
        "HLS-GRAPH-DATE-RANGES"
    ));
    let uncertain = second.replace("2024-03-01T01:00:00+01:00", "2024-03-01T00:00:00");
    let unknown = graph(
        &[value.as_bytes(), first.as_bytes(), uncertain.as_bytes()],
        &links,
        Profile::HlsBaseManifest,
    );
    assert_eq!(unknown[0].errors, 0);
    assert!(
        unknown[0].coverage["HLS-GRAPH-DATE-RANGES"]
            .not_checked_reasons
            .contains(&NotCheckedReason::UnsupportedFeature)
    );
}

#[test]
fn graph_date_ranges_keep_provable_mismatches_when_a_different_child_is_uncertain() {
    let value = master(
        "#EXT-X-STREAM-INF:BANDWIDTH=1000\nfirst.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000\nsecond.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=3000\nthird.m3u8",
    );
    let tags = "#EXT-X-PROGRAM-DATE-TIME:2024-03-01T00:00:00Z\n#EXT-X-DATERANGE:ID=\"range\",START-DATE=\"2024-03-01T00:00:00Z\",DURATION=1";
    let known = media_tags(tags);
    let uncertain = known.replace(
        "START-DATE=\"2024-03-01T00:00:00Z\"",
        "START-DATE=\"2024-03-01T00:00:00\"",
    );
    let different = known.replace("ID=\"range\"", "ID=\"other\"");
    let links = [
        (0, "first.m3u8", 1),
        (0, "second.m3u8", 2),
        (0, "third.m3u8", 3),
    ];
    let result = graph(
        &[
            value.as_bytes(),
            uncertain.as_bytes(),
            known.as_bytes(),
            different.as_bytes(),
        ],
        &links,
        Profile::HlsBaseManifest,
    );
    assert!(result[0].incomplete);
    assert!(has(&result[0], "HLS-GRAPH-DATE-RANGES"));
}
fn variant() -> &'static str {
    "#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"avc1.4d401e\"\nchild.m3u8"
}
fn has(result: &DiagnosticSink<'_>, rule: &str) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.rule_id == rule)
}

#[test]
fn master_children_are_manifest_bindings_not_keys_maps_or_segments() {
    let value = master(&format!(
        "#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"https://keys.example.invalid/key\"\n{}",
        variant()
    ));
    let result = graph(
        &[value.as_bytes(), MEDIA],
        &[(0, "child.m3u8", 1)],
        Profile::HlsBaseManifest,
    );
    assert!(
        result
            .iter()
            .all(|result| result.errors == 0 && !result.incomplete)
    );
    let missing = graph(&[value.as_bytes()], &[], Profile::HlsBaseManifest);
    assert!(missing[0].incomplete);
    let foreign = graph(
        &[value.as_bytes(), MEDIA],
        &[(0, "child.m3u8", 1), (1, "part.ts", 0)],
        Profile::HlsBaseManifest,
    );
    assert!(has(&foreign[1], "HLS-GRAPH-BINDINGS"));
    let nested = graph(
        &[value.as_bytes(), value.as_bytes()],
        &[(0, "child.m3u8", 1), (1, "child.m3u8", 0)],
        Profile::HlsBaseManifest,
    );
    assert!(has(&nested[0], "HLS-GRAPH-PLAYLIST-KIND"));
}

#[test]
fn rendition_constraints_distinguish_omitted_autoselect_from_explicit_no() {
    let declaration = "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"English\",DEFAULT=YES,URI=\"audio.m3u8\"";
    let value = master(&format!(
        "{declaration}\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,AUDIO=\"audio\"\nchild.m3u8"
    ));
    let good = graph(
        &[value.as_bytes(), MEDIA, MEDIA],
        &[(0, "child.m3u8", 1), (0, "audio.m3u8", 2)],
        Profile::HlsBaseManifest,
    );
    assert!(
        good.iter()
            .all(|result| result.errors == 0 && !result.incomplete)
    );
    let wrong = value.replace("DEFAULT=YES", "DEFAULT=YES,AUTOSELECT=NO");
    assert!(has(
        &graph(
            &[wrong.as_bytes(), MEDIA],
            &[(0, "child.m3u8", 1), (0, "audio.m3u8", 1)],
            Profile::HlsBaseManifest
        )[0],
        "HLS-RENDITION"
    ));
    for (replacement, id) in [
        (
            declaration.replace("TYPE=AUDIO", "TYPE=NOT-A-TYPE"),
            "HLS-RENDITION",
        ),
        (
            declaration.replace("GROUP-ID=\"audio\"", "GROUP-ID=audio"),
            "HLS-ATTRIBUTE-TYPES",
        ),
        (
            declaration
                .replace("TYPE=AUDIO", "TYPE=SUBTITLES")
                .replace(",URI=\"audio.m3u8\"", ""),
            "HLS-RENDITION",
        ),
        (format!("{declaration},FORCED=NO"), "HLS-RENDITION"),
        (
            format!("{declaration}\n{declaration}"),
            "HLS-RENDITION-NAMES",
        ),
        (
            format!(
                "{declaration}\n{}",
                declaration.replace("NAME=\"English\"", "NAME=\"French\"")
            ),
            "HLS-RENDITION-DEFAULT",
        ),
    ] {
        let value = master(&format!("{replacement}\n{}", variant()));
        assert!(
            has(
                &graph(
                    &[value.as_bytes(), MEDIA],
                    &[(0, "child.m3u8", 1), (0, "audio.m3u8", 1)],
                    Profile::HlsBaseManifest
                )[0],
                id
            ),
            "missing {id}"
        );
    }
    let missing_group = master("#EXT-X-STREAM-INF:BANDWIDTH=1000000,AUDIO=\"absent\"\nchild.m3u8");
    assert!(has(
        &graph(
            &[missing_group.as_bytes(), MEDIA],
            &[(0, "child.m3u8", 1)],
            Profile::HlsBaseManifest
        )[0],
        "HLS-RENDITION-REFERENCES"
    ));
}

#[test]
fn rendition_groups_may_have_different_group_ids_uris_and_channel_counts() {
    let value = master(
        "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"stereo\",NAME=\"English\",CHANNELS=\"2\",URI=\"audio-stereo.m3u8\"\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"surround\",NAME=\"English\",CHANNELS=\"6\",URI=\"audio-surround.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,AUDIO=\"stereo\"\nchild.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000000,AUDIO=\"surround\"\nchild.m3u8",
    );
    let links = [
        (0, "child.m3u8", 1),
        (0, "audio-stereo.m3u8", 1),
        (0, "audio-surround.m3u8", 1),
    ];
    let result = graph(&[value.as_bytes(), MEDIA], &links, Profile::HlsBaseManifest);
    assert_eq!(result[0].errors, 0);
    let inconsistent = value.replace(
        "GROUP-ID=\"surround\",NAME=\"English\"",
        "GROUP-ID=\"surround\",NAME=\"French\"",
    );
    assert!(has(
        &graph(
            &[inconsistent.as_bytes(), MEDIA],
            &links,
            Profile::HlsBaseManifest
        )[0],
        "HLS-RENDITION-GROUPS"
    ));
}

#[test]
fn graph_target_duration_and_playlist_type_follow_rfc_vod_exceptions() {
    let value = master(
        "#EXT-X-STREAM-INF:BANDWIDTH=1000000\nfirst.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000000\nsecond.m3u8",
    );
    let links = [(0, "first.m3u8", 1), (0, "second.m3u8", 2)];
    let longer = std::str::from_utf8(MEDIA)
        .unwrap()
        .replace("TARGETDURATION:2", "TARGETDURATION:6");
    let mismatch = graph(
        &[value.as_bytes(), MEDIA, longer.as_bytes()],
        &links,
        Profile::HlsBaseManifest,
    );
    assert!(has(&mismatch[0], "HLS-GRAPH-TARGET-DURATIONS"));
    let missing_type = std::str::from_utf8(MEDIA)
        .unwrap()
        .replace("#EXT-X-PLAYLIST-TYPE:VOD\n", "");
    assert!(has(
        &graph(
            &[value.as_bytes(), MEDIA, missing_type.as_bytes()],
            &links,
            Profile::HlsBaseManifest
        )[0],
        "HLS-GRAPH-PLAYLIST-TYPES"
    ));
    let subtitles = master(
        "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"s\",NAME=\"English\",URI=\"second.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,SUBTITLES=\"s\"\nfirst.m3u8",
    );
    assert_eq!(
        graph(
            &[subtitles.as_bytes(), MEDIA, longer.as_bytes()],
            &links,
            Profile::HlsBaseManifest
        )[0]
        .errors,
        0
    );
    let event_first = std::str::from_utf8(MEDIA)
        .unwrap()
        .replace("PLAYLIST-TYPE:VOD", "PLAYLIST-TYPE:EVENT");
    let event_second = longer.replace("PLAYLIST-TYPE:VOD", "PLAYLIST-TYPE:EVENT");
    assert!(has(
        &graph(
            &[
                subtitles.as_bytes(),
                event_first.as_bytes(),
                event_second.as_bytes()
            ],
            &links,
            Profile::HlsBaseManifest
        )[0],
        "HLS-GRAPH-TARGET-DURATIONS"
    ));
    let iframe_master = master(
        "#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=100,URI=\"second.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nfirst.m3u8",
    );
    let iframe = longer.replace(
        "#EXT-X-TARGETDURATION",
        "#EXT-X-I-FRAMES-ONLY\n#EXT-X-TARGETDURATION",
    );
    assert_eq!(
        graph(
            &[iframe_master.as_bytes(), MEDIA, iframe.as_bytes()],
            &links,
            Profile::HlsBaseManifest
        )[0]
        .errors,
        0
    );
}

#[test]
fn closed_caption_services_require_seven_and_iframe_references_require_iframe_children() {
    let declaration = "#EXT-X-MEDIA:TYPE=CLOSED-CAPTIONS,GROUP-ID=\"cc\",NAME=\"English\",INSTREAM-ID=\"SERVICE63\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,CLOSED-CAPTIONS=\"cc\"\nchild.m3u8";
    let value = master(declaration);
    assert_eq!(
        graph(
            &[value.as_bytes(), MEDIA],
            &[(0, "child.m3u8", 1)],
            Profile::HlsBaseManifest
        )[0]
        .errors,
        0
    );
    let low = value.replace("VERSION:7", "VERSION:6");
    assert!(has(
        &graph(
            &[low.as_bytes(), MEDIA],
            &[(0, "child.m3u8", 1)],
            Profile::HlsBaseManifest
        )[0],
        "HLS-VERSION-MINIMUM"
    ));
    let invalid = value.replace("SERVICE63", "SERVICE64");
    assert!(has(
        &graph(
            &[invalid.as_bytes(), MEDIA],
            &[(0, "child.m3u8", 1)],
            Profile::HlsBaseManifest
        )[0],
        "HLS-RENDITION"
    ));
    let iframe = master("#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=1000000,URI=\"child.m3u8\"");
    assert!(has(
        &graph(
            &[iframe.as_bytes(), MEDIA],
            &[(0, "child.m3u8", 1)],
            Profile::HlsBaseManifest
        )[0],
        "HLS-GRAPH-PLAYLIST-KIND"
    ));
    let child = std::str::from_utf8(MEDIA).unwrap().replace(
        "#EXT-X-TARGETDURATION",
        "#EXT-X-I-FRAMES-ONLY\n#EXT-X-TARGETDURATION",
    );
    assert_eq!(
        graph(
            &[iframe.as_bytes(), child.as_bytes()],
            &[(0, "child.m3u8", 1)],
            Profile::HlsBaseManifest
        )[0]
        .errors,
        0
    );
}

#[test]
fn session_data_empty_strings_are_valid_but_duplicate_id_and_language_are_not() {
    let tags = "#EXT-X-SESSION-DATA:DATA-ID=\"example.title\",VALUE=\"\"";
    let value = master(&format!("{tags}\n{}", variant()));
    assert_eq!(
        graph(
            &[value.as_bytes(), MEDIA],
            &[(0, "child.m3u8", 1)],
            Profile::HlsBaseManifest
        )[0]
        .errors,
        0
    );
    let duplicate = master(&format!("{tags}\n{tags}\n{}", variant()));
    assert!(has(
        &graph(
            &[duplicate.as_bytes(), MEDIA],
            &[(0, "child.m3u8", 1)],
            Profile::HlsBaseManifest
        )[0],
        "HLS-SESSION-DATA-DUPLICATES"
    ));
    let both = value.replace("VALUE=\"\"", "VALUE=\"\",URI=\"metadata.json\"");
    assert!(has(
        &graph(
            &[both.as_bytes(), MEDIA],
            &[(0, "child.m3u8", 1)],
            Profile::HlsBaseManifest
        )[0],
        "HLS-SESSION-DATA"
    ));
}

#[test]
fn session_keys_match_child_method_format_and_versions_without_exposing_identifiers() {
    let declaration = "METHOD=SAMPLE-AES,URI=\"skd://private-identifier\",KEYFORMAT=\"com.apple.streamingkeydelivery\",KEYFORMATVERSIONS=\"1\"";
    let value = master(&format!("#EXT-X-SESSION-KEY:{declaration}\n{}", variant()));
    let child = std::str::from_utf8(MEDIA).unwrap().replace(
        "#EXTINF:2,",
        &format!("#EXT-X-KEY:{declaration}\n#EXTINF:2,"),
    );
    let good = graph(
        &[value.as_bytes(), child.as_bytes()],
        &[(0, "child.m3u8", 1)],
        Profile::HlsBaseManifest,
    );
    assert_eq!(good[0].errors, 0);
    let mismatch = child.replace("VERSIONS=\"1\"", "VERSIONS=\"2\"");
    let result = graph(
        &[value.as_bytes(), mismatch.as_bytes()],
        &[(0, "child.m3u8", 1)],
        Profile::HlsBaseManifest,
    );
    assert!(has(&result[0], "HLS-SESSION-KEY-MATCH"));
    assert!(!format!("{:?}", result[0].diagnostics).contains("private-identifier"));
    let duplicate = value.replace(
        "#EXT-X-STREAM-INF",
        &format!("#EXT-X-SESSION-KEY:{declaration}\n#EXT-X-STREAM-INF"),
    );
    assert!(has(
        &graph(
            &[duplicate.as_bytes(), child.as_bytes()],
            &[(0, "child.m3u8", 1)],
            Profile::HlsBaseManifest
        )[0],
        "HLS-SESSION-KEY-DUPLICATES"
    ));
}

#[test]
fn relative_session_key_addresses_need_a_supplied_reference_base() {
    let value = master(&format!(
        "#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"key.bin\"\n{}",
        variant()
    ));
    let child = std::str::from_utf8(MEDIA).unwrap().replace(
        "#EXTINF:2,",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\"\n#EXTINF:2,",
    );
    let result = graph(
        &[value.as_bytes(), child.as_bytes()],
        &[(0, "child.m3u8", 1)],
        Profile::HlsBaseManifest,
    );
    assert_eq!(result[0].errors, 0);
    assert!(result[0].incomplete);
    assert!(
        result[0].coverage["HLS-SESSION-KEY-MATCH"]
            .not_checked_reasons
            .contains(&NotCheckedReason::MissingReferenceBase)
    );
}

#[test]
fn relative_session_key_matching_uses_each_playlist_retrieval_base() {
    let value = master(&format!(
        "#EXT-X-SESSION-KEY:METHOD=AES-128,URI=\"keys/key.bin\"\n{}",
        variant()
    ));
    let child = std::str::from_utf8(MEDIA).unwrap().replace(
        "#EXTINF:2,",
        "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"../keys/key.bin\"\n#EXTINF:2,",
    );
    let bases = [
        Some("https://example.invalid/root/master.m3u8"),
        Some("https://example.invalid/root/sub/child.m3u8"),
    ];
    let mismatch = graph_with_bases(
        &[value.as_bytes(), child.as_bytes()],
        &bases,
        &[(0, "child.m3u8", 1)],
        Profile::HlsBaseManifest,
    );
    assert!(has(&mismatch[0], "HLS-SESSION-KEY-MATCH"));
    let matching_child = child.replace("METHOD=SAMPLE-AES", "METHOD=AES-128");
    let matching = graph_with_bases(
        &[value.as_bytes(), matching_child.as_bytes()],
        &bases,
        &[(0, "child.m3u8", 1)],
        Profile::HlsBaseManifest,
    );
    assert_eq!(matching[0].errors, 0);
    assert!(!matching[0].incomplete);
    let different_child = child.replace("../keys/key.bin", "keys/key.bin");
    let different = graph_with_bases(
        &[value.as_bytes(), different_child.as_bytes()],
        &bases,
        &[(0, "child.m3u8", 1)],
        Profile::HlsBaseManifest,
    );
    assert_eq!(different[0].errors, 0);
    assert!(!different[0].incomplete);
    let invalid_base = [Some("relative/master.m3u8"), bases[1]];
    let missing = graph_with_bases(
        &[value.as_bytes(), matching_child.as_bytes()],
        &invalid_base,
        &[(0, "child.m3u8", 1)],
        Profile::HlsBaseManifest,
    );
    assert!(missing[0].incomplete);
    assert!(
        missing[0].coverage["HLS-SESSION-KEY-MATCH"]
            .not_checked_reasons
            .contains(&NotCheckedReason::MissingReferenceBase)
    );
    let secret = "https://example.invalid/private-token/master.m3u8";
    let debug = format!(
        "{:?}",
        Document {
            id: DocumentId(0),
            format: Format::Hls,
            bytes: value.as_bytes(),
            base_uri: Some(secret)
        }
    );
    assert!(!debug.contains("private-token"));
}

#[test]
fn session_keys_do_not_raise_the_master_compatibility_version() {
    let value = format!(
        "#EXTM3U\n#EXT-X-SESSION-KEY:METHOD=SAMPLE-AES,URI=\"skd://fixture\",KEYFORMAT=\"com.apple.streamingkeydelivery\",KEYFORMATVERSIONS=\"1\",IV=0x1\n{}\n",
        variant()
    );
    let result = graph(
        &[value.as_bytes(), MEDIA],
        &[(0, "child.m3u8", 1)],
        Profile::HlsBaseManifest,
    );
    assert_eq!(result[0].errors, 0);
    assert!(!result[0].incomplete);
    assert_eq!(
        result[0].coverage["HLS-VERSION-MINIMUM"].status,
        CheckStatus::Passed
    );
}

#[test]
fn declared_bandwidth_values_are_not_compared_without_media_evidence() {
    let value = master("#EXT-X-STREAM-INF:BANDWIDTH=100,AVERAGE-BANDWIDTH=200\nchild.m3u8");
    let child = b"#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXTINF:6.4,\nfirst.ts\n#EXTINF:2.9,\nlast.ts\n#EXT-X-ENDLIST\n";
    let result = graph(
        &[value.as_bytes(), child],
        &[(0, "child.m3u8", 1)],
        Profile::HlsBaseManifest,
    );
    assert_eq!(result[0].errors, 0);
    assert!(!result[0].incomplete);
    assert_eq!(
        result[0].coverage["HLS-MEDIA-EVIDENCE"].status,
        CheckStatus::NotChecked(NotCheckedReason::RequiresMediaEvidence)
    );
}

#[test]
fn apple_vod_profile_checks_declared_attributes_and_distinct_video_bitrates() {
    let tags = "#EXT-X-STREAM-INF:BANDWIDTH=1000000,AVERAGE-BANDWIDTH=900000,CODECS=\"avc1.4d401e\",RESOLUTION=640x360,FRAME-RATE=30\nchild.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000000,AVERAGE-BANDWIDTH=1800000,CODECS=\"avc1.4d401e\",RESOLUTION=1280x720,FRAME-RATE=30\nchild.m3u8";
    let value = master(tags);
    let links = [(0, "child.m3u8", 1)];
    let good = graph(
        &[value.as_bytes(), MEDIA],
        &links,
        Profile::AppleVodManifest,
    );
    assert!(
        good.iter()
            .all(|result| result.errors == 0 && !result.incomplete)
    );
    for removed in [
        ",CODECS=\"avc1.4d401e\"",
        ",RESOLUTION=640x360",
        ",FRAME-RATE=30",
    ] {
        let missing = value.replacen(removed, "", 1);
        assert!(has(
            &graph(
                &[missing.as_bytes(), MEDIA],
                &links,
                Profile::AppleVodManifest
            )[0],
            "APPLE-HLS-VARIANT-ATTRIBUTES"
        ));
    }
    let missing_average = value.replace(",AVERAGE-BANDWIDTH=900000", "");
    assert!(has(
        &graph(
            &[missing_average.as_bytes(), MEDIA],
            &links,
            Profile::AppleVodManifest
        )[0],
        "APPLE-HLS-AVERAGE-BANDWIDTH"
    ));
    let duplicate_rate = value.replace("BANDWIDTH=2000000", "BANDWIDTH=1000000");
    assert!(has(
        &graph(
            &[duplicate_rate.as_bytes(), MEDIA],
            &links,
            Profile::AppleVodManifest
        )[0],
        "APPLE-HLS-VARIANTS"
    ));
}

#[test]
fn apple_video_conditions_use_manifest_evidence_and_preserve_unknown_codec_coverage() {
    let tags = "#EXT-X-STREAM-INF:BANDWIDTH=1000000,AVERAGE-BANDWIDTH=900000,CODECS=\"unknown-codec\",RESOLUTION=640x360\nchild.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000000,AVERAGE-BANDWIDTH=1800000,CODECS=\"unknown-codec\",RESOLUTION=1280x720\nchild.m3u8";
    let value = master(tags);
    let links = [(0, "child.m3u8", 1)];
    assert!(has(
        &graph(
            &[value.as_bytes(), MEDIA],
            &links,
            Profile::AppleVodManifest
        )[0],
        "APPLE-HLS-VARIANT-ATTRIBUTES"
    ));
    let unknown = master(
        "#EXT-X-STREAM-INF:BANDWIDTH=100,AVERAGE-BANDWIDTH=90,CODECS=\"avc1garbage\"\nchild.m3u8",
    );
    let result = graph(
        &[unknown.as_bytes(), MEDIA],
        &links,
        Profile::AppleVodManifest,
    );
    assert_eq!(result[0].errors, 0);
    assert!(result[0].incomplete);
    assert!(
        result[0].coverage["APPLE-HLS-VIDEO-CONDITIONS"]
            .not_checked_reasons
            .contains(&NotCheckedReason::UnsupportedFeature)
    );
    let audio = unknown.replace("avc1garbage", "mp4a.40.2");
    let good = graph(
        &[audio.as_bytes(), MEDIA],
        &links,
        Profile::AppleVodManifest,
    );
    assert!(
        good.iter()
            .all(|result| result.errors == 0 && !result.incomplete)
    );
}

#[test]
fn apple_audio_description_requires_automatic_selection_and_language() {
    let audio = "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Description\",LANGUAGE=\"en\",CHARACTERISTICS=\"public.accessibility.describes-video\",AUTOSELECT=YES,URI=\"audio.m3u8\"";
    let value = master(&format!(
        "{audio}\n#EXT-X-STREAM-INF:BANDWIDTH=100,AVERAGE-BANDWIDTH=90,CODECS=\"mp4a.40.2\",AUDIO=\"audio\"\nchild.m3u8"
    ));
    let links = [(0, "child.m3u8", 1), (0, "audio.m3u8", 1)];
    assert_eq!(
        graph(
            &[value.as_bytes(), MEDIA],
            &links,
            Profile::AppleVodManifest
        )[0]
        .errors,
        0
    );
    let manual = value.replace("AUTOSELECT=YES", "AUTOSELECT=NO");
    assert!(has(
        &graph(
            &[manual.as_bytes(), MEDIA],
            &links,
            Profile::AppleVodManifest
        )[0],
        "APPLE-HLS-AUDIO-DESCRIPTION"
    ));
    let missing = value.replace(",LANGUAGE=\"en\"", "");
    assert!(has(
        &graph(
            &[missing.as_bytes(), MEDIA],
            &links,
            Profile::AppleVodManifest
        )[0],
        "APPLE-HLS-AUDIO-DESCRIPTION"
    ));
}

#[test]
fn apple_iframe_average_bandwidth_has_its_own_authoring_rule() {
    let value = master(
        "#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=1000,AVERAGE-BANDWIDTH=900,CODECS=\"avc1.4d401e\",RESOLUTION=640x360,URI=\"child.m3u8\"",
    );
    let child = std::str::from_utf8(MEDIA).unwrap().replace(
        "#EXT-X-TARGETDURATION",
        "#EXT-X-I-FRAMES-ONLY\n#EXT-X-TARGETDURATION",
    );
    let links = [(0, "child.m3u8", 1)];
    let good = graph(
        &[value.as_bytes(), child.as_bytes()],
        &links,
        Profile::AppleVodManifest,
    );
    assert_eq!(good[0].errors, 0);
    assert_eq!(
        good[0].coverage["APPLE-HLS-IFRAME-AVERAGE-BANDWIDTH"].status,
        CheckStatus::Passed
    );
    let missing = value.replace(",AVERAGE-BANDWIDTH=900", "");
    assert!(has(
        &graph(
            &[missing.as_bytes(), child.as_bytes()],
            &links,
            Profile::AppleVodManifest
        )[0],
        "APPLE-HLS-IFRAME-AVERAGE-BANDWIDTH"
    ));
    let base = graph(
        &[missing.as_bytes(), child.as_bytes()],
        &links,
        Profile::HlsBaseManifest,
    );
    assert_eq!(base[0].errors, 0);
}

#[test]
fn apple_hd_ladder_recommendation_is_a_warning_and_uses_distinct_highest_resolution_bitrates() {
    let low = "#EXT-X-STREAM-INF:BANDWIDTH=1000000,AVERAGE-BANDWIDTH=900000,CODECS=\"avc1.4d401e\",RESOLUTION=640x360,FRAME-RATE=30\nchild.m3u8";
    let high = "#EXT-X-STREAM-INF:BANDWIDTH=2000000,AVERAGE-BANDWIDTH=1800000,CODECS=\"avc1.4d401e\",RESOLUTION=1280x720,FRAME-RATE=30\nchild.m3u8";
    let value = master(&format!("{low}\n{high}"));
    let links = [(0, "child.m3u8", 1)];
    let warning = graph(
        &[value.as_bytes(), MEDIA],
        &links,
        Profile::AppleVodManifest,
    );
    assert_eq!(warning[0].errors, 0);
    assert!(has(&warning[0], "APPLE-HLS-HD-VARIANTS"));
    assert_eq!(
        warning[0].coverage["APPLE-HLS-HD-VARIANTS"].severity,
        Severity::Warning
    );
    let complete = master(&format!(
        "{low}\n{high}\n{}",
        high.replace("BANDWIDTH=2000000", "BANDWIDTH=3000000")
    ));
    assert!(!has(
        &graph(
            &[complete.as_bytes(), MEDIA],
            &links,
            Profile::AppleVodManifest
        )[0],
        "APPLE-HLS-HD-VARIANTS"
    ));
    let below = value.replace("1280x720", "1279x720");
    assert!(!has(
        &graph(
            &[below.as_bytes(), MEDIA],
            &links,
            Profile::AppleVodManifest
        )[0],
        "APPLE-HLS-HD-VARIANTS"
    ));
}

#[test]
fn syntax_only_reference_extraction_ignores_media_and_semantic_version_failures() {
    let bytes = b"#EXTM3U\n#EXT-X-VERSION:1\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"English\",URI=\"audio.m3u8\"\n#EXT-X-SESSION-KEY:METHOD=SAMPLE-AES,URI=\"skd://not-opened\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000,AUDIO=\"a\"\n# ordinary comment\nvideo.m3u8\n#EXT-X-I-FRAME-STREAM-INF:BANDWIDTH=100,URI=\"iframe.m3u8\"\n";
    let references = child_manifest_references(bytes, &crate::Limits::default()).unwrap();
    assert_eq!(
        references
            .iter()
            .map(|reference| reference.uri)
            .collect::<Vec<_>>(),
        ["audio.m3u8", "video.m3u8", "iframe.m3u8"]
    );
    for reference in references {
        assert_eq!(
            &bytes[reference.span.start..reference.span.end],
            reference.uri.as_bytes()
        );
    }
    assert!(
        child_manifest_references(MEDIA, &crate::Limits::default())
            .unwrap()
            .is_empty()
    );
    let malformed = master("#EXT-X-STREAM-INF:BANDWIDTH=1000\n#EXTINF:2,\npart.ts");
    assert_eq!(
        child_manifest_references(malformed.as_bytes(), &crate::Limits::default()).unwrap_err(),
        ReferenceError::InvalidSyntax
    );
    let variable = master("#EXT-X-STREAM-INF:BANDWIDTH=1000\n{$path}.m3u8");
    assert_eq!(
        child_manifest_references(variable.as_bytes(), &crate::Limits::default()).unwrap_err(),
        ReferenceError::UnsupportedFeature
    );
    let limits = crate::Limits {
        max_references: 2,
        ..crate::Limits::default()
    };
    assert_eq!(
        child_manifest_references(bytes, &limits).unwrap_err(),
        ReferenceError::ResourceLimit
    );
}
