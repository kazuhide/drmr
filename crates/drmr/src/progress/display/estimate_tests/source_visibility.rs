//! Source visibility, ownership, and cadence regressions.
use super::*;

fn source_pipeline(estimates: Option<bool>) -> State {
    let mut pipeline = State::default();
    assert!(pipeline.observe(
        &record(serde_json::json!({"scope":"source", "state":"started"})),
        Duration::ZERO
    ));
    if let Some(estimates) = estimates {
        assert!(pipeline.observe(
            &record(
                serde_json::json!({"scope":"work-plan", "state":"ready", "planned_outputs":1,
                "estimates_enabled":estimates, "source_completed":0})
            ),
            Duration::ZERO
        ));
    }
    pipeline
}

fn observe_source_preparation(pipeline: &mut State, state: &str, done: u64, now: Duration) {
    let started = (done * 2).min(4);
    let terminal = matches!(state, "finished" | "aborted");
    let active = if terminal { 0 } else { started - done };
    let stopped = if state == "aborted" {
        started - done
    } else {
        0
    };
    assert!(pipeline.observe_nested(
        &record(serde_json::json!({
            "stage":"media-progress", "event":"progress", "task":"preparation",
            "subject":"source", "purpose":"conversion", "unit":"tracks", "state":state,
            "batch_id":1, "completed_units":done, "total_units":4, "estimated":false,
            "url":"private-sentinel"
        })),
        now
    ));
    assert!(pipeline.observe_nested(
        &record(serde_json::json!({
            "stage":"rendition-progress", "event":"progress", "operation":"preparation",
            "subject":"source", "purpose":"conversion", "unit":"tracks", "state":state,
            "batch_id":2, "total_units":4, "started_units":started,
            "active_units":active, "waiting_units":4 - started,
            "completed_units":done, "stopped_units":stopped
        })),
        now
    ));
}

fn preparing_source(estimates: Option<bool>) -> State {
    let mut pipeline = source_pipeline(estimates);
    for (state, done, second) in [("started", 0, 1), ("running", 1, 3), ("running", 2, 5)] {
        observe_source_preparation(&mut pipeline, state, done, Duration::from_secs(second));
    }
    pipeline
}

fn source_view(pipeline: &State) -> View<'_> {
    View {
        activity: "input-preparation",
        task: "",
        downloads: None,
        units: Some(Units {
            completed: 9,
            total: 9,
            initial: 0,
            began: Duration::ZERO,
            segments: true,
            checking: true,
        }),
        pipeline: Some(pipeline),
    }
}

#[test]
fn source_renditions_keep_active_media_counts_without_reusing_finished_preparation() {
    for estimates in [Some(true), Some(false), None] {
        let mut pipeline = preparing_source(estimates);
        let now = Duration::from_secs(5);
        let message = pipeline_message(&pipeline, source_view(&pipeline).units, now);
        assert!(message.contains("2 active 0 wait 2/4"), "{message}");
        assert!(message.contains("2/4 source tracks prepared"), "{message}");
        assert_eq!(
            message.contains("% overall work"),
            estimates == Some(true),
            "{message}"
        );
        assert!(
            !message.contains("9/9") && !message.contains("private-sentinel"),
            "{message}"
        );
        for columns in [40, 80, 120, 300] {
            let frame = dashboard::frame("convert", &source_view(&pipeline), now, columns);
            assert_eq!(frame.lines().count(), 3, "{frame}");
            assert!(
                frame.lines().all(|line| line.len() <= columns.min(120)),
                "{frame}"
            );
            let source = frame
                .lines()
                .find(|line| line.starts_with("Source"))
                .unwrap();
            assert!(
                source.contains("2 active") && source.contains("0 wait"),
                "{frame}"
            );
            if columns >= 120 {
                assert!(source.contains("2/4 tracks"), "{frame}");
            }
            assert!(
                !frame.contains("9/9") && !frame.contains("private-sentinel"),
                "{frame}"
            );
        }
        assert!(pipeline.observe_nested(
            &record(serde_json::json!({
                "stage":"media-progress", "event":"progress", "task":"preparation",
                "subject":"source", "purpose":"conversion", "unit":"tracks", "state":"finished",
                "batch_id":1, "completed_units":4, "total_units":4, "estimated":false
            })),
            Duration::from_secs(6)
        ));
        assert!(pipeline.observe_nested(
            &record(serde_json::json!({
                "stage":"rendition-progress", "event":"progress", "operation":"preparation",
                "subject":"source", "purpose":"conversion", "unit":"tracks", "state":"finished",
                "batch_id":2, "total_units":4, "started_units":4, "active_units":0,
                "waiting_units":0, "completed_units":4, "stopped_units":0
            })),
            Duration::from_secs(6)
        ));
        for (batch, operation) in [
            (3, "snapshot"),
            (4, "normalization"),
            (5, "source-comparison"),
        ] {
            for state in ["started", "running", "finished"] {
                let done = if state == "started" {
                    0
                } else if state == "running" {
                    1
                } else {
                    4
                };
                let active = u64::from(state == "running");
                let started = done + active;
                assert!(pipeline.observe_nested(&record(serde_json::json!({
                    "stage":"rendition-progress", "event":"progress", "operation":operation,
                    "subject":"source", "purpose":"conversion", "unit":"tracks", "state":state,
                    "batch_id":batch, "total_units":4, "started_units":started, "active_units":active,
                    "waiting_units":4 - started, "completed_units":done, "stopped_units":0
                })), Duration::from_secs(7)));
                let message = pipeline_message(
                    &pipeline,
                    source_view(&pipeline).units,
                    Duration::from_secs(7),
                );
                let frame = dashboard::frame(
                    "convert",
                    &source_view(&pipeline),
                    Duration::from_secs(7),
                    120,
                );
                if state == "running" {
                    assert!(message.contains("1 active 2 wait 1/4"), "{message}");
                }
                for text in [&message, &frame] {
                    assert!(!text.contains("4/4") && !text.contains("9/9"), "{text}");
                }
            }
        }
    }
}

fn render_plain_source(display: &mut Display, out: &mut Vec<u8>, pipeline: &State, now: Duration) {
    display
        .render(out, None, "convert", &source_view(pipeline), now, None)
        .unwrap();
}

#[test]
fn plain_source_renditions_appear_after_a_recent_heartbeat_without_burst_history() {
    for estimates in [Some(true), Some(false), None] {
        let mut pipeline = source_pipeline(estimates);
        let mut display = Display::default();
        let mut out = Vec::new();
        for second in [0, 16] {
            render_plain_source(
                &mut display,
                &mut out,
                &pipeline,
                Duration::from_secs(second),
            );
        }
        let before = out.len();
        let now = Duration::from_millis(16_100);
        observe_source_preparation(&mut pipeline, "started", 0, now);
        render_plain_source(&mut display, &mut out, &pipeline, now);
        assert_eq!(
            out.len(),
            before,
            "an unstarted batch consumed the first task snapshot"
        );
        let now = Duration::from_millis(16_200);
        assert!(pipeline.observe_nested(
            &record(serde_json::json!({
                "stage":"rendition-progress", "event":"progress", "operation":"preparation",
                "subject":"source", "purpose":"conversion", "unit":"tracks", "state":"running",
                "batch_id":2, "total_units":4, "started_units":1, "active_units":1,
                "waiting_units":3, "completed_units":0, "stopped_units":0
            })),
            now
        ));
        render_plain_source(&mut display, &mut out, &pipeline, now);
        for (state, done, millis) in [("running", 1, 17_000), ("running", 2, 18_000)] {
            let now = Duration::from_millis(millis);
            observe_source_preparation(&mut pipeline, state, done, now);
            render_plain_source(&mut display, &mut out, &pipeline, now);
        }
        let source = String::from_utf8_lossy(&out[before..]).into_owned();
        assert!(source.contains("1 active 3 wait 0/4"), "{source}");
        assert!(source.contains("0/4 source tracks prepared"), "{source}");
        assert_eq!(
            source.contains("% overall work"),
            estimates == Some(true),
            "{source}"
        );
        assert!(
            !source.contains("9/9") && !source.contains("private-sentinel"),
            "{source}"
        );
        let after_first_observation = out.len();
        for millis in 18_001..19_000 {
            render_plain_source(
                &mut display,
                &mut out,
                &pipeline,
                Duration::from_millis(millis),
            );
        }
        assert_eq!(
            out.len(),
            after_first_observation,
            "source bursts produced extra history"
        );
        let now = Duration::from_secs(19);
        observe_source_preparation(&mut pipeline, "finished", 4, now);
        for scope in ["source", "outputs"] {
            let state = if scope == "source" {
                "finished"
            } else {
                "started"
            };
            assert!(pipeline.observe(
                &record(serde_json::json!({"scope":scope,"state":state})),
                now
            ));
        }
        render_plain_source(&mut display, &mut out, &pipeline, now);
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.find("1 active 3 wait 0/4").unwrap() < text.find("Source checked").unwrap(),
            "{text}"
        );
    }
}

#[test]
fn plain_source_observations_keep_useful_and_idle_snapshot_intervals() {
    let mut pipeline = preparing_source(Some(true));
    let mut display = Display::default();
    let mut out = Vec::new();
    render_plain_source(&mut display, &mut out, &pipeline, Duration::from_secs(16));
    let first = out.len();
    observe_source_preparation(&mut pipeline, "running", 3, Duration::from_secs(17));
    for second in 17..21 {
        render_plain_source(
            &mut display,
            &mut out,
            &pipeline,
            Duration::from_secs(second),
        );
        assert_eq!(
            out.len(),
            first,
            "useful source snapshots printed within five seconds"
        );
    }
    render_plain_source(&mut display, &mut out, &pipeline, Duration::from_secs(21));
    assert!(
        out.len() > first,
        "new source observations were not printed after five seconds"
    );
    let update = String::from_utf8_lossy(&out[first..]);
    assert!(
        update.contains("1 active 0 wait 3/4") && update.contains("3/4 source tracks prepared"),
        "{update}"
    );
    let second = out.len();
    for second_now in 22..36 {
        render_plain_source(
            &mut display,
            &mut out,
            &pipeline,
            Duration::from_secs(second_now),
        );
        assert_eq!(
            out.len(),
            second,
            "elapsed or ETA changes produced extra snapshots"
        );
    }
    render_plain_source(&mut display, &mut out, &pipeline, Duration::from_secs(36));
    assert!(
        out.len() > second,
        "idle source heartbeat missing after fifteen seconds"
    );
}

#[test]
fn plain_terminal_source_observations_do_not_leak_into_later_phases() {
    for estimates in [Some(true), Some(false), None] {
        for terminal in ["finished", "aborted"] {
            let mut pipeline = preparing_source(estimates);
            let mut display = Display::default();
            let mut out = Vec::new();
            render_plain_source(&mut display, &mut out, &pipeline, Duration::from_secs(5));
            let now = Duration::from_secs(6);
            let done = if terminal == "finished" { 4 } else { 2 };
            observe_source_preparation(&mut pipeline, terminal, done, now);
            assert!(pipeline.observe(
                &record(serde_json::json!({"scope":"source", "state":terminal})),
                now
            ));
            let message = pipeline_message(&pipeline, source_view(&pipeline).units, now);
            assert!(
                !message.contains("prepared") && !message.contains("9/9"),
                "{message}"
            );
            let before = out.len();
            render_plain_source(&mut display, &mut out, &pipeline, now);
            for (scope, state, second) in [
                ("outputs", "started", 7),
                ("outputs", "finished", 8),
                ("final-check", "started", 8),
                ("final-check", "finished", 9),
                ("publication", "started", 9),
            ] {
                let now = Duration::from_secs(second);
                assert!(pipeline.observe(
                    &record(serde_json::json!({"scope":scope,"state":state})),
                    now
                ));
                render_plain_source(&mut display, &mut out, &pipeline, now);
            }
            display
                .render(
                    &mut out,
                    None,
                    "convert",
                    &source_view(&pipeline),
                    Duration::from_secs(10),
                    Some(("failed", Some("synthetic-failure"))),
                )
                .unwrap();
            let later = String::from_utf8_lossy(&out[before..]);
            let history = if terminal == "finished" {
                "Source checked"
            } else {
                "Checking source stopped"
            };
            assert_eq!(later.matches(history).count(), 1, "{later}");
            assert!(
                !later.contains("source tracks prepared") && !later.contains("9/9"),
                "{later}"
            );
            assert!(
                !later.contains("private-sentinel") && !later.contains("Finished convert"),
                "{later}"
            );
        }
    }
}

fn source_preparation_tail(estimates: Option<bool>, active: bool) -> State {
    let mut pipeline = preparing_source(estimates);
    observe_source_preparation(
        &mut pipeline,
        if active { "running" } else { "finished" },
        4,
        Duration::from_secs(6),
    );
    pipeline
}

fn source_task(operation: &str, state: &str, batch: u64) -> drmr_core::events::Record {
    let (started, active, done, stopped) = match state {
        "started" => (0, 0, 0, 0),
        "running" => (2, 1, 1, 0),
        "finished" => (4, 0, 4, 0),
        "aborted" => (2, 0, 1, 1),
        _ => panic!("unexpected source task state"),
    };
    record(serde_json::json!({
        "stage":"rendition-progress", "event":"progress", "operation":operation,
        "subject":"source", "purpose":"conversion", "unit":"tracks", "state":state,
        "batch_id":batch, "total_units":4, "started_units":started, "active_units":active,
        "waiting_units":4 - started, "completed_units":done, "stopped_units":stopped,
        "url":"private-sentinel"
    }))
}

#[test]
fn all_nonpreparation_source_states_block_prior_media_and_legacy_fallback() {
    for estimates in [Some(true), Some(false), None] {
        for active_media in [false, true] {
            for terminal in ["finished", "aborted"] {
                let mut pipeline = source_preparation_tail(estimates, active_media);
                for (batch, operation) in [
                    (3, "snapshot"),
                    (4, "normalization"),
                    (5, "source-comparison"),
                ] {
                    for state in ["started", "running", terminal] {
                        let now = Duration::from_secs(7);
                        assert!(
                            pipeline.observe_nested(&source_task(operation, state, batch), now)
                        );
                        let message =
                            pipeline_message(&pipeline, source_view(&pipeline).units, now);
                        assert!(
                            !message.contains("4/4") && !message.contains("9/9"),
                            "{message}"
                        );
                        assert!(!message.contains("private-sentinel"), "{message}");
                        for columns in [40, 80, 120, 300] {
                            let frame =
                                dashboard::frame("convert", &source_view(&pipeline), now, columns);
                            assert_eq!(frame.lines().count(), 3, "{frame}");
                            assert!(
                                frame
                                    .lines()
                                    .all(|line| !line.is_empty() && line.len() <= columns.min(120)),
                                "{frame}"
                            );
                            assert!(!frame.contains("4/4") && !frame.contains("9/9"), "{frame}");
                            assert!(!frame.contains("private-sentinel"), "{frame}");
                        }
                        let raw = pipeline.json(now);
                        assert_eq!(raw["source_media"]["completed_units"], 4);
                        assert_eq!(
                            raw["source_media"]["state"],
                            if active_media { "running" } else { "finished" }
                        );
                        assert_eq!(raw["source_renditions"]["operation"], operation);
                        assert_eq!(raw["source_renditions"]["state"], state);
                        assert!(!raw.to_string().contains("private-sentinel"));
                    }
                }
            }
        }
    }
}

#[test]
fn source_preparation_media_requires_a_compatible_live_rendition_context() {
    for (key, value) in [
        ("subject", serde_json::json!("result")),
        ("purpose", serde_json::json!("admission")),
        ("unit", serde_json::json!("renditions")),
        ("total_units", serde_json::json!(3)),
    ] {
        let mut pipeline = source_preparation_tail(None, true);
        let mut context = source_task("preparation", "started", 3);
        if key == "total_units" {
            context.insert("waiting_units".into(), value.clone());
        }
        context.insert(key.into(), value.clone());
        let now = Duration::from_secs(7);
        assert!(pipeline.observe_nested(&context, now));
        let message = pipeline_message(&pipeline, source_view(&pipeline).units, now);
        assert!(
            !message.contains("4/4") && !message.contains("9/9"),
            "{key}: {message}"
        );
        for columns in [40, 80, 120, 300] {
            let frame = dashboard::frame("convert", &source_view(&pipeline), now, columns);
            assert!(
                !frame.contains("4/4") && !frame.contains("9/9"),
                "{key}: {frame}"
            );
            assert!(
                frame.lines().all(|line| line.len() <= columns.min(120)),
                "{frame}"
            );
        }
        let raw = pipeline.json(now);
        assert_eq!(raw["source_media"]["state"], "running");
        assert_eq!(raw["source_media"]["completed_units"], 4);
        assert_eq!(raw["source_renditions"][key], value);
        assert!(!raw.to_string().contains("private-sentinel"));
    }
}
