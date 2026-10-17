//! Check identifiers, status parsing, and console label coverage.

use drmr_core::checks::{CheckId, Status};
use std::collections::BTreeSet;
use std::path::PathBuf;

#[test]
fn every_id_round_trips_and_the_console_help_table_names_the_same_ids() {
    for id in CheckId::ALL {
        assert_eq!(id.as_str().parse::<CheckId>().unwrap(), *id);
        assert_eq!(id.to_string(), id.as_str());
        assert_eq!(
            serde_json::to_string(id).unwrap(),
            format!("\"{}\"", id.as_str())
        );
        assert_eq!(
            serde_json::from_str::<CheckId>(&format!("\"{}\"", id.as_str())).unwrap(),
            *id
        );
    }
    assert!("6-9.nothing".parse::<CheckId>().is_err());
    assert_eq!(CheckId::ALL.iter().filter(|id| id.is_catalog()).count(), 21);
    assert_eq!(CheckId::ALL.len(), 28);

    // The console explains every check behind its (i); the ids it knows are the ids the
    // pipelines record, and the fake-DOM tests pin the console's list to its rules.
    let text = i18n_text();
    let pattern = regex::Regex::new(r#"["']help_([A-Za-z0-9._-]+)["']"#).unwrap();
    let help: BTreeSet<String> = pattern
        .captures_iter(&text)
        .map(|c| c[1].to_string())
        .collect();
    let all: BTreeSet<String> = CheckId::ALL
        .iter()
        .filter(|id| !CONSOLE_UNEXPLAINED.contains(id))
        .map(|id| id.as_str().to_string())
        .collect();
    assert_eq!(help, all);
}

/// Ids a pipeline records that the console's tables do not explain: `source.decrypt`
/// (`pipeline`'s row for a source that cannot be decrypted) has neither a `check_` label nor
/// a `help_` text in `web/static/js/i18n.js` — a console gap, not a Rust choice.
const CONSOLE_UNEXPLAINED: &[CheckId] = &[CheckId::SourceDecrypt];

#[test]
fn statuses_parse_and_print_as_the_reports_write_them() {
    assert_eq!(Status::parse("PASS"), Some(Status::Pass));
    assert_eq!(
        Status::parse("FAIL: no such file"),
        Some(Status::Fail(Some("no such file".into())))
    );
    assert_eq!(Status::parse("SKIP"), Some(Status::Skip(None)));
    assert_eq!(
        Status::parse("SKIP: DASH-IF validator not yet integrated")
            .unwrap()
            .verdict(),
        "SKIP"
    );
    assert_eq!(Status::parse("HUMAN-PASS"), None);
    assert_eq!(Status::parse("pass"), None);
    assert_eq!(Status::Fail(Some("x".into())).to_string(), "FAIL: x");
    assert_eq!(
        serde_json::to_string(&Status::Skip(None)).unwrap(),
        "\"SKIP\""
    );
    assert_eq!(
        serde_json::from_str::<Status>("\"FAIL: reason\"").unwrap(),
        Status::Fail(Some("reason".into()))
    );
    assert!(serde_json::from_str::<Status>("\"nope\"").is_err());
}

#[test]
fn every_check_id_has_its_label_in_the_console_tables() {
    // `check_<id>` is the label the console shows (status words `check_PASS` … are not ids).
    let text = i18n_text();
    let pattern = regex::Regex::new(r#"["']check_([A-Za-z0-9._-]+)["']"#).unwrap();
    let labels: BTreeSet<String> = pattern
        .captures_iter(&text)
        .map(|c| c[1].to_string())
        .collect();
    for id in CheckId::ALL {
        if CONSOLE_UNEXPLAINED.contains(id) {
            assert!(
                !labels.contains(id.as_str()),
                "{id} is labelled now: drop the exception"
            );
            continue;
        }
        assert!(
            labels.contains(id.as_str()),
            "check_{} missing from i18n.js",
            id.as_str()
        );
    }
}

/// Load the console labels from this workspace so missing UI files fail the cross-check.
fn i18n_text() -> String {
    let i18n = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../drmr-server/static/js/i18n.js");
    std::fs::read_to_string(&i18n).unwrap_or_else(|e| panic!("{}: {e}", i18n.display()))
}
