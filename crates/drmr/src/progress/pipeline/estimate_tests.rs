use super::*;

fn item(value: serde_json::Value) -> Record {
    let serde_json::Value::Object(record) = value else {
        panic!("expected a progress event object");
    };
    record
}

fn phase(state: &mut State, scope: &str, status: &str, second: u64) {
    let _ = state.observe(
        &item(serde_json::json!({"scope":scope,"state":status})),
        Duration::from_secs(second),
    );
}

fn plan(state: &mut State, enabled: bool) {
    let _ = state.observe(
        &item(serde_json::json!({"scope":"work-plan","state":"ready","planned_outputs":2,"estimates_enabled":enabled,"source_completed":2})),
        Duration::from_secs(1),
    );
}

fn output_record(index: usize, scope: &str, state: &str) -> Record {
    item(
        serde_json::json!({"scope":scope,"state":state,"output_index":index,"output_total":2,"output_format":if index==0 {"dash"} else {"hls"}}),
    )
}

fn step(state: &mut State, index: usize, value: &str, second: u64) {
    let mut record = output_record(index, "output-step", "started");
    record.insert("step".into(), value.into());
    let _ = state.observe(&record, Duration::from_secs(second));
}

fn active(enabled: bool) -> State {
    let mut state = State::default();
    phase(&mut state, "source", "started", 0);
    plan(&mut state, enabled);
    phase(&mut state, "source", "finished", 10);
    phase(&mut state, "outputs", "started", 10);
    for index in 0..2 {
        let _ = state.observe(
            &output_record(index, "output", "started"),
            Duration::from_secs(10),
        );
        step(&mut state, index, "conversion", 10);
    }
    state
}

fn media(index: usize, task: &str, status: &str, id: u64, done: u64, total: u64) -> Record {
    let mut record = output_record(index, "unused", "unused");
    record.remove("scope");
    record.extend(item(serde_json::json!({"stage":"media-progress","event":"progress","task":task,"state":status,"batch_id":id,"completed_bytes":done,"total_bytes":total,"estimated":task=="packaging","url":"private-sentinel","argv":["private-sentinel"]})));
    if task == "comparison" && total > 0 {
        record.insert("total_units".into(), 4.into());
        record.insert(
            "completed_units".into(),
            u64::try_from((u128::from(done) * 4) / u128::from(total))
                .unwrap_or(u64::MAX)
                .into(),
        );
    }
    record
}

fn observe(state: &mut State, record: &Record, second: u64) {
    let _ = state.observe_nested(record, Duration::from_secs(second));
}

#[test]
fn generated_bytes_are_independent_estimated_work_and_never_a_success_claim() {
    let mut state = active(true);
    observe(
        &mut state,
        &media(0, "packaging", "started", 10, 0, 1000),
        10,
    );
    observe(
        &mut state,
        &media(1, "packaging", "started", 11, 0, 1000),
        10,
    );
    observe(
        &mut state,
        &media(0, "packaging", "running", 10, 100, 1000),
        12,
    );
    observe(
        &mut state,
        &media(0, "packaging", "running", 10, 400, 1000),
        14,
    );
    observe(
        &mut state,
        &media(1, "packaging", "running", 11, 800, 1000),
        14,
    );
    let value = state.json(Duration::from_secs(15));
    assert_eq!(value["outputs"][0]["media"]["completed_bytes"], 400);
    assert_eq!(value["outputs"][1]["media"]["completed_bytes"], 800);
    assert_eq!(value["outputs"][0]["step"], "conversion");
    assert_eq!(value["estimate"]["basis"], "logical-media-passes");
    assert_eq!(value["estimate"]["estimated"], true);
    assert!(value["estimate"]["percent"].as_u64().unwrap() < 100);
    assert!(value["estimate"]["remaining_ms"].as_u64().unwrap() > 0);
    assert_eq!(value["checked_outputs"], 0);
    assert!(!value.to_string().contains("private-sentinel"));
}

#[test]
fn comparison_batches_and_forward_steps_cannot_mix_or_rewind_progress() {
    let mut state = active(true);
    observe(
        &mut state,
        &media(0, "packaging", "started", 10, 0, 1000),
        10,
    );
    observe(
        &mut state,
        &media(0, "packaging", "finished", 10, 1200, 1000),
        12,
    );
    observe(
        &mut state,
        &media(0, "comparison", "started", 12, 0, 2000),
        13,
    );
    let mut first = media(0, "comparison", "running", 12, 500, 2000);
    first.insert("completed_units".into(), 1.into());
    first.insert("total_units".into(), 4.into());
    observe(&mut state, &first, 14);
    let old = state.json(Duration::from_secs(14))["outputs"][0]["estimated_work_percent"]
        .as_u64()
        .unwrap();
    observe(
        &mut state,
        &media(0, "comparison", "finished", 12, 2000, 2000),
        15,
    );
    step(&mut state, 0, "verification", 16);
    observe(
        &mut state,
        &media(0, "comparison", "started", 13, 0, 2000),
        16,
    );
    observe(
        &mut state,
        &media(0, "comparison", "running", 13, 1500, 2000),
        18,
    );
    let before = state.json(Duration::from_secs(18))["outputs"][0].clone();
    assert!(before["estimated_work_percent"].as_u64().unwrap() > old);
    assert!(!state.observe_nested(&first, Duration::from_secs(19)));
    assert!(!state.observe_nested(
        &media(0, "comparison", "running", 13, 1000, 2000),
        Duration::from_secs(19)
    ));
    assert!(!state.observe_nested(
        &media(0, "comparison", "running", 13, 2500, 2000),
        Duration::from_secs(19)
    ));
    step(&mut state, 0, "conversion", 19);
    let after = state.json(Duration::from_secs(19))["outputs"][0].clone();
    assert_eq!(after["media"], before["media"]);
    assert_eq!(
        after["estimated_work_percent"],
        before["estimated_work_percent"]
    );
    assert_eq!(after["step"], "verification");
}

#[test]
fn all_checked_outputs_leave_guard_work_until_actual_publication_finishes() {
    let mut state = active(true);
    for index in 0..2 {
        let _ = state.observe(
            &output_record(index, "output", "finished"),
            Duration::from_secs(20),
        );
    }
    assert_eq!(state.checked_outputs(), 2);
    assert!(
        state.json(Duration::from_secs(20))["estimate"]["percent"]
            .as_u64()
            .unwrap()
            < 100
    );
    phase(&mut state, "outputs", "finished", 20);
    phase(&mut state, "final-check", "started", 20);
    phase(&mut state, "publication", "started", 20);
    phase(&mut state, "final-check", "finished", 22);
    assert!(
        state.json(Duration::from_secs(22))["estimate"]["percent"]
            .as_u64()
            .unwrap()
            < 100
    );
    phase(&mut state, "publication", "finished", 23);
    let value = state.json(Duration::from_secs(23));
    assert_eq!(value["estimate"]["percent"], 100);
    assert_eq!(value["estimate"]["remaining_ms"], 0);
}

#[test]
fn disabled_or_aborted_work_never_has_a_deadline_or_false_completion() {
    let mut disabled = active(false);
    observe(
        &mut disabled,
        &media(0, "packaging", "started", 1, 0, 1000),
        10,
    );
    observe(
        &mut disabled,
        &media(0, "packaging", "running", 1, 500, 1000),
        12,
    );
    let value = disabled.json(Duration::from_secs(12));
    assert!(value.get("estimate").is_none());
    assert!(value["outputs"][0].get("estimated_work_percent").is_none());
    assert_eq!(value["outputs"][0]["media"]["completed_bytes"], 500);
    let mut state = active(true);
    observe(
        &mut state,
        &media(0, "packaging", "started", 2, 0, 1000),
        10,
    );
    observe(
        &mut state,
        &media(0, "packaging", "running", 2, 100, 1000),
        12,
    );
    observe(
        &mut state,
        &media(0, "packaging", "running", 2, 900, 1000),
        14,
    );
    let _ = state.observe(
        &output_record(0, "output", "aborted"),
        Duration::from_secs(15),
    );
    phase(&mut state, "outputs", "aborted", 15);
    let value = state.json(Duration::from_secs(15));
    assert!(value["estimate"]["percent"].as_u64().unwrap() < 100);
    assert!(value["estimate"]["remaining_ms"].is_null());
}

#[test]
fn eta_requires_observed_work_and_grows_when_that_work_stalls() {
    let mut state = active(true);
    observe(
        &mut state,
        &media(0, "packaging", "started", 1, 0, 1000),
        10,
    );
    observe(
        &mut state,
        &media(0, "packaging", "running", 1, 100, 1000),
        12,
    );
    assert!(state.json(Duration::from_secs(12))["estimate"]["remaining_ms"].is_null());
    observe(
        &mut state,
        &media(0, "packaging", "running", 1, 500, 1000),
        14,
    );
    let early = state.json(Duration::from_secs(15));
    let stalled = state.json(Duration::from_secs(30));
    assert_eq!(early["estimate"]["percent"], stalled["estimate"]["percent"]);
    assert!(
        stalled["estimate"]["remaining_ms"].as_u64().unwrap()
            > early["estimate"]["remaining_ms"].as_u64().unwrap()
    );
    assert!(state.json(Duration::MAX)["estimate"]["remaining_ms"].is_null());
}

#[test]
fn malformed_or_oversized_records_do_not_mutate_bounded_numeric_state() {
    let mut state = active(true);
    observe(&mut state, &media(0, "packaging", "started", 1, 0, 1), 10);
    observe(
        &mut state,
        &media(0, "packaging", "running", 1, u64::MAX, 1),
        12,
    );
    let before = state.json(Duration::from_secs(12));
    assert!(
        before["outputs"][0]["estimated_work_percent"]
            .as_u64()
            .unwrap()
            < 100
    );
    for mut record in [
        media(0, "private-sentinel", "running", 1, u64::MAX, 1),
        media(0, "packaging", "running", 0, 0, 1),
        media(0, "packaging", "running", 1, 0, 2),
    ] {
        record.insert("path".into(), "private-sentinel".into());
        assert!(!state.observe_nested(&record, Duration::from_secs(13)));
    }
    let after = state.json(Duration::from_secs(13));
    assert_eq!(before["outputs"][0]["media"], after["outputs"][0]["media"]);
    assert_eq!(
        before["outputs"][0]["estimated_work_percent"],
        after["outputs"][0]["estimated_work_percent"]
    );
    assert!(!after.to_string().contains("private-sentinel"));
}

#[test]
fn work_plan_identity_is_fixed_and_malformed_fields_are_not_retained() {
    let mut state = State::default();
    phase(&mut state, "source", "started", 0);
    for value in [
        serde_json::json!({"scope":"work-plan","state":"ready","planned_outputs":9,"estimates_enabled":true,"source_completed":2}),
        serde_json::json!({"scope":"work-plan","state":"ready","planned_outputs":2,"estimates_enabled":"private-sentinel","source_completed":2}),
        serde_json::json!({"scope":"work-plan","state":"ready","planned_outputs":2,"estimates_enabled":true,"source_completed":5}),
    ] {
        assert!(!state.observe(&item(value), Duration::from_secs(1)));
    }
    assert!(state.json(Duration::from_secs(1)).get("estimate").is_none());
    plan(&mut state, true);
    let before = state.json(Duration::from_secs(2))["estimate"].clone();
    for value in [
        serde_json::json!({"scope":"work-plan","state":"ready","planned_outputs":2,"estimates_enabled":true,"source_completed":2}),
        serde_json::json!({"scope":"work-plan","state":"ready","planned_outputs":1,"estimates_enabled":false,"source_completed":4}),
    ] {
        assert!(!state.observe(&item(value), Duration::from_secs(3)));
    }
    let after = state.json(Duration::from_secs(3));
    assert_eq!(after["estimate"], before);
    assert!(!after.to_string().contains("private-sentinel"));
}

#[test]
fn unfinished_batches_cannot_be_replaced_and_zero_observation_tools_have_no_eta() {
    let mut state = active(true);
    observe(
        &mut state,
        &media(0, "packaging", "started", 1, 0, 1000),
        10,
    );
    assert!(!state.observe_nested(
        &media(0, "comparison", "started", 2, 0, 2000),
        Duration::from_secs(11)
    ));
    assert!(!state.observe_nested(
        &media(0, "packaging", "started", 1, 0, 1000),
        Duration::from_secs(11)
    ));
    step(&mut state, 0, "finalization", 11);
    assert_eq!(
        state.json(Duration::from_secs(11))["outputs"][0]["step"],
        "conversion"
    );
    observe(
        &mut state,
        &media(0, "packaging", "finished", 1, 0, 1000),
        12,
    );
    let value = state.json(Duration::from_secs(12));
    assert!(value["estimate"]["remaining_ms"].is_null());
    assert_eq!(value["outputs"][0]["media"]["completed_bytes"], 0);
    assert!(
        value["outputs"][0]["estimated_work_percent"]
            .as_u64()
            .unwrap()
            < 100
    );
    assert_eq!(value["checked_outputs"], 0);
}
