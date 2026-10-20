use super::*;
use crate::{DocumentId, Format, Options, Profile};

fn run(tags: &str) -> DiagnosticSink<'_> {
    let document = Document {
        id: DocumentId(0),
        format: Format::Hls,
        bytes: tags.as_bytes(),
        base_uri: None,
    };
    let options = Options::new(Profile::HlsBaseManifest);
    let context = LintContext::new(&options, None);
    let mut sink = DiagnosticSink::new(document, options.limits.max_diagnostics);
    lint(&document, &context, &mut sink);
    sink
}

fn media(tags: &str) -> String {
    format!(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n{tags}\n#EXTINF:2,\npart.ts\n#EXT-X-ENDLIST\n"
    )
}
fn has(result: &DiagnosticSink<'_>, id: &str) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.rule_id == id)
}
const PDT: &str = "#EXT-X-PROGRAM-DATE-TIME:2024-02-29T23:59:59.999999999999999999999Z";
const RANGE: &str = "#EXT-X-DATERANGE:ID=\"range\",START-DATE=\"2024-02-29T23:59:59.999999999999999999999Z\",DURATION=0.000000000000000000001,END-DATE=\"2024-03-01T01:00:00+01:00\"";

#[test]
fn date_range_arithmetic_is_exact_across_fraction_carry_day_and_zone_boundaries() {
    let text = media(&format!("{PDT}\n{RANGE}"));
    let good = run(&text);
    assert_eq!(good.errors, 0);
    assert!(!good.incomplete);
    assert_eq!(
        good.coverage["HLS-DATE-RANGE-TIME"].status,
        CheckStatus::Passed
    );
    let wrong = text.replace(
        "DURATION=0.000000000000000000001",
        "DURATION=0.000000000000000000002",
    );
    assert!(has(&run(&wrong), "HLS-DATE-RANGE-TIME"));
    let backwards = text.replace("2024-03-01T01:00:00+01:00", "2024-02-29T23:59:58Z");
    assert!(has(&run(&backwards), "HLS-DATE-RANGE-TIME"));
}

#[test]
fn date_range_updates_merge_without_requiring_repeated_start_dates() {
    let initial =
        "#EXT-X-DATERANGE:ID=\"range\",START-DATE=\"2024-03-01T00:00:00Z\",PLANNED-DURATION=2";
    let text = media(&format!(
        "{PDT}\n{initial}\n#EXT-X-DATERANGE:ID=\"range\",DURATION=2,END-DATE=\"2024-03-01T00:00:02Z\""
    ));
    assert_eq!(run(&text).errors, 0);
    let conflicting = text.replace(
        "ID=\"range\",DURATION=2",
        "ID=\"range\",PLANNED-DURATION=3,DURATION=2",
    );
    assert!(has(&run(&conflicting), "HLS-DATE-RANGE-IDENTITY"));
    let forbidden = text.replace("PLANNED-DURATION=2", "CLASS=\"ads\",END-ON-NEXT=YES");
    assert!(has(&run(&forbidden), "HLS-DATE-RANGE"));
    let missing_pdt = text.replace(&format!("{PDT}\n"), "");
    assert!(has(&run(&missing_pdt), "HLS-DATE-RANGE-PDT"));
    let same_values = media(&format!(
        "{PDT}\n{initial},X-NUMBER=1.0,X-HEX=0xAABB\n#EXT-X-DATERANGE:ID=\"range\",START-DATE=\"2024-03-01T01:00:00.000+01:00\",PLANNED-DURATION=2.000,X-NUMBER=1,X-HEX=0xaabb,DURATION=0"
    ));
    let equal = run(&same_values);
    assert_eq!(equal.errors, 0);
    assert!(!equal.incomplete);
    let different_type = same_values.replace("X-NUMBER=1,", "X-NUMBER=\"1\",");
    assert!(has(&run(&different_type), "HLS-DATE-RANGE-IDENTITY"));
}

#[test]
fn end_on_next_checks_actual_class_overlaps_and_accepts_touching_ranges() {
    let first = "#EXT-X-DATERANGE:ID=\"first\",CLASS=\"ads\",START-DATE=\"2024-03-01T00:00:00Z\",DURATION=2";
    let next = "#EXT-X-DATERANGE:ID=\"next\",CLASS=\"ads\",START-DATE=\"2024-03-01T00:00:02Z\",END-ON-NEXT=YES";
    let last =
        "#EXT-X-DATERANGE:ID=\"last\",CLASS=\"ads\",START-DATE=\"2024-03-01T00:00:04Z\",DURATION=0";
    let text = media(&format!("{PDT}\n{last}\n{next}\n{first}"));
    let good = run(&text);
    assert_eq!(good.errors, 0);
    assert!(!good.incomplete);
    assert_eq!(
        good.coverage["HLS-DATE-RANGE-OVERLAP"].status,
        CheckStatus::Passed
    );
    let wrong = text.replace("DURATION=2", "DURATION=2.000000000000000000001");
    assert!(has(&run(&wrong), "HLS-DATE-RANGE-OVERLAP"));
    let different_class =
        wrong.replace("ID=\"first\",CLASS=\"ads\"", "ID=\"first\",CLASS=\"other\"");
    assert!(!has(&run(&different_class), "HLS-DATE-RANGE-OVERLAP"));
    let unknown_end = text.replace(",DURATION=2", ",PLANNED-DURATION=2");
    assert!(
        run(&unknown_end).coverage["HLS-DATE-RANGE-OVERLAP"]
            .not_checked_reasons
            .contains(&NotCheckedReason::RequiresDeliveryEvidence)
    );
}

#[test]
fn date_grammar_and_metadata_types_reject_invalid_values_without_leaking_text() {
    for value in [
        "not-a-date",
        "2023-02-29T00:00:00Z",
        "1900-02-29T00:00:00Z",
        "2024-13-01T00:00:00Z",
        "2024-01-01T24:00:01Z",
        "2024-01-01T00:00:00.Z",
        "2024-01-01T00:00:00+éé",
        "2024-01-01T00:00:00+24:00",
    ] {
        let text = media(&format!("#EXT-X-PROGRAM-DATE-TIME:{value}"));
        let result = run(&text);
        assert!(has(&result, "HLS-DATE-TIME"), "accepted malformed date");
        assert!(!format!("{:?}", result.diagnostics).contains(value));
    }
    for date in [
        "2000-02-29T00:00:00Z",
        "2024-02-29T24:00:00Z",
        "2024-03-01T01:00:00+01",
        "2024-03-01T01:00:00+0100",
    ] {
        let text = media(&format!("#EXT-X-PROGRAM-DATE-TIME:{date}"));
        assert_eq!(run(&text).errors, 0);
    }
    for attribute in [
        "DURATION=-1",
        "END-ON-NEXT=NO",
        "END-ON-NEXT=YES",
        "X-EXAMPLE=INVALID",
        "SCTE35-OUT=0xnot-hex",
    ] {
        let text = media(&format!(
            "{PDT}\n#EXT-X-DATERANGE:ID=\"r\",START-DATE=\"2024-03-01T00:00:00Z\",{attribute}"
        ));
        assert!(run(&text).errors > 0);
    }
    let extension = media(&format!(
        "{PDT}\n#EXT-X-DATERANGE:ID=\"\",START-DATE=\"2024-03-01T00:00:00Z\",DURATION=0,X-EXAMPLE=\"\",X-NUMBER=1.5,X-HEX=0xFF,SCTE35-CMD=0x00"
    ));
    let good = run(&extension);
    assert_eq!(good.errors, 0);
    assert!(!good.incomplete);
    assert_eq!(
        good.coverage["HLS-SCTE-EVIDENCE"].scope,
        RuleScope::OutsideScope
    );
}

#[test]
fn date_forms_requiring_unavailable_time_evidence_do_not_silently_pass() {
    let text = media("#EXT-X-PROGRAM-DATE-TIME:2016-12-31T23:59:60Z");
    let result = run(&text);
    assert_eq!(result.errors, 0);
    assert!(result.incomplete);
    assert!(
        result.coverage["HLS-DATE-TIME"]
            .not_checked_reasons
            .contains(&NotCheckedReason::UnsupportedFeature)
    );
    let unzoned = media(
        "#EXT-X-PROGRAM-DATE-TIME:2024-03-01T00:00:00\n#EXT-X-DATERANGE:ID=\"r\",START-DATE=\"2024-03-01T00:00:00\",END-DATE=\"2024-03-01T00:00:02\"",
    );
    assert!(
        run(&unzoned).coverage["HLS-DATE-RANGE-TIME"]
            .not_checked_reasons
            .contains(&NotCheckedReason::UnsupportedFeature)
    );
}
