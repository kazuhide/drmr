use drmr_manifest::{
    CheckStatus, Document, DocumentId, Format, Options, Profile, Report, lint_document,
};
use std::{fmt::Write, num::NonZeroUsize};

const VALID: &str = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" profiles="urn:mpeg:dash:profile:isoff-live:2011" minBufferTime="PT1S" mediaPresentationDuration="PT8S"><Period><AdaptationSet><Representation id="v" bandwidth="1"><SegmentTemplate timescale="1" duration="2" media="s-$Number$" initialization="init"/></Representation></AdaptationSet></Period></MPD>"#;

fn escaped(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars() {
        result.push_str(match character {
            '&' => "&amp;",
            '<' => "&lt;",
            '>' => "&gt;",
            '"' => "&quot;",
            '\t' => "&#9;",
            '\r' => "&#13;",
            '\n' => "&#10;",
            _ => {
                result.push(character);
                continue;
            }
        });
    }
    result
}

fn fixture(element: &str, attribute: &str, value: &str) -> String {
    let attribute = format!("{attribute}=\"{}\"", escaped(value));
    match element {
        "MPD" => VALID.replace(
            "profiles=\"urn:mpeg:dash:profile:isoff-live:2011\"",
            &attribute,
        ),
        "Label" => VALID.replace(
            "<AdaptationSet>",
            &format!("<AdaptationSet><Label {attribute}/>"),
        ),
        "ProgramInformation" => VALID.replace(
            "<Period>",
            &format!("<ProgramInformation {attribute}/><Period>"),
        ),
        "Initialization" | "RepresentationIndex" | "BitstreamSwitching" => VALID.replace(
            "initialization=\"init\"/>",
            &format!("initialization=\"init\"><{element} {attribute}/></SegmentTemplate>"),
        ),
        _ => VALID.replace(&format!("<{element}"), &format!("<{element} {attribute}")),
    }
}

fn lint(text: &str, workers: usize) -> Report {
    let mut options = Options::new(Profile::DashStaticManifest);
    options.workers = NonZeroUsize::new(workers);
    lint_document(
        Document {
            id: DocumentId(1),
            format: Format::Dash,
            bytes: text.as_bytes(),
            base_uri: None,
        },
        &options,
    )
    .unwrap()
}

fn accepted(element: &str, attribute: &str, value: &str) {
    let report = lint(&fixture(element, attribute, value), 1);
    assert!(
        report.is_clean_for_scope(),
        "{element}@{attribute}: {report:?}"
    );
}

fn rejected(element: &str, attribute: &str, value: &str) {
    let report = lint(&fixture(element, attribute, value), 1);
    assert!(
        !report.is_clean_for_scope(),
        "{element}@{attribute} was falsely clean"
    );
    assert!(report.errors > 0 && !report.incomplete, "{report:?}");
    assert!(
        report.coverage.iter().any(|item| {
            item.rule_id == "DASH-ATTRIBUTE-LEXICAL" && item.status == CheckStatus::Failed
        }),
        "{report:?}"
    );
}

#[test]
fn language_uses_the_xml_schema_token_pattern_in_each_known_context() {
    for element in ["AdaptationSet", "Label", "ProgramInformation"] {
        for value in [
            "en",
            "en-US",
            "zh-Hant",
            "abcdefgh-A1b2C3d4",
            " \ten-US\r\n ",
        ] {
            accepted(element, "lang", value);
        }
        for value in [
            "",
            " ",
            "en_US",
            "en US",
            "123",
            "abcdefghi",
            "en-abcdefghi",
            "en--US",
            "en-",
            "日本語",
            "en\u{a0}US",
        ] {
            rejected(element, "lang", value);
        }
    }
}

#[test]
fn sampling_rates_are_one_or_two_xml_whitespace_separated_unsigned_ints() {
    for element in ["AdaptationSet", "Representation"] {
        for value in [
            "0",
            "00001",
            "4294967295",
            " \t48000\r\n44100 ",
            "48000  44100",
        ] {
            accepted(element, "audioSamplingRate", value);
        }
        accepted(
            element,
            "audioSamplingRate",
            &format!("{}1", "0".repeat(100)),
        );
        for value in [
            "",
            " ",
            "-1",
            "+48000",
            "-0",
            "-000",
            "4294967296",
            "48000 bad",
            "48000 44100 32000",
            "48000,44100",
            "1.0",
            "1e3",
            "++1",
            "48000\u{a0}44100",
        ] {
            rejected(element, "audioSamplingRate", value);
        }
    }
}

#[test]
fn content_type_is_the_pinned_six_value_enum_without_string_whitespace_folding() {
    for value in ["text", "image", "audio", "video", "application", "font"] {
        accepted("AdaptationSet", "contentType", value);
    }
    for value in ["", "Video", " video", "video ", "model", "audio/video"] {
        rejected("AdaptationSet", "contentType", value);
    }
}

#[test]
fn codecs_accept_both_pinned_pattern_alternatives_and_reject_malformed_lists() {
    for element in ["AdaptationSet", "Representation"] {
        for value in [
            "avc1.4d401f",
            "avc1.4d401f,mp4a.40.2",
            "a'%GG",
            r"a\b",
            r"utf'en'{item}\b",
            "utf'en'{item}.%2F",
            "utf-x'en-US'{a},b.%ff",
            "utf'en'{item}..x",
            "$-_.+^|'`%!*#~&",
        ] {
            accepted(element, "codecs", value);
        }
        for value in [
            "",
            ",",
            "avc1,",
            ",avc1",
            "avc1,,mp4a",
            "avc1 mp4a",
            "avc1, mp4a",
            "avc1;mp4a",
            "{item}",
            "utf8'en'{item}",
            "utf'en-12'{item}",
            "utf'en'{item}.%GG",
            "utf'en'{item}/x",
            "日本語",
        ] {
            rejected(element, "codecs", value);
        }
    }
}

#[test]
fn profile_lists_preserve_the_exact_pinned_uri_language_and_comma_ambiguity() {
    for element in ["MPD", "AdaptationSet", "Representation"] {
        for value in [
            "urn:mpeg:dash:profile:isoff-live:2011",
            "urn:ab:a#b#c",
            "relative",
            "/relative/path/",
            "x:",
            "https://example.invalid/a,b",
            "https://example.invalid/a#one,https://other.invalid/b#two",
            "urn:ab:first,  https://example.invalid/second",
            "https://example.invalid/%GG",
            "https://user:password@example.invalid:65535/path?q/a?b#f/g?h",
            "https://[2001:db8::1]/path",
            "https://[::]/",
            "https://[::ffff:192.0.2.1]/",
            "https://[::ffff0000:192.0.2.1]/",
            "https://[::192x0x2x1]/",
            "https://[::192界0界2界1]/",
            "https://[fe80:%1]/",
            "https://[v1.example]/",
        ] {
            accepted(element, "profiles", value);
        }
        for value in [
            "",
            " contains space",
            "contains space",
            "relative ",
            "https://example.invalid/a, ",
            "urn:ab:first,\thttps://example.invalid/second",
            "https://example.invalid:65536/path",
            "https://example.invalid:00001/path",
            "https://example.invalid:1a/path",
            "https://example.invalid/a//b",
            "https://user:@example.invalid/path",
            "https://[1:2:3:4:5:6:7:8:9]/",
            "https://[1::2::3]/",
            "https://[::ffff:256.0.2.1]/",
            "https://[vA.example]/",
            "https://example.invalid/[x]",
        ] {
            rejected(element, "profiles", value);
        }
    }
}

#[test]
fn range_lexical_language_and_bounded_semantic_extents_are_distinct() {
    for (element, attribute) in [
        ("SegmentTemplate", "indexRange"),
        ("Initialization", "range"),
        ("RepresentationIndex", "range"),
        ("BitstreamSwitching", "range"),
    ] {
        for value in ["", "-", "123", "123-", "-456", "0001-9999"] {
            let report = lint(&fixture(element, attribute, value), 1);
            let lexical = report
                .coverage
                .iter()
                .find(|item| item.rule_id == "DASH-ATTRIBUTE-LEXICAL")
                .unwrap();
            assert_eq!(lexical.status, CheckStatus::Passed, "{report:?}");
            let requires_extent = element != "BitstreamSwitching" && value != "0001-9999";
            assert_eq!(
                report
                    .coverage
                    .iter()
                    .any(|item| item.rule_id == "DASH-ADDRESSING-RANGES"
                        && item.status == CheckStatus::Failed),
                requires_extent,
                "{report:?}"
            );
            assert_eq!(report.is_clean_for_scope(), !requires_extent, "{report:?}");
        }
        for value in [
            "1-2-3",
            "1,2",
            "bytes=1-2",
            " 1-2",
            "1-2 ",
            "+1-2",
            "１２-３４",
        ] {
            rejected(element, attribute, value);
        }
    }
}

#[test]
fn unrestricted_strings_remain_unrestricted_and_unknown_contexts_are_unresolved() {
    for (element, attribute) in [
        ("AdaptationSet", "mimeType"),
        ("Representation", "mimeType"),
        ("AdaptationSet", "tag"),
        ("Representation", "segmentProfiles"),
        ("Representation", "containerProfiles"),
        ("Representation", "dependencyId"),
        ("Representation", "associationType"),
    ] {
        accepted(element, attribute, " arbitrary 日本語 \t ");
    }
    let body = VALID.replace(
        "<Period>",
        "<BaseURL byteRange=\"arbitrary text\">x/</BaseURL><Period>",
    );
    assert!(lint(&body, 1).is_clean_for_scope());
    for (element, attribute) in [
        ("Representation", "lang"),
        ("Representation", "contentType"),
        ("Period", "audioSamplingRate"),
    ] {
        let report = lint(&fixture(element, attribute, "bad"), 1);
        assert!(report.incomplete && report.errors == 0, "{report:?}");
    }
}

#[test]
fn long_profile_values_are_checked_without_exhaustion_or_value_disclosure() {
    let valid = format!("https://example.invalid/{}", "p,".repeat(50_000));
    accepted("MPD", "profiles", &valid);
    accepted(
        "MPD",
        "profiles",
        &format!("{valid} contains-hidden-value-sentinel"),
    );
    let invalid = format!("{valid} contains hidden-value-sentinel");
    let report = lint(&fixture("MPD", "profiles", &invalid), 1);
    assert!(!report.is_clean_for_scope() && report.errors > 0 && !report.incomplete);
    assert!(!format!("{report:?}").contains("hidden-value-sentinel"));
}

#[test]
fn typed_attribute_failures_have_identical_full_reports_for_one_and_many_workers() {
    let mut body = String::new();
    for index in 0..1024 {
        let rate = if index % 5 == 0 {
            "48000 invalid"
        } else {
            "48000 44100"
        };
        write!(body, "<Representation id=\"r{index}\" bandwidth=\"1\" audioSamplingRate=\"{rate}\" codecs=\"avc1.4d401f\" profiles=\"urn:mpeg:dash:profile:isoff-live:2011\"/>").unwrap();
    }
    let text = format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" profiles="urn:mpeg:dash:profile:isoff-live:2011" minBufferTime="PT1S" mediaPresentationDuration="PT8S"><Period><AdaptationSet><SegmentTemplate duration="2" media="s-$Number$"/>{body}</AdaptationSet></Period></MPD>"#
    );
    assert!(text.len() > 64 * 1024);
    let mut serial = lint(&text, 1);
    let mut parallel = lint(&text, 8);
    assert!(parallel.workers_used > 1);
    assert_eq!(serial.errors, 205);
    assert!(!serial.incomplete && !serial.truncated);
    serial.workers_used = 0;
    parallel.workers_used = 0;
    assert_eq!(serial, parallel);
}

#[test]
fn schema_entity_whitespace_and_instance_character_references_are_distinct() {
    let value = "urn:ab:first,\thttps://example.invalid/second";
    // A literal XML attribute tab is normalized to a space by the XML parser.
    let literal = fixture("MPD", "profiles", value).replace("&#9;", "\t");
    assert!(lint(&literal, 1).is_clean_for_scope());
    // A character reference retains its tab. The schema's own DTD entity tab
    // was normalized to a space in its pattern attribute, so it is not allowed.
    rejected("MPD", "profiles", value);
}
