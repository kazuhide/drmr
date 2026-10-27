use super::*;
use std::sync::mpsc;

struct Captured {
    bytes: Arc<Mutex<Vec<u8>>>,
    flushed: mpsc::Sender<()>,
}
impl Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let _ = self.flushed.send(());
        Ok(())
    }
}
fn records(bytes: &Arc<Mutex<Vec<u8>>>) -> Vec<serde_json::Value> {
    String::from_utf8(bytes.lock().unwrap().clone())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
fn observed(interval: Duration) -> (Progress, Arc<Mutex<Vec<u8>>>, mpsc::Receiver<()>) {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (sent, received) = mpsc::channel();
    let progress = Progress::with_writer(
        Mode::Json,
        "analyze",
        Box::new(Captured {
            bytes: bytes.clone(),
            flushed: sent,
        }),
        interval,
    );
    (progress, bytes, received)
}
#[test]
fn heartbeat_observes_idle_work_and_drop_joins_its_worker() {
    // Use the production throttle interval; receiving a flush bounds this test.
    let (progress, bytes, updates) = observed(Duration::from_millis(300));
    progress.mark("verification");
    updates.recv_timeout(Duration::from_secs(5)).unwrap();
    updates.recv_timeout(Duration::from_secs(5)).unwrap();
    let values = records(&bytes);
    assert!(
        values
            .iter()
            .any(|v| v["event"] == "heartbeat" && v["activity"] == "verification")
    );
    drop(progress);
    assert!(
        records(&bytes)
            .iter()
            .all(|record| record["event"] != "finished")
    );
    assert_eq!(
        Arc::strong_count(&bytes),
        1,
        "worker and writer have been dropped"
    );
}
#[test]
fn only_fixed_tool_labels_and_numeric_observations_reach_output() {
    let (progress, bytes, _) = observed(Duration::from_mins(1));
    progress.mark("verification");
    for value in [
        serde_json::json!({"stage":"tool","event":"command","tool":"ffmpeg","argv":["secret-sentinel"],"rendition_id":"private-sentinel"}),
        serde_json::json!({"stage":"tool","event":"tool-output","tool":"ffmpeg","stdout":"secret-sentinel","stderr":"secret-sentinel"}),
        serde_json::json!({"stage":"conversion","event":"activity","activity":"secret-sentinel","state":"started"}),
        serde_json::json!({"stage":"tool","event":"command","tool":"secret-sentinel"}),
        serde_json::json!({"stage":"acquisition","event":"progress","resources_started":3,"resources_completed":2,"bytes_received":1024,"url":"secret-sentinel"}),
    ] {
        progress.emit(value.as_object().unwrap());
    }
    progress.finish();
    let before = bytes.lock().unwrap().clone();
    progress.mark("publication");
    assert_eq!(*bytes.lock().unwrap(), before);
    let text = String::from_utf8(before).unwrap();
    assert!(!text.contains("sentinel") && !text.contains("argv") && !text.contains("stdout"));
    let values = records(&bytes);
    let last = values.last().unwrap();
    assert_eq!(last["resources_started"], 3);
    assert_eq!(last["resources_completed"], 2);
    assert_eq!(last["bytes_received"], 1024);
    assert_eq!(last["tools_started"], 1);
    assert_eq!(last["tools_closed"], 1);
    assert_eq!(last["active_tools"], serde_json::json!([]));
}

#[test]
fn a_result_write_failure_after_work_finishes_remains_structured() {
    let (progress, bytes, _) = observed(Duration::from_mins(1));
    progress.mark("publication");
    progress.finish();
    progress.diagnostic("analyze", "result-output-unavailable");
    drop(progress);
    let values = records(&bytes);
    assert_eq!(values.len(), 3);
    assert_eq!(values.last().unwrap()["event"], "failed");
    assert_eq!(
        values.last().unwrap()["error_code"],
        "result-output-unavailable"
    );
    assert_eq!(Arc::strong_count(&bytes), 1);
}

#[test]
fn automatic_progress_covers_interactive_json_and_preserves_redirected_json() {
    assert_eq!(resolved(Mode::Auto, false, true), Mode::Text);
    assert_eq!(resolved(Mode::Auto, false, false), Mode::Off);
    for terminal in [false, true] {
        assert_eq!(resolved(Mode::Auto, true, terminal), Mode::Text);
        for mode in [Mode::Text, Mode::Json, Mode::Off] {
            assert_eq!(resolved(mode, false, terminal), mode);
            assert_eq!(resolved(mode, true, terminal), mode);
        }
    }
}

#[test]
fn numeric_bundle_progress_reaches_json_and_result_failure_removes_its_estimate() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (sent, _received) = mpsc::channel();
    let progress = Progress::with_writer(
        Mode::Json,
        "convert",
        Box::new(Captured {
            bytes: bytes.clone(),
            flushed: sent,
        }),
        Duration::from_mins(1),
    );
    let emit = |value: serde_json::Value| progress.emit(value.as_object().unwrap());
    emit(
        serde_json::json!({"stage":"pipeline","event":"progress","scope":"source","state":"started"}),
    );
    emit(
        serde_json::json!({"stage":"pipeline","event":"progress","scope":"work-plan","state":"ready","planned_outputs":1,"estimates_enabled":true,"source_completed":2,"url":"private-sentinel"}),
    );
    emit(
        serde_json::json!({"stage":"pipeline","event":"progress","scope":"source","state":"finished"}),
    );
    emit(
        serde_json::json!({"stage":"pipeline","event":"progress","scope":"outputs","state":"started"}),
    );
    emit(
        serde_json::json!({"stage":"pipeline","event":"progress","scope":"output","state":"started","output_index":0,"output_total":1,"output_format":"dash"}),
    );
    emit(
        serde_json::json!({"stage":"pipeline","event":"progress","scope":"output-step","state":"started","step":"conversion","output_index":0,"output_total":1,"output_format":"dash"}),
    );
    for (state, done) in [("started", 0), ("running", 300), ("running", 600)] {
        emit(
            serde_json::json!({"stage":"media-progress","event":"progress","task":"packaging","state":state,"batch_id":1,"completed_bytes":done,"total_bytes":1000,"estimated":true,"output_index":0,"output_total":1,"output_format":"dash","path":"private-sentinel"}),
        );
    }
    progress.mark("verification");
    let values = records(&bytes);
    assert_eq!(
        values.last().unwrap()["pipeline"]["outputs"][0]["media"]["completed_bytes"],
        600
    );
    assert_eq!(
        values.last().unwrap()["pipeline"]["outputs"][0]["step"],
        "conversion"
    );
    assert!(
        values.last().unwrap()["pipeline"]["estimate"]["percent"]
            .as_u64()
            .unwrap()
            < 100
    );
    emit(
        serde_json::json!({"stage":"pipeline","event":"progress","scope":"output","state":"finished","output_index":0,"output_total":1,"output_format":"dash"}),
    );
    for (scope, state) in [
        ("outputs", "finished"),
        ("final-check", "started"),
        ("publication", "started"),
        ("final-check", "finished"),
        ("publication", "finished"),
    ] {
        emit(
            serde_json::json!({"stage":"pipeline","event":"progress","scope":scope,"state":state}),
        );
    }
    assert_eq!(
        records(&bytes).last().unwrap()["pipeline"]["estimate"]["percent"],
        100
    );
    progress.begin_result();
    progress.fail("result-output-unavailable");
    let values = records(&bytes);
    let last = values.last().unwrap();
    assert_eq!(last["event"], "failed");
    assert!(last["pipeline"].get("estimate").is_none());
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(!text.contains("private-sentinel"));
    assert!(!values.iter().any(|value| value["event"] == "finished"));
}

#[test]
fn repeated_short_tools_do_not_flood_plain_progress() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (sent, _received) = mpsc::channel();
    let progress = Progress::with_writer(
        Mode::Text,
        "analyze",
        Box::new(Captured {
            bytes: bytes.clone(),
            flushed: sent,
        }),
        Duration::from_mins(1),
    );
    progress.mark("inspection");
    for _ in 0..100 {
        for event in ["command", "tool-output"] {
            progress.emit(
                serde_json::json!({
                    "stage":"tool", "event":event, "tool":"ffprobe",
                    "argv":["secret-sentinel"], "stderr":"secret-sentinel"
                })
                .as_object()
                .unwrap(),
            );
        }
    }
    progress.finish();
    let output = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(output.lines().count() <= 4, "{output}");
    assert!(!output.contains("scopes") && !output.contains("sentinel"));
    assert!(!output.contains('\u{1b}') && !output.contains('\r'));
}

#[test]
fn analysis_progress_accepts_only_valid_monotonic_numeric_evidence() {
    let (progress, bytes, _) = observed(Duration::from_mins(1));
    progress.mark("input-preparation");
    for value in [
        serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"started","completed":0,"total":5}),
        serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"running","completed":3,"total":5,"url":"secret-sentinel"}),
        serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"running","completed":2,"total":5}),
        serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"finished","completed":4,"total":5}),
        serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"running","completed":4,"total":6}),
        serde_json::json!({"stage":"analysis","event":"progress","task":"secret-sentinel","state":"running","completed":4,"total":5}),
        serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":"secret-sentinel","completed":4,"total":5}),
    ] {
        progress.emit(value.as_object().unwrap());
    }
    progress.finish();
    let values = records(&bytes);
    let last = values.last().unwrap();
    assert_eq!(last["analysis_task"], "ts-preparation");
    assert_eq!(last["analysis_state"], "running");
    assert_eq!(last["analysis_completed"], 3);
    assert_eq!(last["analysis_total"], 5);
    assert!(
        !String::from_utf8(bytes.lock().unwrap().clone())
            .unwrap()
            .contains("sentinel")
    );
}

#[test]
fn finished_ts_collection_yields_to_the_remaining_source_checks() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (sent, _) = mpsc::channel();
    let progress = Progress::with_writer(
        Mode::Text,
        "analyze",
        Box::new(Captured {
            bytes: bytes.clone(),
            flushed: sent,
        }),
        Duration::from_mins(1),
    );
    for (state, completed) in [("started", 0), ("running", 5), ("finished", 5)] {
        progress.emit(
            serde_json::json!({
                "stage":"analysis", "event":"progress", "task":"ts-preparation",
                "state":state, "completed":completed, "total":5
            })
            .as_object()
            .unwrap(),
        );
    }
    progress.emit(
        serde_json::json!({
            "stage":"tool", "event":"command", "tool":"ffprobe"
        })
        .as_object()
        .unwrap(),
    );
    progress.finish();
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(text.contains("5/5 segments"), "{text}");
    assert!(text.contains("Analyzing source"), "{text}");
    let last = text.lines().rev().nth(1).unwrap();
    assert!(!last.contains("segments"), "{text}");
}

#[test]
fn phase_history_retains_completion_received_inside_the_observer_interval() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (sent, _) = mpsc::channel();
    let progress = Progress::with_writer(
        Mode::Text,
        "analyze",
        Box::new(Captured {
            bytes: bytes.clone(),
            flushed: sent,
        }),
        Duration::from_mins(1),
    );
    for completed in [1, 6] {
        progress.emit(
            serde_json::json!({
                "stage":"acquisition", "event":"progress", "resources_started":6,
                "resources_completed":completed, "resources_total":6, "bytes_received":completed*100
            })
            .as_object()
            .unwrap(),
        );
    }
    progress.mark("input-preparation");
    progress.finish();
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(text.contains("6/6 files"), "{text}");
}

#[test]
fn acquisition_keeps_one_label_while_its_internal_preparation_scope_is_open() {
    let (progress, bytes, _) = observed(Duration::from_mins(1));
    progress.mark("acquisition");
    for state in ["started", "closed"] {
        progress.emit(serde_json::json!({
            "stage":"conversion", "event":"activity", "activity":"input-preparation", "state":state
        }).as_object().unwrap());
    }
    progress.finish();
    let values = records(&bytes);
    let last = values.last().unwrap();
    assert_eq!(last["activity"], "acquisition");
    assert_eq!(last["scopes_started"], 1);
    assert_eq!(last["scopes_closed"], 1);
}

#[test]
fn parallel_output_scopes_do_not_become_completed_command_steps() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (sent, _) = mpsc::channel();
    let progress = Progress::with_writer(
        Mode::Text,
        "convert",
        Box::new(Captured {
            bytes: bytes.clone(),
            flushed: sent,
        }),
        Duration::from_mins(1),
    );
    let emit = |value: serde_json::Value| progress.emit(value.as_object().unwrap());
    for (scope, state) in [
        ("source", "started"),
        ("source", "finished"),
        ("outputs", "started"),
    ] {
        emit(
            serde_json::json!({"stage":"pipeline","event":"progress","scope":scope,"state":state,"output_total":2}),
        );
    }
    for (index, format) in [(0, "hls"), (1, "dash")] {
        emit(
            serde_json::json!({"stage":"pipeline","event":"progress","scope":"output","state":"started","output_index":index,"output_total":2,"output_format":format,"output_id":"private-sentinel"}),
        );
    }
    emit(
        serde_json::json!({"stage":"tool","event":"command","tool":"packager","argv":["private-sentinel"]}),
    );
    for _ in 0..20 {
        for (index, format, activity) in [(0, "hls", "inspection"), (1, "dash", "verification")] {
            for state in ["started", "closed"] {
                emit(
                    serde_json::json!({"stage":"conversion","event":"activity","activity":activity,"state":state,"output_index":index,"output_total":2,"output_format":format}),
                );
            }
        }
    }
    for (index, format) in [(1, "dash"), (0, "hls")] {
        emit(
            serde_json::json!({"stage":"pipeline","event":"progress","scope":"output","state":"finished","output_index":index,"output_total":2,"output_format":format}),
        );
    }
    emit(serde_json::json!({"stage":"tool","event":"tool-output","tool":"packager"}));
    for (scope, state) in [
        ("outputs", "finished"),
        ("final-check", "started"),
        ("final-check", "finished"),
        ("publication", "started"),
        ("publication", "finished"),
    ] {
        emit(
            serde_json::json!({"stage":"pipeline","event":"progress","scope":scope,"state":state,"output_total":2}),
        );
    }
    progress.finish();
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(text.lines().count() <= 12, "{text}");
    assert!(
        text.contains("Output 1") && text.contains("Output 2"),
        "{text}"
    );
    assert!(
        text.contains("HLS fMP4") && text.contains("DASH fMP4"),
        "{text}"
    );
    assert!(
        !text.contains("packager") && !text.contains("sentinel"),
        "{text}"
    );
}

#[test]
fn dropping_an_unfinished_observer_does_not_create_success() {
    let (progress, bytes, _) = observed(Duration::from_mins(1));
    progress.mark("inspection");
    drop(progress);
    assert!(
        records(&bytes)
            .iter()
            .all(|record| record["event"] != "finished")
    );
    assert_eq!(Arc::strong_count(&bytes), 1);
}

#[test]
fn retained_source_notice_uses_the_progress_writer_once_in_text_mode_only() {
    for mode in [Mode::Text, Mode::Json, Mode::Off] {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let (sent, _) = mpsc::channel();
        let progress = Progress::with_writer(
            mode,
            "convert",
            Box::new(Captured {
                bytes: bytes.clone(),
                flushed: sent,
            }),
            Duration::from_mins(1),
        );
        progress.source_retained();
        progress.source_retained();
        drop(progress);
        let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
        if mode == Mode::Text {
            assert_eq!(text, "Downloaded source retained for offline use.\n");
        } else {
            assert!(text.is_empty(), "{mode:?}: {text}");
        }
        assert_eq!(Arc::strong_count(&bytes), 1);
    }
}

#[test]
fn structured_pipeline_keeps_existing_fields_and_only_fixed_bounded_output_context() {
    let (progress, bytes, _) = observed(Duration::from_mins(1));
    progress.mark("input-preparation");
    let emit = |value: serde_json::Value| progress.emit(value.as_object().unwrap());
    emit(
        serde_json::json!({"stage":"pipeline","event":"progress","scope":"outputs","state":"started"}),
    );
    for index in 0..8 {
        emit(serde_json::json!({
            "stage":"pipeline","event":"progress","scope":"output","state":"started",
            "output_index":index,"output_total":8,"output_format":"dash",
            "output_id":"private-sentinel", "url":"https://private-sentinel", "key":"private-sentinel"
        }));
    }
    emit(
        serde_json::json!({"stage":"conversion","event":"activity","activity":"verification","state":"started","output_index":3,"output_total":8,"output_format":"dash"}),
    );
    emit(
        serde_json::json!({"stage":"pipeline","event":"progress","scope":"output","state":"started","output_index":8,"output_total":9,"output_format":"mp4"}),
    );
    progress.fail("source-invalid");
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(
        !text.contains("sentinel")
            && !text.contains("output_id")
            && !text.contains("\"url\"")
            && !text.contains("\"key\""),
        "{text}"
    );
    let values = records(&bytes);
    let last = values.last().unwrap();
    assert_eq!(last["schema_version"], 1);
    for field in [
        "activity",
        "task",
        "scopes_started",
        "scopes_closed",
        "active_scopes",
        "tools_started",
        "tools_closed",
        "active_tool_count",
        "active_tools",
        "work_units_completed",
    ] {
        assert!(last.get(field).is_some(), "missing {field}: {last}");
    }
    assert_eq!(last["event"], "failed");
    let pipeline = &last["pipeline"];
    assert_eq!(pipeline["active_outputs"], 8);
    assert_eq!(pipeline["checked_outputs"], 0);
    let outputs = pipeline["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 8);
    assert_eq!(outputs[3]["index"], 3);
    assert_eq!(outputs[3]["format"], "dash");
    assert_eq!(outputs[3]["activity"], "verification");
}

#[test]
fn live_fallback_details_are_active_scopes_and_tasks_not_the_last_closed_label() {
    let mut state = State::default();
    let activity = |activity, status| serde_json::json!({"activity":activity,"state":status});
    for value in [
        activity("inspection", "started"),
        activity("packaging", "started"),
        activity("packaging", "closed"),
    ] {
        assert!(state.observe_activity(value.as_object().unwrap()).is_some());
    }
    assert_eq!(state.last_activity, Some("packaging"));
    assert_eq!(state.live_task(), "Reading metadata");
    state.tasks[0] = 1;
    assert_eq!(
        state.live_task(),
        "Reading complete packet timestamps and durations"
    );
    state.tasks[0] = 0;
    state
        .observe_activity(activity("inspection", "closed").as_object().unwrap())
        .unwrap();
    assert_eq!(state.live_task(), "");
}
