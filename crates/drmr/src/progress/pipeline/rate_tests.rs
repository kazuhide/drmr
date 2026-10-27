//! Progress follows each actual preparation/comparison cycle, rather than a high-water mark.
use super::estimate_tests::{active, media, observe, output_record, phase, step};
use super::*;

fn preparation(
    index: usize,
    subject: &str,
    purpose: &str,
    status: &str,
    id: u64,
    done: u64,
) -> Record {
    let mut record = output_record(index, "unused", status);
    record.remove("scope");
    record.extend(serde_json::from_value::<Record>(serde_json::json!({
        "stage":"media-progress","event":"progress","task":"preparation",
        "subject":subject,"purpose":purpose,"unit":"tracks","state":status,"batch_id":id,"estimated":false,
        "completed_units":done,"total_units":4,"private":"private-sentinel"
    })).unwrap());
    record
}

fn prepared(
    state: &mut State,
    index: usize,
    subject: &str,
    purpose: &str,
    id: u64,
    began: u64,
    end: u64,
) {
    observe(
        state,
        &preparation(index, subject, purpose, "started", id, 0),
        began,
    );
    observe(
        state,
        &preparation(index, subject, purpose, "running", id, 2),
        began + (end - began) / 2,
    );
    observe(
        state,
        &preparation(index, subject, purpose, "finished", id, 4),
        end,
    );
}

fn compared(state: &mut State, index: usize, id: u64, began: u64, end: u64) {
    observe(
        state,
        &media(index, "comparison", "started", id, 0, 2000),
        began,
    );
    observe(
        state,
        &media(index, "comparison", "running", id, 1000, 2000),
        began + (end - began) / 2,
    );
    observe(
        state,
        &media(index, "comparison", "finished", id, 2000, 2000),
        end,
    );
}

#[test]
fn repeated_preparation_and_comparison_cycles_each_advance_planned_work() {
    let mut state = active(true);
    prepared(&mut state, 0, "output", "conversion", 1, 10, 12);
    observe(
        &mut state,
        &media(0, "packaging", "started", 2, 0, 1000),
        12,
    );
    observe(
        &mut state,
        &media(0, "packaging", "finished", 2, 1000, 1000),
        14,
    );
    prepared(&mut state, 0, "result", "conversion", 3, 14, 24);
    compared(&mut state, 0, 4, 24, 26);
    let first = state.json(Duration::from_secs(26))["outputs"][0]["estimated_work_percent"]
        .as_u64()
        .unwrap();
    prepared(&mut state, 0, "result", "conversion", 5, 26, 36);
    compared(&mut state, 0, 6, 36, 38);
    let second = state.json(Duration::from_secs(38))["outputs"][0]["estimated_work_percent"]
        .as_u64()
        .unwrap();
    assert!(
        second > first,
        "a second real preparation/comparison must advance work: {first} -> {second}"
    );
    step(&mut state, 0, "verification", 38);
    prepared(&mut state, 0, "result", "admission", 7, 39, 44);
    prepared(&mut state, 0, "source", "verification", 8, 44, 48);
    prepared(&mut state, 0, "result", "verification", 9, 48, 58);
    compared(&mut state, 0, 10, 58, 60);
    let verified = state.json(Duration::from_mins(1));
    assert!(
        verified["outputs"][0]["estimated_work_percent"]
            .as_u64()
            .unwrap()
            > second
    );
    assert!(verified["estimate"]["percent"].as_u64().unwrap() < 100);
}

#[test]
fn preparation_reports_bounded_units_without_inventing_byte_counts() {
    let mut state = active(true);
    assert!(state.observe_nested(
        &preparation(0, "output", "conversion", "started", 1, 0),
        Duration::from_secs(10)
    ));
    assert!(state.observe_nested(
        &preparation(0, "output", "conversion", "running", 1, 2),
        Duration::from_secs(11)
    ));
    let before = state.json(Duration::from_secs(11));
    let current = &before["outputs"][0]["media"];
    assert_eq!(current["task"], "preparation");
    assert_eq!(current["subject"], "output");
    assert_eq!(current["completed_units"], 2);
    assert!(current.get("completed_bytes").is_none());
    assert!(current.get("total_bytes").is_none());
    for mut bad in [
        preparation(0, "output", "private-sentinel", "running", 1, 3),
        preparation(0, "private-sentinel", "conversion", "running", 1, 3),
        preparation(0, "output", "conversion", "running", 1, 1),
    ] {
        bad.insert("path".into(), "private-sentinel".into());
        assert!(!state.observe_nested(&bad, Duration::from_secs(12)));
    }
    assert_eq!(
        state.json(Duration::from_secs(12))["outputs"][0]["media"],
        *current
    );
    assert!(!before.to_string().contains("private-sentinel"));
}

#[test]
fn uncalibrated_preparation_never_reuses_download_or_packaging_speed_for_eta() {
    let mut state = active(true);
    observe(
        &mut state,
        &media(0, "packaging", "started", 1, 0, 1000),
        10,
    );
    observe(
        &mut state,
        &media(0, "packaging", "running", 1, 100, 1000),
        11,
    );
    observe(
        &mut state,
        &media(0, "packaging", "running", 1, 900, 1000),
        12,
    );
    let value = state.json(Duration::from_secs(12));
    assert!(
        value["estimate"]["remaining_ms"].is_null(),
        "unseen preparation cannot inherit packaging throughput"
    );
    assert_eq!(value["estimate"]["eta_basis"], "observed-step-rates");
    assert!(value["estimate"]["percent"].as_u64().unwrap() < 100);
    phase(&mut state, "outputs", "aborted", 13);
    assert!(state.json(Duration::from_secs(14))["estimate"]["remaining_ms"].is_null());
}

fn integrity_record(status: &str, id: u64, done: u64) -> Record {
    serde_json::from_value(serde_json::json!({"stage":"media-progress","event":"progress","task":"integrity","state":status,"batch_id":id,"completed_bytes":done*250,"total_bytes":1000,"completed_units":done,"total_units":4,"unit":"files","estimated":false,"path":"private-sentinel"})).unwrap()
}
fn calibrated_integrity(state: &mut State) {
    for (id, offset) in [(99, 0), (100, 2)] {
        for (status, done, second) in [("started", 0, 0), ("running", 2, 1), ("finished", 4, 2)] {
            assert!(state.observe_media(
                &integrity_record(status, id, done),
                Duration::from_secs(offset + second)
            ));
        }
    }
}

fn converted_pair() -> State {
    let mut state = active(true);
    calibrated_integrity(&mut state);
    for index in 0..2 {
        prepared(&mut state, index, "output", "conversion", 1, 10, 12);
    }
    for index in 0..2 {
        observe(
            &mut state,
            &media(index, "packaging", "started", 2, 0, 1000),
            12,
        );
        observe(
            &mut state,
            &media(index, "packaging", "finished", 2, 1000, 1000),
            14,
        );
        prepared(&mut state, index, "result", "conversion", 3, 14, 24);
        compared(&mut state, index, 4, 24, 26);
        prepared(&mut state, index, "result", "conversion", 5, 26, 28);
        compared(&mut state, index, 6, 28, 30);
        step(&mut state, index, "verification", 30);
        prepared(&mut state, index, "result", "admission", 7, 31, 35);
        prepared(&mut state, index, "source", "verification", 8, 35, 41);
    }
    for index in 0..2 {
        observe(
            &mut state,
            &preparation(index, "result", "verification", "started", 9, 0),
            41,
        );
        observe(
            &mut state,
            &preparation(index, "result", "verification", "running", 9, 1),
            43,
        );
        observe(
            &mut state,
            &preparation(index, "result", "verification", "running", 9, 2),
            46,
        );
    }
    state
}

#[test]
fn nonuniform_observed_rates_use_the_parallel_critical_path_and_slow_down_on_stalls() {
    let state = converted_pair();
    let value = state.json(Duration::from_secs(46));
    // Five seconds of result preparation, two comparison, two child integrity,
    // and six final guard seconds remain. Signaling reuses the verified output binding.
    assert_eq!(value["estimate"]["remaining_ms"], 15_000);
    let stalled = state.json(Duration::from_secs(47));
    assert_eq!(stalled["estimate"]["remaining_ms"], 16_000);
    assert_eq!(stalled["estimate"]["percent"], value["estimate"]["percent"]);
    assert_eq!(value["estimate"]["eta_basis"], "observed-step-rates");
    assert!(!value.to_string().contains("private-sentinel"));
    assert!(state.json(Duration::MAX)["estimate"]["remaining_ms"].is_null());
}

#[test]
fn completed_preparation_units_are_still_checks_until_the_explicit_batch_finish() {
    let mut state = converted_pair();
    observe(
        &mut state,
        &preparation(0, "result", "verification", "running", 9, 4),
        49,
    );
    let value = state.json(Duration::from_secs(50));
    assert_eq!(state.outputs()[0].detail(), "Checking prepared track data");
    assert_eq!(value["outputs"][0]["media"]["state"], "running");
    assert!(
        value["estimate"]["remaining_ms"].is_null(),
        "a first unmeasured admission tail must not pretend it has zero remaining cost"
    );
    let percent = value["outputs"][0]["estimated_work_percent"]
        .as_u64()
        .unwrap();
    observe(
        &mut state,
        &preparation(0, "result", "verification", "finished", 9, 4),
        51,
    );
    let finished = state.json(Duration::from_secs(51));
    assert!(
        finished["outputs"][0]["estimated_work_percent"]
            .as_u64()
            .unwrap()
            >= percent
    );
    assert_eq!(state.checked_outputs(), 0);
    assert!(finished["estimate"]["percent"].as_u64().unwrap() < 100);
}

#[test]
fn overlapping_integrity_batches_do_not_replace_preparation_and_overflow_disables_eta() {
    let mut state = converted_pair();
    for id in 101..117 {
        assert!(state.observe_media(&integrity_record("started", id, 0), Duration::from_secs(46)));
    }
    let before = state.json(Duration::from_secs(46));
    assert!(before["estimate"]["remaining_ms"].as_u64().is_some());
    assert!(state.observe_media(
        &integrity_record("started", 117, 0),
        Duration::from_secs(46)
    ));
    let after = state.json(Duration::from_secs(47));
    assert!(after["estimate"]["remaining_ms"].is_null());
    assert_eq!(before["outputs"][0]["media"], after["outputs"][0]["media"]);
    for id in 101..117 {
        assert!(state.observe_media(
            &integrity_record("finished", id, 4),
            Duration::from_secs(48)
        ));
    }
    assert!(!state.observe_media(
        &integrity_record("finished", 101, 4),
        Duration::from_secs(49)
    ));
    assert!(state.json(Duration::from_secs(49))["estimate"]["remaining_ms"].is_null());
}

#[test]
fn unscoped_source_preparation_is_bounded_and_never_uses_a_verification_role() {
    let mut state = State::default();
    phase(&mut state, "source", "started", 0);
    super::estimate_tests::plan(&mut state, true);
    let mut started = preparation(0, "source", "conversion", "started", 1, 0);
    for key in ["output_index", "output_total", "output_format"] {
        started.remove(key);
    }
    assert!(state.observe_media(&started, Duration::from_secs(1)));
    let before = state.json(Duration::from_secs(1));
    let mut running = started.clone();
    running.insert("state".into(), "running".into());
    running.insert("completed_units".into(), 2.into());
    assert!(state.observe_media(&running, Duration::from_secs(3)));
    let after = state.json(Duration::from_secs(3));
    assert!(
        after["estimate"]["percent"].as_u64().unwrap()
            > before["estimate"]["percent"].as_u64().unwrap()
    );
    assert_eq!(after["source_media"]["completed_units"], 2);
    running.insert("purpose".into(), "verification".into());
    assert!(!state.observe_media(&running, Duration::from_secs(4)));
    assert!(after["estimate"]["remaining_ms"].is_null());
    assert!(!after.to_string().contains("private-sentinel"));
}

#[test]
fn concurrent_integrity_start_delivery_does_not_assume_numeric_id_order() {
    let mut state = converted_pair();
    assert!(state.observe_media(
        &integrity_record("started", 102, 0),
        Duration::from_secs(46)
    ));
    assert!(
        state.observe_media(
            &integrity_record("started", 101, 0),
            Duration::from_secs(46)
        ),
        "a valid concurrent batch may allocate first and emit second"
    );
    for id in [101, 102] {
        assert!(state.observe_media(&integrity_record("running", id, 2), Duration::from_secs(47)));
        assert!(state.observe_media(
            &integrity_record("finished", id, 4),
            Duration::from_secs(48)
        ));
        assert!(
            !state.observe_media(&integrity_record("started", id, 0), Duration::from_secs(49)),
            "recent completed batches cannot be replayed"
        );
    }
    let value = state.json(Duration::from_secs(48));
    assert_eq!(value["outputs"][0]["media"]["task"], "preparation");
    assert!(value["estimate"]["remaining_ms"].as_u64().is_some());
}

#[test]
fn integrity_sensor_exposes_only_bounded_fixed_observations_and_calibration() {
    let mut state = converted_pair();
    for id in 101..117 {
        assert!(state.observe_media(&integrity_record("started", id, 0), Duration::from_secs(46)));
    }
    let value = state.json(Duration::from_secs(47));
    let sensor = &value["integrity_batches"];
    assert_eq!(sensor["active"].as_array().unwrap().len(), 16);
    assert_eq!(sensor["completed_batches"], 2);
    assert_eq!(sensor["reference_bytes"], 1000);
    assert_eq!(sensor["observed_bytes"], 2000);
    assert_eq!(sensor["observed_batch_ms"], 4000);
    assert_eq!(sensor["calibrated"], true);
    assert_eq!(sensor["overflow"], false);
    assert_eq!(sensor["last_terminal"]["state"], "finished");
    assert_eq!(sensor["last_terminal"]["elapsed_ms"], 2000);
    assert_eq!(sensor["active"][0]["elapsed_ms"], 1000);
    assert!(!value.to_string().contains("private-sentinel"));
}

#[test]
fn completed_integrity_replay_memory_is_bounded_without_forgetting_live_batches() {
    let mut state = converted_pair();
    for id in 101..354 {
        assert!(state.observe_media(&integrity_record("started", id, 0), Duration::from_secs(46)));
        assert!(state.observe_media(
            &integrity_record("finished", id, 4),
            Duration::from_secs(48)
        ));
    }
    for id in 1000..1016 {
        assert!(state.observe_media(&integrity_record("started", id, 0), Duration::from_secs(48)));
    }
    for id in 1000..1016 {
        assert!(state.observe_media(
            &integrity_record("finished", id, 4),
            Duration::from_secs(50)
        ));
    }
    let value = state.json(Duration::from_secs(51));
    assert_eq!(
        value["integrity_batches"]["active"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(value["integrity_batches"]["completed_batches"], 271);
    assert_eq!(value["integrity_batches"]["overflow"], true);
    assert_eq!(value["integrity_batches"]["calibrated"], false);
    assert!(value["estimate"]["remaining_ms"].is_null());
    assert!(!state.observe_media(
        &integrity_record("started", 1020, 0),
        Duration::from_secs(51)
    ));
    assert!(!state.observe_media(
        &integrity_record("started", 100, 0),
        Duration::from_secs(51)
    ));
}

fn hash_interval(
    sensor: &mut super::integrity::Integrity,
    id: u64,
    began: Duration,
    ended: Duration,
) {
    assert!(sensor.observe(&integrity_record("started", id, 0), began));
    assert!(sensor.observe(&integrity_record("finished", id, 4), ended));
}

#[test]
fn integrity_short_hash_batches_need_two_completed_observations() {
    let mut sensor = super::integrity::Integrity::default();
    hash_interval(&mut sensor, 1, Duration::ZERO, Duration::from_millis(17));
    assert!(sensor.rates().duration(work::Class::Integrity).is_none());
    assert_eq!(
        sensor.json(Duration::from_millis(17)).unwrap()["calibrated"],
        false
    );
    hash_interval(
        &mut sensor,
        2,
        Duration::from_millis(20),
        Duration::from_millis(43),
    );
    assert_eq!(
        sensor.rates().duration(work::Class::Integrity),
        Some(Duration::from_millis(20))
    );
    let value = sensor.json(Duration::from_millis(43)).unwrap();
    assert_eq!(value["completed_batches"], 2);
    assert_eq!(value["observed_batch_ms"], 40);
    assert_eq!(value["calibrated"], true);
}

#[test]
fn integrity_single_or_zero_duration_batches_do_not_calibrate() {
    let mut single = super::integrity::Integrity::default();
    hash_interval(&mut single, 1, Duration::ZERO, Duration::from_secs(2));
    assert!(
        single.rates().duration(work::Class::Integrity).is_none(),
        "one completed hash is insufficient even if it took more than one second"
    );
    let mut zero = super::integrity::Integrity::default();
    for id in [1, 2] {
        hash_interval(&mut zero, id, Duration::ZERO, Duration::ZERO);
    }
    assert!(zero.rates().duration(work::Class::Integrity).is_none());
    assert_eq!(zero.json(Duration::ZERO).unwrap()["calibrated"], false);
}

fn finalizing_pair() -> State {
    let mut state = converted_pair();
    for index in 0..2 {
        observe(
            &mut state,
            &preparation(index, "result", "verification", "finished", 9, 4),
            51,
        );
        compared(&mut state, index, 10, 51, 53);
        step(&mut state, index, "finalization", 53);
    }
    state
}

#[test]
fn finalization_reuses_bound_signaling_without_charging_another_capture() {
    let state = finalizing_pair();
    let mut rates = work::Rates::default();
    rates.sample(
        work::Class::Capture,
        Duration::from_secs(5),
        Duration::from_secs(53),
    );
    rates.sample(
        work::Class::Integrity,
        Duration::from_secs(1),
        Duration::from_secs(53),
    );
    for output in state.outputs() {
        assert_eq!(
            output.work.remaining(&rates, Duration::from_secs(53)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(output.work.remaining(&rates, Duration::from_secs(54)), None);
    }
    let mut guard_only = work::Rates::default();
    guard_only.sample(
        work::Class::Integrity,
        Duration::from_secs(1),
        Duration::from_secs(53),
    );
    assert_eq!(
        state.outputs()[0]
            .work
            .remaining(&guard_only, Duration::from_secs(53)),
        Some(Duration::from_secs(1))
    );
    assert_eq!(state.checked_outputs(), 0);
}

#[test]
fn finalization_uses_parallel_remaining_paths_then_the_distinct_quiet_guards() {
    let mut state = finalizing_pair();
    // Signaling reuses each output binding; only its two-second integrity pass remains.
    // Two concurrent two-second paths precede three sequential two-second guards.
    let running = state.json(Duration::from_secs(53));
    assert_eq!(running["estimate"]["remaining_ms"], 8000);
    assert!(running["estimate"]["percent"].as_u64().unwrap() < 100);
    for index in 0..2 {
        assert!(state.observe(
            &output_record(index, "output", "finished"),
            Duration::from_secs(54)
        ));
    }
    let checked = state.json(Duration::from_secs(54));
    assert_eq!(checked["estimate"]["remaining_ms"], 6000);
    assert!(checked["estimate"]["percent"].as_u64().unwrap() < 100);
}
