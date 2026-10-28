//! Source admission counters are visible without calibrating work already represented by the plan.
use super::*;

fn source(estimates: Option<bool>) -> State {
    let mut state = State::default();
    estimate_tests::phase(&mut state, "source", "started", 0);
    if let Some(enabled) = estimates {
        estimate_tests::plan(&mut state, enabled);
    }
    state
}

fn rendition(status: &str, started: u64, active: u64, done: u64) -> Record {
    serde_json::from_value(serde_json::json!({
        "stage":"rendition-progress", "event":"progress", "batch_id":20,
        "state":status, "operation":"preparation", "subject":"source",
        "purpose":"admission", "unit":"renditions", "total_units":4,
        "started_units":started, "active_units":active, "waiting_units":4-started,
        "completed_units":done, "stopped_units":0,
        "url":"private-sentinel", "argv":["private-sentinel"]
    }))
    .unwrap()
}

fn media(purpose: &str, status: &str, id: u64, done: u64) -> Record {
    serde_json::from_value(serde_json::json!({
        "stage":"media-progress", "event":"progress", "task":"preparation",
        "subject":"source", "purpose":purpose, "unit":"renditions", "state":status,
        "batch_id":id, "estimated":false, "completed_units":done, "total_units":4,
        "path":"private-sentinel"
    }))
    .unwrap()
}

#[test]
fn source_admission_renditions_are_visible_without_estimated_work() {
    for enabled in [Some(true), Some(false), None] {
        let mut state = source(enabled);
        assert!(state.observe_nested(&rendition("started", 0, 0, 0), Duration::from_secs(2)));
        assert!(state.observe_nested(&rendition("running", 3, 2, 1), Duration::from_secs(3)));
        let work = state.source_renditions().unwrap();
        assert_eq!(work.label(false), "Checking source media");
        assert_eq!(work.counts(false), "2 active, 1 waiting, 1/4 done");
        let value = state.json(Duration::from_secs(3));
        assert_eq!(value["source_renditions"]["purpose"], "admission");
        assert_eq!(value["source_renditions"]["active_units"], 2);
        assert!(!value.to_string().contains("private-sentinel"));
        assert_eq!(state.source_work.fraction(), 0);
        assert_eq!(state.source_work.observations(), 0);
    }
}

#[test]
fn source_admission_media_tail_is_visible_without_advancing_estimates() {
    for enabled in [Some(true), Some(false), None] {
        let mut state = source(enabled);
        let before = state.json(Duration::from_secs(2)).get("estimate").cloned();
        assert!(state.observe_nested(&media("admission", "started", 1, 0), Duration::from_secs(2)));
        assert!(state.observe_nested(&media("admission", "running", 1, 4), Duration::from_secs(3)));
        let current = state.source_media().unwrap();
        assert!(current.active() && current.checking());
        let value = state.json(Duration::from_secs(4));
        assert_eq!(value["source_media"]["state"], "running");
        assert_eq!(value["source_media"]["purpose"], "admission");
        assert_eq!(value["source_media"]["completed_units"], 4);
        assert_eq!(state.lifetime(Phase::Source).status, Status::Running);
        assert_eq!(state.source_work.fraction(), 0);
        assert_eq!(state.source_work.observations(), 0);
        assert_eq!(value.get("estimate").cloned(), before);
        assert!(!value.to_string().contains("private-sentinel"));
        assert!(!state.observe_nested(
            &media("conversion", "started", 2, 0),
            Duration::from_secs(4)
        ));
        assert!(state.observe_nested(
            &media("admission", "finished", 1, 4),
            Duration::from_secs(5)
        ));
        assert!(state.source_media().is_none());
        assert_eq!(
            state.json(Duration::from_secs(5))["source_media"]["state"],
            "finished"
        );
        assert_eq!(
            state.json(Duration::from_secs(5)).get("estimate").cloned(),
            before
        );
    }
}

#[test]
fn source_admission_does_not_calibrate_conversion_work_or_rates() {
    let mut observed = source(Some(true));
    let mut baseline = source(Some(true));
    for (status, done, second) in [("started", 0, 2), ("running", 2, 4), ("finished", 4, 8)] {
        assert!(observed.observe_nested(
            &media("admission", status, 1, done),
            Duration::from_secs(second)
        ));
    }
    for (status, done, second) in [("started", 0, 10), ("running", 2, 12), ("finished", 4, 14)] {
        let record = media("conversion", status, 2, done);
        assert!(observed.observe_nested(&record, Duration::from_secs(second)));
        assert!(baseline.observe_nested(&record, Duration::from_secs(second)));
        assert_eq!(
            observed.json(Duration::from_secs(second))["estimate"],
            baseline.json(Duration::from_secs(second))["estimate"]
        );
    }
    assert_eq!(
        observed.source_work.observations(),
        baseline.source_work.observations()
    );
    assert_eq!(
        observed.source_work.fraction(),
        baseline.source_work.fraction()
    );
    let before = observed.json(Duration::from_secs(15));
    assert!(!observed.observe_nested(
        &media("admission", "running", 1, 4),
        Duration::from_secs(15)
    ));
    assert_eq!(observed.json(Duration::from_secs(15)), before);
}

#[test]
fn source_admission_observations_require_known_tuples_and_a_live_source_phase() {
    let mut state = source(None);
    let before = state.json(Duration::ZERO);
    for (field, value) in [
        ("operation", serde_json::json!("snapshot")),
        ("operation", serde_json::json!("comparison")),
        ("purpose", serde_json::json!("private-sentinel")),
        ("subject", serde_json::json!("private-sentinel")),
        ("total_units", serde_json::json!(65)),
    ] {
        let mut invalid = rendition("started", 0, 0, 0);
        invalid.insert(field.into(), value);
        assert!(!state.observe_nested(&invalid, Duration::ZERO));
    }
    for purpose in ["verification", "private-sentinel"] {
        assert!(!state.observe_nested(&media(purpose, "started", 1, 0), Duration::ZERO));
    }
    assert_eq!(state.json(Duration::ZERO), before);
    let mut inactive = State::default();
    assert!(!inactive.observe_nested(&rendition("started", 0, 0, 0), Duration::ZERO));
    assert!(!inactive.observe_nested(&media("admission", "started", 1, 0), Duration::ZERO));
    estimate_tests::phase(&mut state, "source", "finished", 1);
    assert!(!state.observe_nested(&rendition("started", 0, 0, 0), Duration::from_secs(2)));
    assert!(!state.observe_nested(&media("admission", "started", 1, 0), Duration::from_secs(2)));
}
