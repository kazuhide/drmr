use super::*;
use crate::{CheckStatus, DocumentId, Format, Options, Profile};

fn run(bytes: &[u8], profile: Profile) -> DiagnosticSink<'_> {
    run_limited(bytes, profile, crate::Limits::default())
}

fn run_limited(bytes: &[u8], profile: Profile, limits: crate::Limits) -> DiagnosticSink<'_> {
    let document = Document {
        id: DocumentId(1),
        format: Format::Hls,
        bytes,
        base_uri: None,
    };
    let mut options = Options::new(profile);
    options.limits = limits;
    let context = LintContext::new(&options, None);
    let mut sink = DiagnosticSink::new(document, options.limits.max_diagnostics);
    lint(&document, &context, &mut sink);
    sink
}

#[test]
fn resource_boundaries_and_truncated_reports_preserve_failures_and_positions() {
    let text = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\npart.ts\n";
    let line_count = text.lines().count();
    let longest = text.lines().map(str::len).max().unwrap();
    let limits = crate::Limits {
        max_document_bytes: text.len(),
        max_lines: line_count,
        max_line_bytes: longest,
        max_references: 1,
        ..crate::Limits::default()
    };
    assert_eq!(
        run_limited(text.as_bytes(), Profile::HlsBaseManifest, limits.clone()).errors,
        0
    );
    for field in [0, 1, 2, 3] {
        let mut smaller = limits.clone();
        match field {
            0 => smaller.max_document_bytes -= 1,
            1 => smaller.max_lines -= 1,
            2 => smaller.max_line_bytes -= 1,
            _ => smaller.max_references -= 1,
        }
        let result = run_limited(text.as_bytes(), Profile::HlsBaseManifest, smaller);
        assert_eq!(result.errors, 0);
        assert!(result.incomplete);
        assert!(
            result.coverage["HLS-LIMITS"]
                .not_checked_reasons
                .contains(&NotCheckedReason::ResourceLimit)
        );
    }
    let many = "#EXTM3U\r\n#EXT-X-VERSION:7\r\n#EXT-X-TARGETDURATION:2\r\n#EXT-X-KEY:METHOD=AES-128,URI=\"private-identifier\",IV=\"0x1\"\r\n#EXT-X-KEY:METHOD=NONE,URI=\"private-identifier\"\r\n#EXTINF:99,\r\npart.ts\r\n";
    let capped = crate::Limits {
        max_diagnostics: 2,
        ..crate::Limits::default()
    };
    let result = run_limited(many.as_bytes(), Profile::HlsBaseManifest, capped);
    assert!(result.truncated && result.incomplete);
    assert!(result.errors > result.diagnostics.len());
    assert_eq!(result.diagnostics.len(), 2);
    let typed = result
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.rule_id == "HLS-ATTRIBUTE-TYPES")
        .unwrap();
    let location = typed.location.unwrap();
    assert_eq!(location.line, 4);
    assert_eq!(&many[location.span.start..location.span.end], "0x1");
    assert!(!format!("{:?}", result.diagnostics).contains("private-identifier"));
    let zero = crate::Limits {
        max_diagnostics: 0,
        ..crate::Limits::default()
    };
    let empty = run_limited(many.as_bytes(), Profile::HlsBaseManifest, zero);
    assert!(
        empty.diagnostics.is_empty() && empty.errors > 0 && empty.truncated && empty.incomplete
    );
}

#[test]
fn singleton_tag_start_and_sequence_boundaries_are_checked_independently() {
    let base =
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXTINF:2,\npart.ts\n#EXT-X-ENDLIST\n";
    for tag in [
        "#EXT-X-VERSION:7",
        "#EXT-X-TARGETDURATION:2",
        "#EXT-X-MEDIA-SEQUENCE:0",
        "#EXT-X-DISCONTINUITY-SEQUENCE:0",
        "#EXT-X-ENDLIST",
        "#EXT-X-PLAYLIST-TYPE:VOD",
        "#EXT-X-I-FRAMES-ONLY",
        "#EXT-X-INDEPENDENT-SEGMENTS",
        "#EXT-X-START:TIME-OFFSET=-0.5,PRECISE=YES",
    ] {
        let text = base.replace(
            "#EXT-X-VERSION:7\n",
            &format!("#EXT-X-VERSION:7\n{tag}\n{tag}\n"),
        );
        assert!(
            run(text.as_bytes(), Profile::HlsBaseManifest)
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.rule_id == "HLS-TAG-CARDINALITY")
        );
    }
    for offset in ["0", "-0.5", ".5", "-1."] {
        let text = base.replace(
            "#EXTINF:2,",
            &format!("#EXT-X-START:TIME-OFFSET={offset},PRECISE=YES\n#EXTINF:2,"),
        );
        assert_eq!(run(text.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    }
    for start in [
        "TIME-OFFSET=NaN",
        "TIME-OFFSET=1e3",
        "TIME-OFFSET=--1",
        "TIME-OFFSET=1,PRECISE=MAYBE",
        "PRECISE=YES",
    ] {
        let text = base.replace("#EXTINF:2,", &format!("#EXT-X-START:{start}\n#EXTINF:2,"));
        assert!(
            run(text.as_bytes(), Profile::HlsBaseManifest)
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.rule_id == "HLS-START")
        );
    }
    let before = base.replace("#EXTINF:2,", "#EXT-X-MEDIA-SEQUENCE:18446744073709551615\n#EXT-X-DISCONTINUITY-SEQUENCE:0\n#EXT-X-DISCONTINUITY\n#EXTINF:2,");
    assert_eq!(run(before.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    for text in [
        format!("{base}#EXT-X-MEDIA-SEQUENCE:1\n"),
        base.replace(
            "#EXTINF:2,",
            "#EXT-X-DISCONTINUITY\n#EXT-X-DISCONTINUITY-SEQUENCE:0\n#EXTINF:2,",
        ),
    ] {
        assert!(
            run(text.as_bytes(), Profile::HlsBaseManifest)
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.rule_id == "HLS-SEQUENCE")
        );
    }
    let valued = base.replace("#EXT-X-ENDLIST", "#EXT-X-ENDLIST:unexpected");
    assert!(
        run(valued.as_bytes(), Profile::HlsBaseManifest)
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule_id == "HLS-TAG-SYNTAX")
    );
    let mixed = base.replace("#EXTINF:2,", "#EXT-X-STREAM-INF:BANDWIDTH=1000\n#EXTINF:2,");
    assert!(
        run(mixed.as_bytes(), Profile::HlsBaseManifest)
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule_id == "HLS-PLAYLIST-KIND")
    );
}

fn media(version: u32) -> String {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:{version}\n#EXT-X-TARGETDURATION:2\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\npart.m4s\n#EXT-X-ENDLIST\n"
    )
}

#[test]
fn effective_key_methods_must_agree_and_none_resets_all_formats() {
    let value = media(7).replace("#EXT-X-MAP", "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",KEYFORMAT=\"format-one\",IV=0x1\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://fixture\",KEYFORMAT=\"format-two\"\n#EXT-X-MAP");
    let mixed = run(value.as_bytes(), Profile::HlsBaseManifest);
    assert!(
        mixed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule_id == "HLS-KEY-METHOD-CONSISTENCY")
    );
    let reset = value.replace("#EXT-X-MAP", "#EXT-X-KEY:METHOD=NONE\n#EXT-X-MAP");
    assert_eq!(run(reset.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    let replaced = value.replace("KEYFORMAT=\"format-two\"", "KEYFORMAT=\"format-one\"");
    assert_eq!(run(replaced.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    let future = value.replace("METHOD=SAMPLE-AES,", "METHOD=FUTURE-AES,");
    let incomplete = run(future.as_bytes(), Profile::HlsBaseManifest);
    assert!(
        incomplete.coverage["HLS-KEY-METHOD-CONSISTENCY"]
            .not_checked_reasons
            .contains(&NotCheckedReason::UnsupportedFeature)
    );
}

#[test]
fn ctr_fmp4_declarations_are_complete_at_the_feature_derived_version() {
    for version in [6, 7] {
        let value = media(version).replace(
            "#EXT-X-MAP",
            "#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"key.bin\"\n#EXT-X-MAP",
        );
        let result = run(value.as_bytes(), Profile::HlsBaseManifest);
        assert_eq!(result.errors, 0);
        assert!(!result.incomplete, "valid CTR declarations must be checked");
        for id in ["HLS-CTR-KEY", "HLS-CTR-MAP", "HLS-KEY-METHOD-CONSISTENCY"] {
            assert_eq!(result.coverage[id].status, CheckStatus::Passed);
        }
        let reference = &result.coverage["HLS-CTR-KEY"].reference;
        assert_eq!(
            reference.url,
            "https://datatracker.ietf.org/doc/html/draft-pantos-hls-rfc8216bis-22"
        );
        assert_eq!(reference.section, "4.4.4.4");
        assert_eq!(
            reference.revision,
            "draft-pantos-hls-rfc8216bis-22 (2026-05-01, work in progress)"
        );
    }
    let too_old = media(5).replace(
        "#EXT-X-MAP",
        "#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"key.bin\"\n#EXT-X-MAP",
    );
    assert!(has(
        &run(too_old.as_bytes(), Profile::HlsBaseManifest),
        "HLS-VERSION-MINIMUM"
    ));
    assert_eq!(Profile::HlsBaseManifest.revision(), "2026-10-30.1");
    assert_eq!(Profile::AppleVodManifest.revision(), "2026-10-30.1");
    assert_eq!(Profile::DashStaticManifest.revision(), "2026-10-26.1");
}

#[test]
fn ctr_requires_an_applicable_map_for_each_encrypted_segment() {
    let declaration = "#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"key.bin\"";
    for body in [
        format!("{declaration}\n#EXTINF:2,\npart.ts"),
        format!(
            "{declaration}\n#EXTINF:2,\nearlier.ts\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\nlater.m4s"
        ),
        format!(
            "#EXT-X-MAP:URI=\"init.mp4\"\n{declaration}\n#EXT-X-MAP:BYTERANGE=\"16@0\"\n#EXTINF:2,\npart.m4s"
        ),
    ] {
        let value = plain(&body);
        let result = run(value.as_bytes(), Profile::HlsBaseManifest);
        assert!(has(&result, "HLS-CTR-MAP"), "{body}");
        let failure = result
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.rule_id == "HLS-CTR-MAP")
            .unwrap();
        let span = failure.location.unwrap().span;
        assert!(matches!(
            &value[span.start..span.end],
            "part.ts" | "earlier.ts" | "part.m4s"
        ));
    }
    let value = plain(&format!(
        "#EXTINF:2,\nclear.ts\n#EXT-X-MAP:URI=\"init.mp4\"\n{declaration}\n#EXTINF:2,\npart.m4s"
    ));
    let result = run(value.as_bytes(), Profile::HlsBaseManifest);
    assert_eq!(result.errors, 0);
    assert!(!result.incomplete);
}

#[test]
fn ctr_effective_methods_must_agree_and_replacement_or_none_removes_the_method() {
    let matching = media(6).replace("#EXT-X-MAP", "#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"wv.bin\",KEYFORMAT=\"urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed\",KEYFORMATVERSIONS=\"1\"\n#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"pr.bin\",KEYFORMAT=\"com.microsoft.playready\",KEYFORMATVERSIONS=\"1\"\n#EXT-X-MAP");
    let result = run(matching.as_bytes(), Profile::HlsBaseManifest);
    assert_eq!(result.errors, 0);
    assert!(!result.incomplete);
    for other in ["AES-128,IV=0x1", "SAMPLE-AES"] {
        let value = media(7).replace("#EXT-X-MAP", &format!("#EXT-X-KEY:METHOD={other},URI=\"key-one.bin\",KEYFORMAT=\"one\"\n#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"key-two.bin\",KEYFORMAT=\"two\"\n#EXT-X-MAP"));
        let result = run(value.as_bytes(), Profile::HlsBaseManifest);
        assert_eq!(
            result.coverage["HLS-KEY-METHOD-CONSISTENCY"].status,
            CheckStatus::Failed
        );
        assert!(!result.incomplete);
    }
    for replacement in [
        "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"replacement.bin\"",
        "#EXT-X-KEY:METHOD=NONE",
    ] {
        let value = plain(&format!(
            "#EXT-X-KEY:METHOD=SAMPLE-AES-CTR,URI=\"key.bin\"\n{replacement}\n#EXTINF:2,\npart.ts"
        ));
        let result = run(value.as_bytes(), Profile::HlsBaseManifest);
        assert_eq!(result.errors, 0);
        assert!(!result.incomplete);
        assert!(!result.coverage.contains_key("HLS-CTR-MAP"));
    }
}

#[test]
fn ctr_iv_is_forbidden_on_media_and_session_keys_and_apple_rejects_ctr() {
    for tag in ["#EXT-X-KEY", "#EXT-X-SESSION-KEY"] {
        let template = if tag == "#EXT-X-KEY" {
            media(7).replace("#EXT-X-MAP", "KEY-DECLARATION\n#EXT-X-MAP")
        } else {
            "#EXTM3U\n#EXT-X-VERSION:7\nKEY-DECLARATION\n#EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"mp4a.40.2\",AVERAGE-BANDWIDTH=120000\nchild.m3u8\n".to_owned()
        };
        let declaration = format!("{tag}:METHOD=SAMPLE-AES-CTR,URI=\"key.bin\"");
        let valid = template.replace("KEY-DECLARATION", &declaration);
        let base = run(valid.as_bytes(), Profile::HlsBaseManifest);
        assert_eq!(base.coverage["HLS-CTR-KEY"].status, CheckStatus::Passed);
        assert!(!has(&base, "APPLE-HLS-CTR"));
        let invalid = template.replace("KEY-DECLARATION", &format!("{declaration},IV=0x1"));
        for profile in [Profile::HlsBaseManifest, Profile::AppleVodManifest] {
            assert!(has(&run(invalid.as_bytes(), profile), "HLS-CTR-KEY"));
        }
        let apple = run(valid.as_bytes(), Profile::AppleVodManifest);
        assert!(has(&apple, "APPLE-HLS-CTR"));
        let coverage = &apple.coverage["APPLE-HLS-CTR"];
        assert_eq!(coverage.status, CheckStatus::Failed);
        assert_eq!(coverage.reference.section, "13.11");
        assert_eq!(coverage.reference.revision, "retrieved 2026-10-30");
    }
}

#[test]
fn decimal_integer_grammar_preserves_the_rfc_unsigned_range_and_length() {
    let zero = "#EXTM3U\n#EXT-X-TARGETDURATION:0\n#EXTINF:0,\npart.ts\n";
    assert_eq!(run(zero.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    let too_long = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:000000000000000000002,\npart.ts\n";
    assert!(
        run(too_long.as_bytes(), Profile::HlsBaseManifest)
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule_id == "HLS-EXTINF")
    );
    let largest = "#EXTM3U\n#EXT-X-TARGETDURATION:18446744073709551615\n#EXTINF:18446744073709551615,\npart.ts\n";
    assert_eq!(run(largest.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    let overflow = largest.replace("18446744073709551615", "18446744073709551616");
    assert!(run(overflow.as_bytes(), Profile::HlsBaseManifest).errors > 0);
}

#[test]
fn ordinary_map_requires_six_but_accepts_six_and_seven() {
    let too_old = media(5);
    let result = run(too_old.as_bytes(), Profile::HlsBaseManifest);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.rule_id == "HLS-VERSION-MINIMUM")
    );
    for version in [6, 7] {
        let value = media(version);
        let result = run(value.as_bytes(), Profile::HlsBaseManifest);
        assert_eq!(result.errors, 0);
        assert!(!result.incomplete);
        assert_eq!(
            result.coverage["HLS-VERSION-MINIMUM"].status,
            CheckStatus::Passed
        );
    }
}

fn has(result: &DiagnosticSink<'_>, id: &str) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.rule_id == id)
}

fn plain(body: &str) -> String {
    format!("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n{body}\n#EXT-X-ENDLIST\n")
}

#[test]
fn text_grammar_reports_encoding_layout_normalization_and_header_separately() {
    for (bytes, expected) in [
        (b"#EXTM3U\n\xff".as_slice(), "HLS-UTF8"),
        (b"\xef\xbb\xbf#EXTM3U\n".as_slice(), "HLS-TEXT"),
        (b"#EXTM3U\n#\tcomment\n".as_slice(), "HLS-TEXT"),
        ("#EXTM3U\n# e\u{301}\n".as_bytes(), "HLS-NFC"),
        (b"#EXTM3U\r# comment\n".as_slice(), "HLS-TEXT"),
        (b"\n#EXTM3U\n".as_slice(), "HLS-HEADER"),
    ] {
        assert!(
            has(&run(bytes, Profile::HlsBaseManifest), expected),
            "missing rule {expected}"
        );
    }
    let value = plain("#EXTINF:2,Caf\u{e9}\npart.ts").replace('\n', "\r\n");
    assert_eq!(run(value.as_bytes(), Profile::HlsBaseManifest).errors, 0);
}

#[test]
fn segment_association_and_numeric_grammar_are_checked_without_float_rounding() {
    for duration in [
        "2",
        "2.4999999999999999999999999999999999",
        "0002.49",
        "0.001",
    ] {
        let value = plain(&format!("#EXTINF:{duration},\npart.ts"));
        assert_eq!(
            run(value.as_bytes(), Profile::HlsBaseManifest).errors,
            0,
            "valid duration {duration}"
        );
    }
    for duration in ["2.5", "3", "18446744073709551616000000000.1"] {
        let value = plain(&format!("#EXTINF:{duration},\npart.ts"));
        assert!(has(
            &run(value.as_bytes(), Profile::HlsBaseManifest),
            "HLS-TARGET-DURATION"
        ));
    }
    for duration in ["NaN", "inf", "2e0", "-1", "1.2.3"] {
        let value = plain(&format!("#EXTINF:{duration},\npart.ts"));
        assert!(has(
            &run(value.as_bytes(), Profile::HlsBaseManifest),
            "HLS-EXTINF"
        ));
    }
    for body in ["part.ts", "#EXTINF:2,", "#EXTINF:2,\n#EXTINF:2,\npart.ts"] {
        assert!(has(
            &run(plain(body).as_bytes(), Profile::HlsBaseManifest),
            "HLS-SEGMENT-ASSOCIATION"
        ));
    }
}

#[test]
fn endlist_can_precede_segments_and_live_is_not_a_base_grammar_error() {
    let before = "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-ENDLIST\n#EXTINF:2,\npart.ts\n";
    assert_eq!(run(before.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    let live = before.replace("#EXT-X-ENDLIST\n", "");
    assert_eq!(run(live.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    assert!(has(
        &run(live.as_bytes(), Profile::AppleVodManifest),
        "APPLE-HLS-VOD"
    ));
}

#[test]
fn attribute_types_duplicates_and_encrypted_map_state_are_checked() {
    for (key, expected) in [
        ("METHOD=AES-128,URI=unquoted", "HLS-ATTRIBUTE-TYPES"),
        ("METHOD=\"AES-128\",URI=\"key\"", "HLS-ATTRIBUTE-TYPES"),
        (
            "METHOD=AES-128,METHOD=NONE,URI=\"key\"",
            "HLS-ATTRIBUTES-UNIQUE",
        ),
        ("METHOD=AES-128,URI=\"unterminated", "HLS-ATTRIBUTES"),
        ("METHOD=NONE,URI=\"key\"", "HLS-KEY"),
        ("METHOD=AES-128", "HLS-KEY"),
        ("METHOD=AES-128,URI=\"key\",IV=0xnothex", "HLS-KEY"),
    ] {
        let value = plain(&format!("#EXT-X-KEY:{key}\n#EXTINF:2,\npart.ts"));
        assert!(
            has(&run(value.as_bytes(), Profile::HlsBaseManifest), expected),
            "missing {expected}"
        );
    }
    let missing = plain(
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:2,\npart.m4s",
    );
    assert!(has(
        &run(missing.as_bytes(), Profile::HlsBaseManifest),
        "HLS-MAP-KEY-IV"
    ));
    let short = missing.replace("URI=\"key\"", "URI=\"key\",IV=0X1");
    assert_eq!(run(short.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    let later = missing.replace(
        "#EXTINF:2,",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"key\",IV=0x1\n#EXTINF:2,",
    );
    assert!(has(
        &run(later.as_bytes(), Profile::HlsBaseManifest),
        "HLS-MAP-KEY-IV"
    ));
}

#[test]
fn byte_ranges_use_only_the_previous_segment_of_the_same_resource() {
    let valid = plain(
        "#EXTINF:2,\n#EXT-X-BYTERANGE:10@0\npart.ts\n#EXTINF:2,\n#EXT-X-BYTERANGE:10\npart.ts",
    );
    assert_eq!(run(valid.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    for body in [
        "#EXTINF:2,\n#EXT-X-BYTERANGE:10\npart.ts",
        "#EXTINF:2,\n#EXT-X-BYTERANGE:10@0\npart.ts\n#EXTINF:2,\n#EXT-X-BYTERANGE:10\nother.ts",
        "#EXTINF:2,\n#EXT-X-BYTERANGE:18446744073709551616@1\npart.ts",
    ] {
        assert!(has(
            &run(plain(body).as_bytes(), Profile::HlsBaseManifest),
            "HLS-BYTE-RANGE"
        ));
    }
}

#[test]
fn byte_range_end_calculation_does_not_invent_a_u64_end_exclusive_constraint() {
    let text = "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"init.mp4\",BYTERANGE=\"1@18446744073709551615\"\n#EXT-X-BYTERANGE:1@18446744073709551615\n#EXTINF:2,\npart.m4s\n#EXT-X-BYTERANGE:1\n#EXTINF:2,\npart.m4s\n";
    assert_eq!(run(text.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    let invalid = text.replace("18446744073709551615", "18446744073709551616");
    assert!(run(invalid.as_bytes(), Profile::HlsBaseManifest).errors > 0);
}

#[test]
fn feature_minimum_versions_follow_the_iframe_map_exception() {
    let value = media(5).replace("#EXT-X-MAP", "#EXT-X-I-FRAMES-ONLY\n#EXT-X-MAP");
    assert_eq!(run(value.as_bytes(), Profile::HlsBaseManifest).errors, 0);
    assert!(has(
        &run(
            value.replace("VERSION:5", "VERSION:4").as_bytes(),
            Profile::HlsBaseManifest
        ),
        "HLS-VERSION-MINIMUM"
    ));
    for (version, feature) in [
        (
            1,
            "#EXT-X-KEY:METHOD=AES-128,URI=\"key\",IV=0x1\n#EXTINF:2,\npart.ts",
        ),
        (2, "#EXTINF:2.0,\npart.ts"),
        (3, "#EXT-X-BYTERANGE:10@0\n#EXTINF:2,\npart.ts"),
        (
            4,
            "#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://fixture\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:2,\npart.ts",
        ),
    ] {
        let value = plain(feature).replace("VERSION:7", &format!("VERSION:{version}"));
        assert!(has(
            &run(value.as_bytes(), Profile::HlsBaseManifest),
            "HLS-VERSION-MINIMUM"
        ));
        assert!(!has(
            &run(
                value
                    .replace(
                        &format!("VERSION:{version}"),
                        &format!("VERSION:{}", version + 1)
                    )
                    .as_bytes(),
                Profile::HlsBaseManifest
            ),
            "HLS-VERSION-MINIMUM"
        ));
    }
}

#[test]
fn unknown_tags_and_attributes_are_explicit_incomplete_coverage() {
    for body in [
        "#EXT-X-FUTURE:private-operator-value\n#EXTINF:2,\npart.ts",
        "#EXT-X-KEY:METHOD=AES-128,URI=\"private-operator-value\",FUTURE=YES\n#EXTINF:2,\npart.ts",
    ] {
        let value = plain(body);
        let result = run(value.as_bytes(), Profile::HlsBaseManifest);
        assert_eq!(result.errors, 0);
        assert!(result.incomplete);
        assert!(!format!("{:?}", result.diagnostics).contains("private-operator-value"));
    }
}
