use super::*;

fn record(state: &str, started: u64, active: u64, done: u64, stopped: u64) -> Record {
    let serde_json::Value::Object(record) = serde_json::json!({
        "stage":"rendition-progress", "event":"progress", "batch_id":20,
        "state":state, "operation":"snapshot", "subject":"source",
        "purpose":"conversion", "unit":"tracks", "total_units":7,
        "started_units":started, "active_units":active,
        "waiting_units":7-started, "completed_units":done, "stopped_units":stopped,
        "url":"private-sentinel", "argv":["private-sentinel"],
    }) else {
        unreachable!()
    };
    record
}

fn source() -> State {
    let mut state = State::default();
    estimate_tests::phase(&mut state, "source", "started", 0);
    state
}

#[test]
fn source_task_counts_are_observed_and_cleared_at_the_phase_boundary() {
    let mut state = source();
    assert!(state.observe_nested(&record("started", 0, 0, 0, 0), Duration::ZERO));
    assert!(state.observe_nested(&record("running", 3, 2, 1, 0), Duration::from_secs(1)));
    let value = state.json(Duration::from_secs(1));
    let counts = &value["source_renditions"];
    assert_eq!(counts["active_units"], 2);
    assert_eq!(counts["waiting_units"], 4);
    assert_eq!(counts["completed_units"], 1);
    assert!(!value.to_string().contains("private-sentinel"));
    estimate_tests::phase(&mut state, "source", "aborted", 2);
    assert!(
        state
            .json(Duration::from_secs(2))
            .get("source_renditions")
            .is_none()
    );
    assert!(!state.observe_nested(&record("running", 4, 3, 1, 0), Duration::from_secs(3)));
}

#[test]
fn invalid_and_replayed_task_snapshots_cannot_replace_observed_counts() {
    let mut state = source();
    assert!(state.observe_nested(&record("started", 0, 0, 0, 0), Duration::ZERO));
    assert!(state.observe_nested(&record("running", 3, 2, 1, 0), Duration::from_secs(1)));
    let before = state.json(Duration::from_secs(1))["source_renditions"].clone();
    for mut invalid in [
        record("running", 3, 3, 1, 0),
        record("running", 2, 2, 0, 0),
        record("finished", 3, 0, 3, 0),
        record("running", 3, 2, 1, 0),
    ] {
        invalid.insert("path".into(), "private-sentinel".into());
        assert!(!state.observe_nested(&invalid, Duration::from_secs(2)));
    }
    assert_eq!(
        state.json(Duration::from_secs(2))["source_renditions"],
        before
    );
    for (key, value) in [
        ("total_units", serde_json::json!(65)),
        ("active_units", serde_json::json!(u64::MAX)),
        ("operation", serde_json::json!("private-sentinel")),
        ("subject", serde_json::json!("result")),
        ("purpose", serde_json::json!("verification")),
        ("unit", serde_json::json!("files")),
        ("batch_id", serde_json::json!(0)),
    ] {
        let mut invalid = record("running", 4, 3, 1, 0);
        invalid.insert(key.into(), value);
        assert!(!state.observe_nested(&invalid, Duration::from_secs(3)));
    }
}

#[test]
fn output_context_and_step_boundary_prevent_late_task_state_from_leaking() {
    let mut state = estimate_tests::active(true);
    let mut start = record("started", 0, 0, 0, 0);
    for (key, value) in [
        ("output_index", serde_json::json!(0)),
        ("output_total", serde_json::json!(2)),
        ("output_format", serde_json::json!("dash")),
    ] {
        start.insert(key.into(), value);
    }
    assert!(state.observe_nested(&start, Duration::from_secs(11)));
    let mut running = start.clone();
    running.insert("state".into(), "running".into());
    for (key, value) in [
        ("started_units", 3),
        ("active_units", 2),
        ("completed_units", 1),
        ("waiting_units", 4),
    ] {
        running.insert(key.into(), value.into());
    }
    assert!(state.observe_nested(&running, Duration::from_secs(12)));
    assert_eq!(
        state.json(Duration::from_secs(12))["outputs"][0]["renditions"]["active_units"],
        2
    );
    estimate_tests::step(&mut state, 0, "verification", 13);
    assert!(
        state.json(Duration::from_secs(13))["outputs"][0]
            .get("renditions")
            .is_none()
    );
    assert!(!state.observe_nested(&running, Duration::from_secs(14)));
}

#[test]
fn terminal_command_failure_clears_in_progress_task_observations() {
    let mut state = source();
    assert!(state.observe_nested(&record("started", 0, 0, 0, 0), Duration::ZERO));
    assert!(state.observe_nested(&record("running", 3, 2, 1, 0), Duration::from_secs(1)));
    state.stop_estimates();
    assert!(
        state
            .json(Duration::from_secs(2))
            .get("source_renditions")
            .is_none()
    );
    assert!(!state.observe_nested(&record("running", 4, 3, 1, 0), Duration::from_secs(3)));
}
