use drmr_manifest::{
    CheckStatus, Document, DocumentId, Format, NotCheckedReason, Options, Profile, RuleScope,
    lint_document,
};
use std::fmt::Write;

const VALID: &str = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" profiles="urn:mpeg:dash:profile:isoff-live:2011" minBufferTime="PT1S" mediaPresentationDuration="PT8S"><Period><AdaptationSet mimeType="video/mp4" codecs="avc1.4d401f"><SegmentTemplate timescale="1000" media="seg-$Number$.m4s" initialization="init.mp4"><SegmentTimeline><S t="0" d="2000" r="3"/></SegmentTimeline></SegmentTemplate><Representation id="v1" bandwidth="1000000"/></AdaptationSet></Period></MPD>"#;

fn lint(text: &str) -> drmr_manifest::Report {
    lint_document(
        Document {
            id: DocumentId(7),
            format: Format::Dash,
            bytes: text.as_bytes(),
            base_uri: None,
        },
        &Options::new(Profile::DashStaticManifest),
    )
    .unwrap()
}
fn failed(report: &drmr_manifest::Report, id: &str) -> bool {
    report
        .coverage
        .iter()
        .any(|item| item.rule_id == id && item.status == CheckStatus::Failed)
}

#[test]
fn static_template_timeline_is_checked_beyond_xml() {
    let report = lint(VALID);
    assert!(report.is_clean_for_scope(), "{report:?}");
    for rule in [
        "DASH-TEMPLATE-TIMESCALE",
        "DASH-TIMELINE-ENTRIES",
        "DASH-REPRESENTATION-ATTRIBUTES",
    ] {
        assert!(
            report
                .coverage
                .iter()
                .any(|item| item.rule_id == rule && item.passed_checks > 0),
            "{rule}"
        );
    }
    assert!(
        report
            .coverage
            .iter()
            .any(|item| item.scope == RuleScope::OutsideScope
                && item.status == CheckStatus::NotChecked(NotCheckedReason::RequiresMediaEvidence))
    );
}

#[test]
fn zero_timescale_and_missing_representation_bandwidth_fail() {
    assert!(failed(
        &lint(&VALID.replace("timescale=\"1000\"", "timescale=\"0\"")),
        "DASH-TEMPLATE-TIMESCALE"
    ));
    assert!(failed(
        &lint(&VALID.replace(" bandwidth=\"1000000\"", "")),
        "DASH-REPRESENTATION-ATTRIBUTES"
    ));
}

#[test]
fn timeline_numeric_bounds_and_unbounded_repeat_do_not_expand() {
    assert!(failed(
        &lint(&VALID.replace("d=\"2000\"", "d=\"0\"")),
        "DASH-TIMELINE-ENTRIES"
    ));
    assert!(failed(
        &lint(&VALID.replace("r=\"3\"", "r=\"nonsense\"")),
        "DASH-TIMELINE-ENTRIES"
    ));
    assert!(lint(&VALID.replace("r=\"3\"", "r=\"18446744073709551616\"")).is_clean_for_scope());
    let report = lint(&VALID.replace("r=\"3\"", "r=\"-1\""));
    assert!(report.is_clean_for_scope(), "{report:?}");
}

#[test]
fn template_tokens_are_parsed_without_exposing_the_value() {
    assert!(
        lint(&VALID.replace(
            "seg-$Number$.m4s",
            "seg-$$-$Number%05d$-$RepresentationID$.m4s"
        ))
        .is_clean_for_scope()
    );
    let text = VALID.replace("seg-$Number$.m4s", "confidential-$Unknown$.m4s");
    let report = lint(&text);
    assert!(failed(&report, "DASH-TEMPLATE-TOKENS"));
    assert!(!format!("{report:?}").contains("confidential"));
}

#[test]
fn dynamic_and_xlink_are_incomplete_without_network_access() {
    for text in [VALID.replace("type=\"static\"", "type=\"dynamic\""),
        VALID.replace("<Period>", "<Period xmlns:xlink=\"http://www.w3.org/1999/xlink\" xlink:href=\"https://private.invalid/secret\">")] {
        let report = lint(&text);
        assert!(report.incomplete); assert!(!report.is_clean_for_scope());
        assert!(!format!("{report:?}").contains("private.invalid"));
    }
}

#[test]
fn invalid_duration_and_mutually_exclusive_addressing_fail() {
    assert!(failed(
        &lint(&VALID.replace("minBufferTime=\"PT1S\"", "minBufferTime=\"PTNaNS\"")),
        "DASH-MPD-MIN-BUFFER-TIME"
    ));
    assert!(failed(
        &lint(&VALID.replace("<SegmentTemplate", "<SegmentList/><SegmentTemplate")),
        "DASH-SEGMENT-ADDRESSING"
    ));
}

#[test]
fn xml_schema_lexical_forms_and_boolean_errors_are_distinct() {
    for value in ["0", "0001", " 4294967295 "] {
        let report =
            lint(&VALID.replace("bandwidth=\"1000000\"", &format!("bandwidth=\"{value}\"")));
        assert!(report.is_clean_for_scope(), "{value}: {report:?}");
    }
    for value in [
        "+0001",
        "+0",
        "-0",
        "-000",
        "-1",
        "4294967296",
        "1 2",
        "1.0",
        "",
        "+-0",
        "１２",
    ] {
        assert!(
            failed(
                &lint(&VALID.replace("bandwidth=\"1000000\"", &format!("bandwidth=\"{value}\""))),
                "DASH-REPRESENTATION-ATTRIBUTES"
            ),
            "{value}"
        );
    }
    for value in ["true", "false", "1", "0", " 1 "] {
        let report = lint(&VALID.replace(
            "<AdaptationSet ",
            &format!("<AdaptationSet bitstreamSwitching=\"{value}\" "),
        ));
        assert!(report.is_clean_for_scope(), "{value}: {report:?}");
    }
    assert!(failed(
        &lint(&VALID.replace(
            "<AdaptationSet ",
            "<AdaptationSet bitstreamSwitching=\"yes\" "
        )),
        "DASH-ATTRIBUTE-LEXICAL"
    ));
}

#[test]
fn unknown_features_and_unsupported_subnumber_are_explicitly_incomplete() {
    for text in [
        VALID.replace("<MPD ", "<MPD mystery=\"private\" "),
        VALID.replace("seg-$Number$.m4s", "seg-$SubNumber$.m4s"),
        VALID.replace("<Period>", "<Period><EventStream/>"),
    ] {
        let report = lint(&text);
        assert!(report.incomplete && report.errors == 0, "{report:?}");
    }
}

#[test]
fn doctype_text_in_a_comment_is_not_an_external_declaration() {
    assert!(
        lint(&VALID.replace("<Period>", "<!-- <!DOCTYPE harmless> --><Period>"))
            .is_clean_for_scope()
    );
    let report = lint(&format!(
        "<!DOCTYPE MPD SYSTEM 'https://private.invalid/schema'> {VALID}"
    ));
    assert!(report.incomplete && report.errors == 0);
}

#[test]
fn cenc_manifest_signaling_is_checked_without_loading_media_or_drm_data() {
    const PROTECTION: &str = r#"<ContentProtection xmlns:cenc="urn:mpeg:cenc:2013" schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc" cenc:default_KID="00112233-4455-6677-8899-aabbccddeeff"/>"#;
    let valid = VALID.replace("<SegmentTemplate", &format!("{PROTECTION}<SegmentTemplate"));
    let report = lint(&valid);
    assert!(report.is_clean_for_scope(), "{report:?}");
    assert!(
        report
            .coverage
            .iter()
            .any(|item| item.rule_id == "DASH-CENC-DEFAULT-KID"
                && item.status == CheckStatus::Passed)
    );
    assert!(failed(
        &lint(&valid.replace(
            "00112233-4455-6677-8899-aabbccddeeff",
            "private-invalid-key"
        )),
        "DASH-CENC-DEFAULT-KID"
    ));
    let pssh = "<ContentProtection xmlns:cenc=\"urn:mpeg:cenc:2013\" schemeIdUri=\"urn:uuid:1077efec-c0b2-4d02-ace3-3c1e52e2fb4b\"><cenc:pssh>AAAAIHBzc2gAAAAAEHfv7MCyTQKs4zweUuL7SwAAAAA=</cenc:pssh></ContentProtection>";
    assert!(
        lint(&VALID.replace("<SegmentTemplate", &format!("{pssh}<SegmentTemplate")))
            .is_clean_for_scope()
    );
    assert!(failed(
        &lint(&VALID.replace(
            "<SegmentTemplate",
            &format!(
                "{}<SegmentTemplate",
                pssh.replace("AAAAIHBzc2gAAAAAEHfv7MCyTQKs4zweUuL7SwAAAAA=", "Yg==")
            )
        )),
        "DASH-CENC-PSSH-BOX"
    ));
}

#[test]
fn xml_attribute_admission_precedes_dom_duplicate_attribute_processing() {
    // A small bounded control reproduces the missing admission rule. Large
    // adversarial counts are exercised only after the lexical guard exists.
    let mut attributes = String::new();
    for index in 0..130 {
        write!(attributes, " a{index}=\"0\"").unwrap();
    }
    let report = lint(&VALID.replace("<MPD ", &format!("<MPD{attributes} ")));
    assert!(
        report
            .coverage
            .iter()
            .any(|item| item.rule_id == "DASH-XML-LIMIT"
                && item.status == CheckStatus::NotChecked(NotCheckedReason::ResourceLimit))
    );
    assert_eq!(report.errors, 0);
    assert!(
        report
            .coverage
            .iter()
            .filter(|item| item.rule_id == "DASH-XML-LIMIT")
            .all(|item| {
                item.reference.url.ends_with("#resource-limits")
                    && item.reference.section == "resource-limits"
            })
    );
}

#[test]
fn inherited_template_lexical_values_are_checked_once_per_declaration() {
    let mut representations = String::new();
    for index in 0..64 {
        write!(representations, "<Representation id=\"r{index}\" bandwidth=\"1\"><SegmentTemplate timescale=\"1000\"/></Representation>").unwrap();
    }
    let text = VALID.replace(
        "<Representation id=\"v1\" bandwidth=\"1000000\"/>",
        &representations,
    );
    let report = lint(&text);
    assert!(report.is_clean_for_scope(), "{report:?}");
    assert_eq!(
        report
            .coverage
            .iter()
            .find(|item| item.rule_id == "DASH-TEMPLATE-TOKENS")
            .unwrap()
            .passed_checks,
        2
    );
}

#[test]
fn unsupported_xml_encodings_are_unresolved_rather_than_malformed_utf8() {
    let mut utf16 = vec![0xff, 0xfe];
    for unit in VALID.encode_utf16() {
        utf16.extend(unit.to_le_bytes());
    }
    let report = lint_document(
        Document {
            id: DocumentId(1),
            format: Format::Dash,
            bytes: &utf16,
            base_uri: None,
        },
        &Options::new(Profile::DashStaticManifest),
    )
    .unwrap();
    assert!(report.incomplete && report.errors == 0, "{report:?}");
    let report = lint(&format!(
        "<?xml version='1.0' encoding='ISO-8859-1'?>{VALID}"
    ));
    assert!(report.incomplete && report.errors == 0, "{report:?}");
    assert!(
        lint(&format!(
            "\u{feff}<?xml version='1.0' encoding='UTF-8'?>{VALID}"
        ))
        .is_clean_for_scope()
    );
}

#[test]
fn repeated_representation_identity_needs_equivalence_evidence_and_is_not_a_schema_error() {
    let representation = "<Representation id=\"v1\" bandwidth=\"1000000\"/>";
    let same_set = VALID.replace(representation, &representation.repeat(2));
    let adaptation = VALID
        .split_once("<AdaptationSet ")
        .unwrap()
        .1
        .split_once("</AdaptationSet>")
        .unwrap()
        .0;
    let other_set = VALID.replace(
        "</AdaptationSet>",
        &format!("</AdaptationSet><AdaptationSet {adaptation}</AdaptationSet>"),
    );
    for text in [same_set, other_set] {
        let report = lint(&text);
        assert_eq!(report.errors, 0, "{report:?}");
        assert!(
            report.incomplete
                && report
                    .coverage
                    .iter()
                    .any(|item| item.rule_id == "DASH-REPRESENTATION-IDENTITY"
                        && item.status
                            == CheckStatus::NotChecked(NotCheckedReason::InvalidPrerequisite)),
            "{report:?}"
        );
    }
    let period = VALID
        .split_once("<Period>")
        .unwrap()
        .1
        .split_once("</Period>")
        .unwrap()
        .0;
    assert!(
        lint(&VALID.replace("</Period>", &format!("</Period><Period>{period}</Period>")))
            .is_clean_for_scope()
    );
    assert!(failed(
        &lint(&VALID.replace("id=\"v1\"", "id=\"v 1\"")),
        "DASH-REPRESENTATION-ATTRIBUTES"
    ));
}

#[test]
fn namespace_admission_bounds_global_unique_bindings_and_allows_repeated_cenc_declarations() {
    let mut repeated = String::new();
    let mut distinct = String::new();
    for index in 0..256 {
        write!(
            repeated,
            "<Representation id=\"r{index}\" bandwidth=\"1\" xmlns:cenc=\"urn:mpeg:cenc:2013\"/>"
        )
        .unwrap();
        write!(distinct, "<Representation id=\"r{index}\" bandwidth=\"1\" xmlns:p{index}=\"urn:unique:{index}\"/>").unwrap();
    }
    let representation = "<Representation id=\"v1\" bandwidth=\"1000000\"/>";
    assert!(lint(&VALID.replace(representation, &repeated)).is_clean_for_scope());
    let report = lint(&VALID.replace(representation, &distinct));
    assert!(
        report.incomplete
            && report.errors == 0
            && report
                .coverage
                .iter()
                .any(|item| item.rule_id == "DASH-XML-LIMIT"
                    && item.status == CheckStatus::NotChecked(NotCheckedReason::ResourceLimit)),
        "{report:?}"
    );
}

#[test]
fn selection_priority_uses_unsigned_int_on_both_representation_base_elements() {
    for element in ["AdaptationSet", "Representation"] {
        let original = format!("<{element} ");
        for value in ["0", "1", "0001", "4294967295", " 1 "] {
            let text = VALID.replace(
                &original,
                &format!("<{element} selectionPriority=\"{value}\" "),
            );
            let report = lint(&text);
            assert!(report.is_clean_for_scope(), "{element} {value}: {report:?}");
        }
        for value in [
            "+1",
            "-0",
            "-000",
            "-1",
            "4294967296",
            "1.0",
            "1e0",
            "",
            " ",
            "+",
        ] {
            let text = VALID.replace(
                &original,
                &format!("<{element} selectionPriority=\"{value}\" "),
            );
            let report = lint(&text);
            assert!(
                failed(&report, "DASH-ATTRIBUTE-LEXICAL"),
                "{element} {value}: {report:?}"
            );
        }
    }
    let report = lint(&VALID.replace(
        "<AdaptationSet ",
        "<AdaptationSet selectionPriorityUnknown=\"1\" ",
    ));
    assert!(report.incomplete && report.errors == 0, "{report:?}");
}

#[test]
fn representation_and_adaptation_attributes_keep_their_schema_contexts() {
    for (element, attribute) in [
        ("AdaptationSet", "group"),
        ("Representation", "qualityRanking"),
    ] {
        let report = lint(&VALID.replace(
            &format!("<{element} "),
            &format!("<{element} {attribute}=\"1\" "),
        ));
        assert!(report.is_clean_for_scope(), "{report:?}");
    }
    for (element, attribute) in [
        ("Representation", "group"),
        ("AdaptationSet", "qualityRanking"),
    ] {
        let report = lint(&VALID.replace(
            &format!("<{element} "),
            &format!("<{element} {attribute}=\"1\" "),
        ));
        assert!(report.incomplete && report.errors == 0, "{report:?}");
        assert!(report.coverage.iter().any(|item| {
            item.rule_id == "DASH-ATTRIBUTE-COVERAGE"
                && item.status == CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature)
        }));
    }
}

#[test]
fn scan_type_accepts_only_the_pinned_schema_enumeration() {
    for element in ["AdaptationSet", "Representation"] {
        for value in ["progressive", "interlaced", "unknown"] {
            assert!(
                lint(&VALID.replace(
                    &format!("<{element} "),
                    &format!("<{element} scanType=\"{value}\" ")
                ))
                .is_clean_for_scope()
            );
        }
        for value in ["", "Progressive", " progressive", "private"] {
            let report = lint(&VALID.replace(
                &format!("<{element} "),
                &format!("<{element} scanType=\"{value}\" "),
            ));
            assert!(failed(&report, "DASH-ATTRIBUTE-LEXICAL"), "{report:?}");
        }
    }
}

#[test]
fn representation_identifier_may_be_empty_or_contain_non_separator_unicode() {
    for value in ["", "音声", "v\u{85}1", "v\u{180e}1", "v\u{200b}1"] {
        let report = lint(&VALID.replace("id=\"v1\"", &format!("id=\"{value}\"")));
        assert!(report.is_clean_for_scope(), "{report:?}");
    }
    assert!(failed(
        &lint(&VALID.replace(" id=\"v1\"", "")),
        "DASH-REPRESENTATION-ATTRIBUTES"
    ));
}

#[test]
fn representation_identifier_rejects_the_unicode_separator_categories() {
    for separator in [
        ' ', '\t', '\r', '\n', '\u{a0}', '\u{1680}', '\u{2000}', '\u{200a}', '\u{2028}',
        '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}',
    ] {
        let report = lint(&VALID.replace("id=\"v1\"", &format!("id=\"v{separator}1\"")));
        assert!(
            failed(&report, "DASH-REPRESENTATION-ATTRIBUTES"),
            "{report:?}"
        );
    }
}

fn two_periods(first_attributes: &str, second_attributes: &str, first_repeat: &str) -> String {
    let body = VALID
        .split_once("<Period>")
        .unwrap()
        .1
        .split_once("</Period>")
        .unwrap()
        .0;
    VALID
        .replace(" mediaPresentationDuration=\"PT8S\"", "")
        .replace(
            &format!("<Period>{body}</Period>"),
            &format!(
                "<Period{first_attributes}>{}</Period><Period{second_attributes}>{body}</Period>",
                body.replace("r=\"3\"", &format!("r=\"{first_repeat}\""))
            ),
        )
}

#[test]
fn terminal_negative_repeat_uses_its_own_period_duration() {
    let report = lint(&two_periods(" duration=\"PT8S\"", "", "-1"));
    assert!(report.is_clean_for_scope(), "{report:?}");
}

#[test]
fn unrelated_period_or_presentation_duration_cannot_bound_an_earlier_repeat() {
    for (last_attributes, total_duration) in [
        (" duration=\"PT8S\"", ""),
        ("", " mediaPresentationDuration=\"PT16S\""),
    ] {
        let text = two_periods("", last_attributes, "-1")
            .replace("<MPD ", &format!("<MPD{total_duration} "));
        let report = lint(&text);
        assert!(report.incomplete && report.errors == 0, "{report:?}");
        assert!(report.coverage.iter().any(|item| {
            item.rule_id == "DASH-TIMELINE-ARITHMETIC"
                && item.status == CheckStatus::NotChecked(NotCheckedReason::InvalidPrerequisite)
        }));
    }
}

#[test]
fn period_repeat_boundaries_can_follow_next_start_or_last_presentation_end() {
    let next_start = two_periods("", " start=\"PT8S\" duration=\"PT8S\"", "-1");
    assert!(lint(&next_start).is_clean_for_scope());
    let last = two_periods(" duration=\"PT8S\"", "", "3")
        .replace("<MPD ", "<MPD mediaPresentationDuration=\"PT16S\" ");
    let (before, tail) = last.rsplit_once("r=\"3\"").unwrap();
    let last = format!("{before}r=\"-1\"{tail}");
    assert!(lint(&last).is_clean_for_scope());
}

fn repeat_before(next_time: u64) -> drmr_manifest::Report {
    lint(&VALID.replace(
        "<S t=\"0\" d=\"2000\" r=\"3\"/>",
        &format!("<S t=\"100\" d=\"2\" r=\"-1\"/><S t=\"{next_time}\" d=\"2\"/>"),
    ))
}

fn unresolved_repeat(report: &drmr_manifest::Report) {
    assert!(report.incomplete && report.errors == 0, "{report:?}");
    assert!(report.coverage.iter().any(|item| {
        item.rule_id == "DASH-TIMELINE-ARITHMETIC"
            && item.status == CheckStatus::NotChecked(NotCheckedReason::InvalidPrerequisite)
    }));
}

#[test]
fn a_negative_repeat_before_a_backward_boundary_is_unresolved_not_a_spec_error() {
    unresolved_repeat(&repeat_before(1));
}

#[test]
fn a_negative_repeat_before_an_equal_boundary_is_unresolved_not_a_spec_error() {
    unresolved_repeat(&repeat_before(100));
}

#[test]
fn forward_negative_repeat_boundaries_do_not_impose_a_divisibility_constraint() {
    for next_time in [101, 102, 103] {
        let report = repeat_before(next_time);
        assert!(report.is_clean_for_scope(), "{report:?}");
    }
}

#[test]
fn orphan_timeline_entries_are_not_skipped_outside_their_timeline_parent() {
    let report = lint(&VALID.replace(
        "<Representation id=\"v1\" bandwidth=\"1000000\"/>",
        "<Representation id=\"v1\" bandwidth=\"1000000\"><S d=\"0\" future=\"x\"/></Representation>",
    ));
    assert!(failed(&report, "DASH-ELEMENT-PLACEMENT"), "{report:?}");
    assert!(report.incomplete);
}

fn stylesheet_is_unresolved(text: &str) {
    let report = lint(text);
    assert!(report.errors == 0 && report.incomplete, "{report:?}");
    assert!(report.coverage.iter().any(|item| {
        item.rule_id == "DASH-XML-EXTERNAL"
            && item.status == CheckStatus::NotChecked(NotCheckedReason::UnsupportedFeature)
    }));
}

#[test]
fn stylesheet_processing_instructions_before_the_root_are_not_ignored() {
    stylesheet_is_unresolved(&format!("<?xml-stylesheet href='style.xsl'?>{VALID}"));
}

#[test]
fn stylesheet_processing_instructions_after_the_root_are_not_ignored() {
    stylesheet_is_unresolved(&format!("{VALID}<?xml-stylesheet href='style.xsl'?>"));
}

#[test]
fn stylesheet_processing_instructions_inside_root_are_still_checked_once() {
    let text = VALID.replace("<Period>", "<?xml-stylesheet href='style.xsl'?><Period>");
    stylesheet_is_unresolved(&text);
    assert_eq!(
        lint(&text)
            .coverage
            .iter()
            .find(|item| item.rule_id == "DASH-XML-EXTERNAL")
            .unwrap()
            .not_checked_checks,
        1
    );
    assert!(lint(&format!("<?status ready?>{VALID}")).is_clean_for_scope());
}

#[test]
fn alignment_attribute_lexical_space_matches_the_pinned_fifth_edition_schema() {
    for attribute in [
        "segmentAlignment",
        "subsegmentAlignment",
        "bitstreamSwitching",
    ] {
        for value in ["true", "false", "1", "0", " 1 "] {
            assert!(
                lint(&VALID.replace(
                    "<AdaptationSet ",
                    &format!("<AdaptationSet {attribute}=\"{value}\" ")
                ))
                .is_clean_for_scope()
            );
        }
        assert!(failed(
            &lint(&VALID.replace(
                "<AdaptationSet ",
                &format!("<AdaptationSet {attribute}=\"2\" ")
            )),
            "DASH-ATTRIBUTE-LEXICAL"
        ));
    }
}

fn terminal_manifest(duration: &str, timescale: u64, offset: u64, start: u64) -> String {
    VALID
        .replace(
            "mediaPresentationDuration=\"PT8S\"",
            &format!("mediaPresentationDuration=\"{duration}\""),
        )
        .replace(
            "timescale=\"1000\"",
            &format!("timescale=\"{timescale}\" presentationTimeOffset=\"{offset}\""),
        )
        .replace(
            "<S t=\"0\" d=\"2000\" r=\"3\"/>",
            &format!("<S t=\"{start}\" d=\"2000\" r=\"-1\"/>"),
        )
}

#[test]
fn terminal_repeat_extent_uses_rational_duration_effective_timescale_and_offset() {
    for duration in ["PT8S", "PT8.5S"] {
        let text = terminal_manifest(duration, 1000, 4, 4);
        assert!(lint(&text).is_clean_for_scope());
    }
    for start in [8004, 8005] {
        unresolved_repeat(&lint(&terminal_manifest("PT8S", 1000, 4, start)));
    }
    assert!(lint(&terminal_manifest("PT8.5S", 1000, 4, 8004)).is_clean_for_scope());
}

#[test]
fn terminal_repeat_endpoint_overflow_is_in_scope_resource_exhaustion() {
    let text = terminal_manifest("PT340282366920938463463374607431768211455S", 2, 0, 0);
    let report = lint(&text);
    assert!(report.incomplete && report.errors == 0, "{report:?}");
    assert!(report.coverage.iter().any(|item| {
        item.rule_id == "DASH-TIMELINE-ARITHMETIC"
            && item.status == CheckStatus::NotChecked(NotCheckedReason::ResourceLimit)
            && item.scope == RuleScope::InScope
    }));
}

#[test]
fn inherited_terminal_timelines_are_checked_for_active_effective_contexts_once() {
    let base = terminal_manifest("PT1S", 1000, 0, 1000);
    let original = "<Representation id=\"v1\" bandwidth=\"1000000\"/>";
    let mut representations = String::new();
    for index in 0..32 {
        write!(representations, "<Representation id=\"r{index}\" bandwidth=\"1\"><SegmentTemplate presentationTimeOffset=\"1\"/></Representation>").unwrap();
    }
    let text = base.replace(original, &representations);
    let report = lint(&text);
    assert!(report.is_clean_for_scope(), "{report:?}");
    assert_eq!(
        report
            .coverage
            .iter()
            .find(|item| item.rule_id == "DASH-TIMELINE-ARITHMETIC")
            .unwrap()
            .passed_checks,
        1
    );
    let text = text.replace("</AdaptationSet>", "<Representation id=\"bad\" bandwidth=\"1\"><SegmentTemplate presentationTimeOffset=\"0\"/></Representation></AdaptationSet>");
    let report = lint(&text);
    unresolved_repeat(&report);
    let coverage = report
        .coverage
        .iter()
        .find(|item| item.rule_id == "DASH-TIMELINE-ARITHMETIC")
        .unwrap();
    assert_eq!(coverage.passed_checks, 1);
    assert_eq!(coverage.not_checked_checks, 1);
}

#[test]
fn terminal_repeat_uses_the_implicit_cursor_after_a_finite_run() {
    for (duration, expected_clean) in [("PT8S", false), ("PT8.5S", true)] {
        let text = terminal_manifest(duration, 1000, 4, 4).replace(
            "<S t=\"4\" d=\"2000\" r=\"-1\"/>",
            "<S t=\"4\" d=\"2000\" r=\"3\"/><S d=\"2000\" r=\"-1\"/>",
        );
        let report = lint(&text);
        assert_eq!(report.is_clean_for_scope(), expected_clean, "{report:?}");
        if !expected_clean {
            unresolved_repeat(&report);
        }
    }
}

#[test]
fn terminal_repeat_inferred_period_duration_subtracts_its_own_start() {
    let source = two_periods(" start=\"PT2S\"", " start=\"PT10.5S\"", "-1");
    for (start, expected_clean) in [(8499, true), (8500, false)] {
        let text = source.replacen("t=\"0\"", &format!("t=\"{start}\""), 1);
        let report = lint(&text);
        assert_eq!(report.is_clean_for_scope(), expected_clean, "{report:?}");
        if !expected_clean {
            unresolved_repeat(&report);
        }
    }
}

#[test]
fn zero_or_unsupported_period_duration_never_proves_a_terminal_repeat() {
    for (duration, reason) in [
        ("PT0S", NotCheckedReason::InvalidPrerequisite),
        ("P1M", NotCheckedReason::UnsupportedFeature),
        (
            "PT0.00000000000000000001S",
            NotCheckedReason::UnsupportedFeature,
        ),
    ] {
        let report = lint(&terminal_manifest(duration, 1000, 0, 0));
        assert!(report.incomplete && report.errors == 0, "{report:?}");
        assert!(report.coverage.iter().any(|item| {
            item.rule_id == "DASH-TIMELINE-ARITHMETIC"
                && item.status == CheckStatus::NotChecked(reason)
                && item.scope == RuleScope::InScope
        }));
    }
}

#[test]
fn timeline_arithmetic_cites_ordinary_timing_facts_without_imposing_restricted_rules() {
    let report = lint(&terminal_manifest("PT8.5S", 1000, 4, 4));
    let coverage = report
        .coverage
        .iter()
        .find(|item| item.rule_id == "DASH-TIMELINE-ARITHMETIC")
        .unwrap();
    assert_eq!(
        coverage.reference.url,
        "https://dashif.org/Guidelines-TimingModel/"
    );
    assert_eq!(coverage.reference.revision, "Commit Snapshot 2024-10-24");
    assert!(
        coverage
            .reference
            .section
            .contains("ordinary timing descriptions")
    );
    assert!(report.is_clean_for_scope());
}
