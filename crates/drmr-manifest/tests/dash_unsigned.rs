use drmr_manifest::{CheckStatus, Document, DocumentId, Format, Options, Profile, lint_document};

const VALID: &str = r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" profiles="urn:mpeg:dash:profile:isoff-live:2011" minBufferTime="PT1S" mediaPresentationDuration="PT8S"><Period><AdaptationSet width="1920"><SegmentTemplate timescale="1000" presentationTimeOffset="0" media="$Number$.m4s"><SegmentTimeline><S t="0" d="2000" r="3" n="1" k="1"/></SegmentTimeline></SegmentTemplate><Representation id="v1" bandwidth="1000000"/></AdaptationSet></Period></MPD>"#;

fn lint(text: &str) -> drmr_manifest::Report {
    lint_document(
        Document {
            id: DocumentId(1),
            format: Format::Dash,
            bytes: text.as_bytes(),
            base_uri: None,
        },
        &Options::new(Profile::DashStaticManifest),
    )
    .unwrap()
}

fn reject_signs(original: &str, attribute: &str, rule: &str) {
    reject_signs_on(VALID, original, attribute, rule);
}

fn reject_signs_on(source: &str, original: &str, attribute: &str, rule: &str) {
    for value in ["+1", " +1 ", "+0", "-0", "-000", "-1"] {
        let text = source.replace(original, &format!("{attribute}=\"{value}\""));
        let report = lint(&text);
        assert!(
            report.errors > 0 && !report.is_clean_for_scope(),
            "{attribute}: errors={}, incomplete={}, truncated={}",
            report.errors,
            report.incomplete,
            report.truncated
        );
        assert!(
            report
                .coverage
                .iter()
                .any(|item| item.rule_id == rule && item.status == CheckStatus::Failed)
        );
    }
}

#[test]
fn unsigned_representation_and_attribute_ints_reject_signed_lexical_forms() {
    reject_signs(
        "bandwidth=\"1000000\"",
        "bandwidth",
        "DASH-REPRESENTATION-ATTRIBUTES",
    );
    reject_signs("width=\"1920\"", "width", "DASH-ATTRIBUTE-LEXICAL");
}

#[test]
fn unsigned_long_attributes_reject_signed_lexical_forms() {
    for attribute in ["n", "k"] {
        reject_signs(
            &format!("{attribute}=\"1\""),
            attribute,
            "DASH-ATTRIBUTE-LEXICAL",
        );
    }
}

#[test]
fn template_timescale_rejects_signed_lexical_forms() {
    reject_signs("timescale=\"1000\"", "timescale", "DASH-TEMPLATE-TIMESCALE");
}

#[test]
fn template_presentation_offset_rejects_signed_lexical_forms() {
    reject_signs(
        "presentationTimeOffset=\"0\"",
        "presentationTimeOffset",
        "DASH-TEMPLATE-INTEGERS",
    );
}

#[test]
fn template_duration_and_segment_numbers_keep_unsigned_int_lexical_bounds() {
    let base = VALID.replace(
        "<SegmentTimeline><S t=\"0\" d=\"2000\" r=\"3\" n=\"1\" k=\"1\"/></SegmentTimeline>",
        "",
    );
    for attribute in ["duration", "startNumber", "endNumber"] {
        let duration = if attribute == "duration" {
            ""
        } else {
            "duration=\"2000\" "
        };
        let source = base.replace(
            "<SegmentTemplate ",
            &format!("<SegmentTemplate {duration}{attribute}=\"1\" "),
        );
        assert!(lint(&source).is_clean_for_scope());
        reject_signs_on(
            &source,
            &format!("{attribute}=\"1\""),
            attribute,
            "DASH-TEMPLATE-INTEGERS",
        );
        assert!(
            lint(&source.replace(
                &format!("{attribute}=\"1\""),
                &format!("{attribute}=\"4294967295\"")
            ))
            .is_clean_for_scope()
        );
        assert!(
            lint(&source.replace(
                &format!("{attribute}=\"1\""),
                &format!("{attribute}=\"4294967296\"")
            ))
            .errors
                > 0
        );
    }
}

#[test]
fn timeline_time_and_duration_reject_signed_lexical_forms() {
    reject_signs("t=\"0\"", "t", "DASH-TIMELINE-ENTRIES");
    reject_signs("d=\"2000\"", "d", "DASH-TIMELINE-ENTRIES");
}

#[test]
fn unsigned_digits_whitespace_leading_zeroes_and_bounds_keep_their_meaning() {
    for value in ["0", "000", "0001", " 1 ", "&#x9;1&#xA;", "4294967295"] {
        let report =
            lint(&VALID.replace("bandwidth=\"1000000\"", &format!("bandwidth=\"{value}\"")));
        assert!(report.is_clean_for_scope(), "{report:?}");
    }
    for (original, attribute) in [
        ("presentationTimeOffset=\"0\"", "presentationTimeOffset"),
        ("t=\"0\"", "t"),
    ] {
        for value in ["0", "0001", "18446744073709551615"] {
            assert!(
                lint(&VALID.replace(original, &format!("{attribute}=\"{value}\"")))
                    .is_clean_for_scope()
            );
        }
        assert!(
            lint(&VALID.replace(original, &format!("{attribute}=\"18446744073709551616\""))).errors
                > 0
        );
    }
    assert!(lint(&VALID.replace("bandwidth=\"1000000\"", "bandwidth=\"4294967296\"")).errors > 0);
}

#[test]
fn timeline_repeat_is_a_signed_integer_including_signed_zero_and_negative_runs() {
    for value in ["+3", "+0", "-0", "-000", " +0 ", " -1 "] {
        let report = lint(&VALID.replace("r=\"3\"", &format!("r=\"{value}\"")));
        assert!(report.is_clean_for_scope(), "{report:?}");
    }
}
