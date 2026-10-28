use super::*;

fn record(id: u64, states: &[&str]) -> Record {
    let waiting = states.iter().filter(|&&value| value == "waiting").count();
    let active = states.iter().filter(|&&value| value == "active").count();
    let completed = states.iter().filter(|&&value| value == "completed").count();
    let stopped = states.iter().filter(|&&value| value == "stopped").count();
    serde_json::json!({
        "batch_id":id, "operation":"preparation", "state":if waiting == states.len() {"started"} else {"running"},
        "subject":"source", "purpose":"conversion", "unit":"renditions",
        "total_units":states.len(), "started_units":states.len()-waiting,
        "active_units":active, "waiting_units":waiting,
        "completed_units":completed, "stopped_units":stopped,
        "unit_states":states,
    })
    .as_object()
    .unwrap()
    .clone()
}

#[test]
fn unit_details_preserve_actual_ordinals_through_json() {
    let input = record(1, &["active", "waiting", "completed", "stopped"]);
    let work = Work::parse(&input).unwrap();
    assert_eq!(work.json()["unit_states"], input["unit_states"]);
    assert_eq!(work.json()["active_units"], 1);
    assert_eq!(work.json()["waiting_units"], 1);
    assert_eq!(work.json()["completed_units"], 1);
    assert_eq!(work.json()["stopped_units"], 1);
}

#[test]
fn malformed_or_inconsistent_unit_details_cannot_replace_valid_counts() {
    let valid = record(1, &["active", "waiting"]);
    for details in [
        serde_json::json!(null),
        serde_json::json!("active"),
        serde_json::json!([]),
        serde_json::json!(["active"]),
        serde_json::json!(["active", "waiting", "waiting"]),
        serde_json::json!(["active", "unknown"]),
        serde_json::json!(["active", 0]),
        serde_json::json!(["waiting", "waiting"]),
        serde_json::json!(["completed", "waiting"]),
    ] {
        let mut input = valid.clone();
        input.insert("unit_states".into(), details.clone());
        assert!(Work::parse(&input).is_none(), "{details}");
    }
}

#[test]
fn ordinal_reversal_is_rejected_even_when_all_aggregate_counts_advance() {
    let mut tracker = Tracker::default();
    assert!(tracker.observe(&record(1, &["waiting", "waiting", "waiting"])));
    assert!(tracker.observe(&record(1, &["active", "active", "waiting"])));
    assert!(!tracker.observe(&record(1, &["waiting", "active", "completed"])));
    assert!(tracker.observe(&record(1, &["completed", "active", "waiting"])));
    assert_eq!(
        tracker.current().unwrap().json()["unit_states"],
        serde_json::json!(["completed", "active", "waiting"])
    );
}

#[test]
fn optional_legacy_counts_remain_supported_without_inventing_ordinals() {
    let mut legacy = record(1, &["waiting", "waiting"]);
    legacy.remove("unit_states");
    let mut tracker = Tracker::default();
    assert!(tracker.observe(&legacy));
    assert!(
        tracker
            .current()
            .unwrap()
            .json()
            .get("unit_states")
            .is_none()
    );
    // Adding actual observations can be useful without changing the totals.
    let mut observed = record(1, &["waiting", "waiting"]);
    observed.insert("state".into(), "running".into());
    assert!(tracker.observe(&observed));
    assert_eq!(
        tracker.current().unwrap().json()["unit_states"],
        serde_json::json!(["waiting", "waiting"])
    );
}

#[test]
fn details_cannot_disappear_mid_batch_or_reappear_after_clear() {
    let mut tracker = Tracker::default();
    assert!(tracker.observe(&record(1, &["waiting", "waiting"])));
    assert!(tracker.observe(&record(1, &["active", "waiting"])));
    let mut without = record(1, &["completed", "active"]);
    without.remove("unit_states");
    assert!(!tracker.observe(&without));
    tracker.clear();
    assert!(!tracker.observe(&record(1, &["completed", "active"])));
    assert!(tracker.current().is_none());
    assert!(tracker.observe(&record(2, &["waiting", "waiting"])));
}

#[test]
fn detail_snapshots_are_bounded_and_do_not_echo_arbitrary_labels() {
    let mut states = vec!["waiting"; 64];
    states[63] = "active";
    let mut input = record(1, &states);
    input.insert(
        "unit_names".into(),
        serde_json::json!(["private-path-and-key-sentinel"]),
    );
    let value = Work::parse(&input).unwrap().json();
    assert_eq!(value["unit_states"].as_array().unwrap().len(), 64);
    assert_eq!(value["unit_states"][63], "active");
    assert!(!value.to_string().contains("private-path-and-key-sentinel"));
    states.push("waiting");
    assert!(Work::parse(&record(2, &states)).is_none());
}

#[test]
fn unit_descriptions_preserve_only_known_kind_and_numeric_resolution() {
    let mut input = record(1, &["active", "waiting", "waiting"]);
    input.insert(
        "unit_descriptions".into(),
        serde_json::json!([
            {"kind":"video", "resolution":[1920,1080], "name":"private-label-sentinel"},
            {"kind":"audio", "language":"private-language-sentinel"},
            null
        ]),
    );
    let value = Work::parse(&input).unwrap().json();
    assert_eq!(
        value["unit_descriptions"],
        serde_json::json!([
            {"kind":"video", "resolution":[1920,1080]}, {"kind":"audio"}, null
        ])
    );
    assert!(!value.to_string().contains("private-label-sentinel"));
    assert!(!value.to_string().contains("private-language-sentinel"));
}

#[test]
fn malformed_unit_descriptions_do_not_change_the_current_snapshot() {
    let original = record(1, &["active", "waiting"]);
    for descriptions in [
        serde_json::json!(null),
        serde_json::json!([]),
        serde_json::json!([null]),
        serde_json::json!([null, null, null]),
        serde_json::json!(["video", null]),
        serde_json::json!([{"kind":"private-kind-sentinel"}, null]),
        serde_json::json!([{"kind":"unknown"}, null]),
        serde_json::json!([{"kind":"video","resolution":null}, null]),
        serde_json::json!([{"kind":"video","resolution":[0,1080]}, null]),
        serde_json::json!([{"kind":"video","resolution":[1920,0]}, null]),
        serde_json::json!([{"kind":"video","resolution":[1920,1080,1]}, null]),
        serde_json::json!([{"kind":"video","resolution":[1920,1.5]}, null]),
        serde_json::json!([{"kind":"video","resolution":[1920,4_294_967_296_u64]}, null]),
        serde_json::json!([{"kind":"audio","resolution":[1920,1080]}, null]),
    ] {
        let mut input = original.clone();
        input.insert("unit_descriptions".into(), descriptions.clone());
        assert!(Work::parse(&input).is_none(), "{descriptions}");
    }
}

#[test]
fn descriptions_cannot_be_attached_without_actual_ordinal_states() {
    let mut input = record(1, &["waiting", "waiting"]);
    input.remove("unit_states");
    input.insert(
        "unit_descriptions".into(),
        serde_json::json!([{"kind":"video"}, null]),
    );
    assert!(Work::parse(&input).is_none());
}

#[test]
fn known_descriptions_cannot_change_or_disappear_within_a_batch() {
    let mut tracker = Tracker::default();
    let mut started = record(1, &["waiting", "waiting"]);
    started.insert("unit_descriptions".into(), serde_json::json!([null, null]));
    assert!(tracker.observe(&started));
    let descriptions = serde_json::json!([{"kind":"video", "resolution":[640,360]}, null]);
    let mut active = record(1, &["active", "waiting"]);
    active.insert("unit_descriptions".into(), descriptions.clone());
    assert!(tracker.observe(&active));
    for changed in [
        serde_json::json!([{"kind":"audio"}, null]),
        serde_json::json!([{"kind":"video", "resolution":[1920,1080]}, null]),
        serde_json::json!([null, null]),
    ] {
        let mut next = record(1, &["completed", "active"]);
        next.insert("unit_descriptions".into(), changed.clone());
        assert!(!tracker.observe(&next), "{changed}");
    }
    assert_eq!(
        tracker.current().unwrap().json()["unit_descriptions"],
        descriptions
    );
    assert!(!tracker.observe(&record(1, &["completed", "active"])));
}

#[test]
fn a_late_description_cannot_relabel_a_completed_task() {
    let mut tracker = Tracker::default();
    let mut input = record(1, &["waiting", "waiting"]);
    input.insert("unit_descriptions".into(), serde_json::json!([null, null]));
    assert!(tracker.observe(&input));
    input = record(1, &["completed", "waiting"]);
    input.insert("unit_descriptions".into(), serde_json::json!([null, null]));
    assert!(tracker.observe(&input));
    input = record(1, &["completed", "active"]);
    input.insert(
        "unit_descriptions".into(),
        serde_json::json!([{"kind":"video"}, null]),
    );
    assert!(!tracker.observe(&input));
}
