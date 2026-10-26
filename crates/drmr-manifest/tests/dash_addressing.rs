use drmr_manifest::{
    CheckStatus, Document, DocumentId, Format, NotCheckedReason, Options, Profile, Report,
    RuleScope, lint_document,
};
use std::{fmt::Write as _, num::NonZeroUsize};

fn presentation(body: &str, duration: &str) -> String {
    format!(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" profiles="urn:mpeg:dash:profile:full:2011" minBufferTime="PT1S" mediaPresentationDuration="{duration}"><Period><AdaptationSet>{body}</AdaptationSet></Period></MPD>"#
    )
}
fn lint(text: &str) -> Report {
    lint_workers(text, 1)
}
fn lint_workers(text: &str, workers: usize) -> Report {
    let mut options = Options::new(Profile::DashStaticManifest);
    options.workers = NonZeroUsize::new(workers);
    options.limits.max_diagnostics = 3;
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
fn failed(report: &Report, id: &str) -> bool {
    report
        .coverage
        .iter()
        .any(|item| item.rule_id == id && item.status == CheckStatus::Failed)
}
fn unresolved(report: &Report, id: &str, reason: NotCheckedReason) -> bool {
    report
        .coverage
        .iter()
        .any(|item| item.rule_id == id && item.status == CheckStatus::NotChecked(reason))
}

const REPRESENTATION: &str = r#"<Representation id="audio" bandwidth="128000"/>"#;
const LIST: &str = r#"<SegmentList timescale="44100" presentationTimeOffset="44100"><Initialization sourceURL="audio.mp4" range="0-99"/><SegmentTimeline><S t="44100" d="264192" r="1"/><S d="89116"/></SegmentTimeline><SegmentURL media="audio.mp4" mediaRange="100-199"/><SegmentURL media="audio.mp4" mediaRange="200-299"/><SegmentURL media="audio.mp4" mediaRange="300-349"/></SegmentList>"#;
const BASE: &str = r#"<BaseURL>audio.mp4</BaseURL><SegmentBase timescale="44100" presentationTimeOffset="44100" indexRange="100-199" indexRangeExact="true"><Initialization range="0-99"/></SegmentBase>"#;

#[test]
fn explicit_list_and_indexed_base_have_structural_coverage_without_media_claims() {
    for source in [LIST, BASE] {
        let text = presentation(&format!("{source}{REPRESENTATION}"), "PT14S");
        let report = lint(&text);
        assert!(report.is_clean_for_scope(), "{report:?}");
        assert!(
            report
                .coverage
                .iter()
                .any(|item| item.rule_id == "DASH-MEDIA-EVIDENCE"
                    && item.scope == RuleScope::OutsideScope
                    && item.status
                        == CheckStatus::NotChecked(NotCheckedReason::RequiresMediaEvidence))
        );
    }
}

#[test]
fn nominal_template_and_list_accept_signed_delta_and_exact_finite_number_bounds() {
    let template = r#"<SegmentTemplate timescale="1000" presentationTimeOffset="1000" eptDelta="-1000" duration="6000" startNumber="4294967296" endNumber="4294967297" media="seg-$Number$.m4s" initialization="init.mp4"/>"#;
    let list = r#"<SegmentList timescale="1000" presentationTimeOffset="1000" eptDelta="-1000" duration="6000" startNumber="4294967296" endNumber="4294967297"><Initialization sourceURL="init.mp4"/><SegmentURL media="a.m4s"/><SegmentURL media="b.m4s"/></SegmentList>"#;
    for source in [template, list] {
        let text = presentation(&format!("{source}{REPRESENTATION}"), "PT11S");
        let report = lint(&text);
        assert!(report.is_clean_for_scope(), "{report:?}");
        assert!(
            report
                .coverage
                .iter()
                .any(|item| item.rule_id == "DASH-NOMINAL-TIMING" && item.passed_checks > 0)
        );
        let changed = text.replace("4294967297", "4294967298");
        assert!(failed(&lint(&changed), "DASH-SEGMENT-NUMBERS"));
    }
}

#[test]
fn nominal_own_duration_and_explicit_final_numbers_do_not_use_global_ceiling() {
    let template = r#"<SegmentTemplate timescale="1000" duration="6000" presentationDuration="12001" startNumber="1" endNumber="2" media="$Number$.m4s" initialization="init.mp4"/>"#;
    let list = r#"<SegmentList timescale="1000" duration="6000" presentationDuration="12001" startNumber="1" endNumber="2"><Initialization sourceURL="init.mp4"/><SegmentURL media="a.m4s"/><SegmentURL media="b.m4s"/></SegmentList>"#;
    for source in [template, list] {
        let report = lint(&presentation(&format!("{source}{REPRESENTATION}"), "PT14S"));
        assert!(report.is_clean_for_scope(), "{report:?}");
        let own_ceiling = source
            .replace(
                "presentationDuration=\"12001\"",
                "presentationDuration=\"11999\"",
            )
            .replace(" endNumber=\"2\"", "");
        assert!(
            lint(&presentation(
                &format!("{own_ceiling}{REPRESENTATION}"),
                "PT14S"
            ))
            .is_clean_for_scope()
        );
        let out_of_period = source.replace(
            "presentationDuration=\"12001\"",
            "presentationDuration=\"14001\"",
        );
        assert!(failed(
            &lint(&presentation(
                &format!("{out_of_period}{REPRESENTATION}"),
                "PT14S"
            )),
            "DASH-NOMINAL-TIMING"
        ));
        let extra = source.replace("endNumber=\"2\"", "endNumber=\"4\"");
        let late = source.replace("duration=\"6000\"", "duration=\"6000\" eptDelta=\"13000\"");
        assert!(failed(
            &lint(&presentation(&format!("{late}{REPRESENTATION}"), "PT14S")),
            "DASH-SEGMENT-NUMBERS"
        ));
        let kind = if source == template {
            "SegmentTemplate"
        } else {
            "SegmentList"
        };
        let inherited = format!(
            r#"{source}<Representation id="audio" bandwidth="1"><{kind} presentationDuration="11999"/></Representation>"#
        );
        assert!(lint(&presentation(&inherited, "PT14S")).is_clean_for_scope());
        assert!(failed(
            &lint(&presentation(&format!("{extra}{REPRESENTATION}"), "PT14S")),
            "DASH-SEGMENT-NUMBERS"
        ));
    }
}

#[test]
fn own_duration_comparison_keeps_fractional_aac_units_and_integer_bounds() {
    let template = r#"<SegmentTemplate timescale="44100" duration="88200" presentationDuration="88201" endNumber="1" media="$Number$.m4s"/>"#;
    let clean = lint(&presentation(
        &format!("{template}{REPRESENTATION}"),
        "PT2.000022676S",
    ));
    assert!(clean.is_clean_for_scope(), "{clean:?}");
    let short = lint(&presentation(
        &format!("{template}{REPRESENTATION}"),
        "PT2.000022675S",
    ));
    assert!(failed(&short, "DASH-NOMINAL-TIMING"));
    for (value, rule) in [
        ("1.0", "DASH-TEMPLATE-INTEGERS"),
        ("18446744073709551616", "DASH-TEMPLATE-INTEGERS"),
    ] {
        let invalid = template.replace(
            "presentationDuration=\"88201\"",
            &format!("presentationDuration=\"{value}\""),
        );
        assert!(failed(
            &lint(&presentation(
                &format!("{invalid}{REPRESENTATION}"),
                "PT14S"
            )),
            rule
        ));
    }
}

#[test]
fn same_kind_inherits_children_and_changed_kind_resets_parent_facts() {
    let inherited_list = format!(
        r#"{LIST}<Representation id="audio" bandwidth="1"><SegmentList startNumber="7"/></Representation>"#
    );
    let inherited_base = format!(
        r#"{BASE}<Representation id="audio" bandwidth="1"><SegmentBase presentationTimeOffset="88200"/></Representation>"#
    );
    let changed = r#"<SegmentTemplate timescale="1000" duration="2000" media="old-$Number$" initialization="old-init"/><Representation id="audio" bandwidth="1"><SegmentList><Initialization sourceURL="init.mp4"/><SegmentTimeline><S d="1" r="2"/></SegmentTimeline><SegmentURL media="1.m4s"/><SegmentURL media="2.m4s"/><SegmentURL media="3.m4s"/></SegmentList></Representation>"#;
    for (source, duration) in [
        (inherited_list.as_str(), "PT14S"),
        (inherited_base.as_str(), "PT14S"),
        (changed, "PT3S"),
    ] {
        let report = lint(&presentation(source, duration));
        assert!(report.is_clean_for_scope(), "{report:?}");
    }
}

#[test]
fn finite_list_counts_ranges_and_unique_children_are_checked() {
    for (source, rule) in [
        (LIST.replace("r=\"1\"", "r=\"0\""), "DASH-LIST-SEGMENTS"),
        (LIST.replace("200-299", "199-299"), "DASH-ADDRESSING-RANGES"),
        (LIST.replace("200-299", "300-200"), "DASH-ADDRESSING-RANGES"),
        (LIST.replace("200-299", "200-"), "DASH-ADDRESSING-RANGES"),
        (LIST.replace("0-99", "0-100"), "DASH-ADDRESSING-RANGES"),
        (
            LIST.replace(
                "<SegmentTimeline>",
                "<Initialization sourceURL=\"duplicate\"/><SegmentTimeline>",
            ),
            "DASH-SEGMENT-ADDRESSING",
        ),
        (
            LIST.replace("<SegmentTimeline>", "<SegmentTimeline/><SegmentTimeline>"),
            "DASH-SEGMENT-ADDRESSING",
        ),
        (
            LIST.replace("timescale=\"44100\"", "timescale=\"0\""),
            "DASH-ADDRESSING-TIMESCALE",
        ),
        (BASE.replace("100-199", "99-199"), "DASH-ADDRESSING-RANGES"),
        (
            BASE.replace("indexRangeExact=\"true\"", "indexRangeExact=\"yes\""),
            "DASH-ATTRIBUTE-LEXICAL",
        ),
    ] {
        let report = lint(&presentation(&format!("{source}{REPRESENTATION}"), "PT14S"));
        assert!(failed(&report, rule), "{rule}: {report:?}");
    }
}

#[test]
fn nominal_values_do_not_relax_explicit_timelines_or_overflow_silently() {
    for source in [
        LIST.replace(
            "timescale=\"44100\"",
            "timescale=\"44100\" duration=\"6000\"",
        ),
        LIST.replace("timescale=\"44100\"", "timescale=\"44100\" eptDelta=\"0\""),
    ] {
        assert!(failed(
            &lint(&presentation(&format!("{source}{REPRESENTATION}"), "PT14S")),
            "DASH-SEGMENT-ADDRESSING"
        ));
    }
    let nominal = r#"<SegmentTemplate duration="6" eptDelta="340282366920938463463374607431768211456" media="$Number$.m4s"/>"#;
    let report = lint(&presentation(
        &format!("{nominal}{REPRESENTATION}"),
        "PT14S",
    ));
    assert!(unresolved(
        &report,
        "DASH-NOMINAL-TIMING",
        NotCheckedReason::ResourceLimit
    ));
    assert_eq!(report.errors, 0);
    let invalid = nominal.replace("340282366920938463463374607431768211456", "1.2");
    assert!(failed(
        &lint(&presentation(
            &format!("{invalid}{REPRESENTATION}"),
            "PT14S"
        )),
        "DASH-NOMINAL-TIMING"
    ));
}

#[test]
fn terminal_list_counts_use_each_inherited_period_clock_without_expansion() {
    let source = LIST
        .replace("d=\"264192\" r=\"1\"", "d=\"264600\" r=\"-1\"")
        .replace("<S d=\"89116\"/>", "");
    let report = lint(&presentation(&format!("{source}{REPRESENTATION}"), "PT14S"));
    assert!(report.is_clean_for_scope(), "{report:?}");
    let report = lint(&presentation(&format!("{source}{REPRESENTATION}"), "PT20S"));
    assert!(failed(&report, "DASH-LIST-SEGMENTS"));
}

#[test]
fn nominal_rational_cancellation_keeps_representable_large_counts_exact() {
    let source =
        r#"<SegmentTemplate timescale="1000000000" duration="1000000000" media="$Number$.m4s"/>"#;
    let report = lint(&presentation(
        &format!("{source}{REPRESENTATION}"),
        "PT3000000000000000000.000000000000000001S",
    ));
    assert!(report.is_clean_for_scope(), "{report:?}");
}

#[test]
fn inherited_initialization_uses_the_shared_literal_resource_index() {
    let body = format!(
        r#"{LIST}<Representation id="audio" bandwidth="1"><SegmentList><Initialization sourceURL="audio.mp4" range="150-199"/></SegmentList></Representation>"#
    );
    assert!(failed(
        &lint(&presentation(&body, "PT14S")),
        "DASH-ADDRESSING-RANGES"
    ));
    let body = format!(
        r#"{LIST}<Representation id="audio" bandwidth="1"><SegmentList><SegmentURL media="replacement.m4s"/></SegmentList></Representation>"#
    );
    assert!(failed(
        &lint(&presentation(&body, "PT14S")),
        "DASH-LIST-SEGMENTS"
    ));
}

#[test]
fn unknown_attributes_and_unrepresentable_extents_never_become_clean() {
    let source = LIST.replace("<SegmentList ", "<SegmentList media=\"unsupported\" ");
    let report = lint(&presentation(&format!("{source}{REPRESENTATION}"), "PT14S"));
    assert!(unresolved(
        &report,
        "DASH-ATTRIBUTE-COVERAGE",
        NotCheckedReason::UnsupportedFeature
    ));
    for value in ["0-18446744073709551615", "0-18446744073709551616"] {
        let source = BASE.replace("100-199", value);
        let report = lint(&presentation(&format!("{source}{REPRESENTATION}"), "PT14S"));
        assert!(unresolved(
            &report,
            "DASH-ADDRESSING-RANGES",
            NotCheckedReason::ResourceLimit
        ));
    }
}

#[test]
fn range_only_lists_require_one_inherited_object_base_url() {
    let source = LIST
        .replace(" sourceURL=\"audio.mp4\"", "")
        .replace(" media=\"audio.mp4\"", "");
    let body = format!("<BaseURL>audio.mp4</BaseURL>{source}{REPRESENTATION}");
    assert!(lint(&presentation(&body, "PT14S")).is_clean_for_scope());
    for base in ["", "<BaseURL>directory/</BaseURL>"] {
        let report = lint(&presentation(
            &format!("{base}{source}{REPRESENTATION}"),
            "PT14S",
        ));
        assert!(unresolved(
            &report,
            "DASH-LIST-SEGMENTS",
            NotCheckedReason::InvalidPrerequisite
        ));
    }
    let report = lint(&presentation(
        &format!("<BaseURL>a.mp4</BaseURL><BaseURL>b.mp4</BaseURL>{source}{REPRESENTATION}"),
        "PT14S",
    ));
    assert!(unresolved(
        &report,
        "DASH-LIST-SEGMENTS",
        NotCheckedReason::UnsupportedFeature
    ));
}

#[test]
fn ambiguous_base_urls_and_external_expansion_remain_unresolved() {
    let source = BASE.replace("<BaseURL>", "<BaseURL>alternate.mp4</BaseURL><BaseURL>");
    let report = lint(&presentation(&format!("{source}{REPRESENTATION}"), "PT14S"));
    assert!(unresolved(
        &report,
        "DASH-BASE-ADDRESSING",
        NotCheckedReason::UnsupportedFeature
    ));
    let source = LIST.replace("<SegmentList ", "<SegmentList xmlns:xlink=\"http://www.w3.org/1999/xlink\" xlink:href=\"https://source.invalid/private\" ");
    let report = lint(&presentation(&format!("{source}{REPRESENTATION}"), "PT14S"));
    assert!(unresolved(
        &report,
        "DASH-XML-EXTERNAL",
        NotCheckedReason::UnsupportedFeature
    ));
    assert!(!format!("{report:?}").contains("source.invalid"));
}

#[test]
fn inherited_lists_and_base_urls_run_in_parallel_with_deterministic_results() {
    for source in [LIST, BASE] {
        for invalid in [false, true] {
            let mut body = source.to_string();
            for index in 0..4096 {
                if invalid {
                    let (kind, uri) = if source == LIST {
                        ("SegmentList", " sourceURL=\"audio.mp4\"")
                    } else {
                        ("SegmentBase", "")
                    };
                    write!(body, r#"<Representation id="a{index}" bandwidth="128000"><{kind}><Initialization{uri} range="0-150"/></{kind}></Representation>"#).unwrap();
                } else {
                    write!(
                        body,
                        r#"<Representation id="a{index}" bandwidth="128000"/>"#
                    )
                    .unwrap();
                }
            }
            let text = presentation(&body, "PT14S");
            let mut serial = lint_workers(&text, 1);
            let mut parallel = lint_workers(&text, 8);
            assert!(parallel.workers_used > 1);
            assert_eq!(parallel.is_clean_for_scope(), !invalid, "{parallel:?}");
            assert_eq!(parallel.truncated, invalid);
            if invalid {
                assert_eq!(parallel.errors, 4096);
            }
            serial.workers_used = 0;
            parallel.workers_used = 0;
            assert_eq!(serial, parallel);
        }
    }
}
