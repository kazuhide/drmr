use drmr_manifest::{
    CheckStatus, Document, DocumentId, Format, NotCheckedReason, Options, Profile, lint_document,
};
use std::fmt::Write;
use std::num::NonZeroUsize;

fn many_representations(invalid: bool) -> String {
    let mut text = String::from(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" profiles="urn:mpeg:dash:profile:isoff-live:2011" minBufferTime="PT1S" mediaPresentationDuration="PT20S"><Period><AdaptationSet><SegmentTemplate media="$Number$.m4s" initialization="init.mp4" timescale="1000"><SegmentTimeline><S d="2000" r="9"/></SegmentTimeline></SegmentTemplate>"#,
    );
    for index in 0..4096 {
        let bandwidth = if invalid {
            ""
        } else {
            " bandwidth=\"1000000\""
        };
        write!(text, r#"<Representation id="r{index}"{bandwidth} width="1920" height="1080" frameRate="30000/1001" codecs="avc1.4d401f"/>"#).unwrap();
    }
    text.push_str("</AdaptationSet></Period></MPD>");
    text
}

#[test]
fn a_single_mpd_shares_one_parse_and_runs_semantic_work_in_parallel() {
    for invalid in [false, true] {
        let text = many_representations(invalid);
        let document = Document {
            id: DocumentId(1),
            format: Format::Dash,
            bytes: text.as_bytes(),
            base_uri: None,
        };
        let mut options = Options::new(Profile::DashStaticManifest);
        options.limits.max_diagnostics = 3;
        options.workers = NonZeroUsize::new(1);
        let mut serial = lint_document(document, &options).unwrap();
        options.workers = NonZeroUsize::new(8);
        let mut parallel = lint_document(document, &options).unwrap();
        assert!(
            parallel.workers_used > 1,
            "large single MPD must admit semantic workers"
        );
        assert_eq!(serial.workers_used, 1);
        if invalid {
            assert!(parallel.truncated && parallel.incomplete);
            assert_eq!(parallel.errors, 4096);
        } else {
            assert!(parallel.is_clean_for_scope(), "{parallel:?}");
        }
        // A shared inherited timeline is checked once, not once per Representation.
        assert_eq!(
            parallel
                .coverage
                .iter()
                .find(|item| item.rule_id == "DASH-TIMELINE-ENTRIES")
                .unwrap()
                .passed_checks,
            2
        );
        serial.workers_used = 0;
        parallel.workers_used = 0;
        assert_eq!(serial, parallel);
    }
}

#[test]
fn multiple_period_boundary_evidence_is_identical_for_one_and_many_workers() {
    let source = many_representations(false);
    let (head, remaining) = source.split_once("<Period>").unwrap();
    let body = remaining.split_once("</Period>").unwrap().0;
    let head = head.replace(" mediaPresentationDuration=\"PT20S\"", "");
    let repeated = body.replace("r=\"9\"", "r=\"-1\"");
    for bounded in [false, true] {
        let (first, last) = if bounded {
            (" duration=\"PT20S\"", "")
        } else {
            ("", " duration=\"PT20S\"")
        };
        let text =
            format!("{head}<Period{first}>{repeated}</Period><Period{last}>{body}</Period></MPD>");
        let document = Document {
            id: DocumentId(1),
            format: Format::Dash,
            bytes: text.as_bytes(),
            base_uri: None,
        };
        let mut options = Options::new(Profile::DashStaticManifest);
        options.workers = NonZeroUsize::new(1);
        let mut serial = lint_document(document, &options).unwrap();
        options.workers = NonZeroUsize::new(8);
        let mut parallel = lint_document(document, &options).unwrap();
        assert!(parallel.workers_used > 1);
        assert_eq!(parallel.errors, 0);
        assert_eq!(parallel.is_clean_for_scope(), bounded);
        assert_eq!(parallel.incomplete, !bounded);
        serial.workers_used = 0;
        parallel.workers_used = 0;
        assert_eq!(serial, parallel);
    }
}

fn terminal_contexts(invalid: bool) -> String {
    let mut text = String::from(
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" profiles="urn:mpeg:dash:profile:isoff-live:2011" minBufferTime="PT1S" mediaPresentationDuration="PT1S"><Period><AdaptationSet><SegmentTemplate media="$Number$.m4s" timescale="1000"><SegmentTimeline><S t="1000" d="2000" r="-1"/></SegmentTimeline></SegmentTemplate>"#,
    );
    for index in 0..4096 {
        let bandwidth = if invalid && index % 257 == 0 {
            ""
        } else {
            " bandwidth=\"1\""
        };
        let offset = if invalid { index } else { index + 1 };
        write!(text, r#"<Representation id="r{index}"{bandwidth}><SegmentTemplate presentationTimeOffset="{offset}"/></Representation>"#).unwrap();
    }
    text.push_str("</AdaptationSet></Period></MPD>");
    text
}

#[test]
fn active_terminal_contexts_reuse_structure_and_merge_deterministically() {
    for invalid in [false, true] {
        let text = terminal_contexts(invalid);
        let document = Document {
            id: DocumentId(1),
            format: Format::Dash,
            bytes: text.as_bytes(),
            base_uri: None,
        };
        let mut options = Options::new(Profile::DashStaticManifest);
        options.limits.max_diagnostics = 3;
        options.workers = NonZeroUsize::new(1);
        let mut serial = lint_document(document, &options).unwrap();
        options.workers = NonZeroUsize::new(8);
        let mut parallel = lint_document(document, &options).unwrap();
        assert!(parallel.workers_used > 1);
        let structure = parallel
            .coverage
            .iter()
            .find(|item| item.rule_id == "DASH-TIMELINE-ENTRIES")
            .unwrap();
        assert_eq!(structure.passed_checks, 2);
        let arithmetic = parallel
            .coverage
            .iter()
            .find(|item| item.rule_id == "DASH-TIMELINE-ARITHMETIC")
            .unwrap();
        if invalid {
            assert_eq!(arithmetic.passed_checks, 4095);
            assert_eq!(arithmetic.not_checked_checks, 1);
            assert_eq!(
                arithmetic.status,
                CheckStatus::NotChecked(NotCheckedReason::InvalidPrerequisite)
            );
            assert!(parallel.truncated && parallel.incomplete);
            assert_eq!(parallel.errors, 16);
        } else {
            assert_eq!(arithmetic.passed_checks, 4096);
            assert!(parallel.is_clean_for_scope(), "{parallel:?}");
        }
        serial.workers_used = 0;
        parallel.workers_used = 0;
        assert_eq!(serial, parallel);
    }
}
