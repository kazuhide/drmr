use super::*;
use crate::progress::pipeline::State;

fn record(value: serde_json::Value) -> drmr_core::events::Record {
    let serde_json::Value::Object(record) = value else {
        panic!("expected a progress event object");
    };
    record
}

fn started() -> State {
    let mut pipeline = State::default();
    for (value, second) in [
        (serde_json::json!({"scope":"source","state":"started"}), 0),
        (
            serde_json::json!({"scope":"work-plan","state":"ready","planned_outputs":2,"estimates_enabled":true,"source_completed":2}),
            1,
        ),
        (serde_json::json!({"scope":"source","state":"finished"}), 10),
        (serde_json::json!({"scope":"outputs","state":"started"}), 10),
    ] {
        assert!(pipeline.observe(&record(value), Duration::from_secs(second)));
    }
    for (index, format) in [(0, "dash"), (1, "hls")] {
        for value in [
            serde_json::json!({"scope":"output","state":"started","output_index":index,"output_total":2,"output_format":format}),
            serde_json::json!({"scope":"output-step","state":"started","step":"conversion","output_index":index,"output_total":2,"output_format":format}),
        ] {
            assert!(pipeline.observe(&record(value), Duration::from_secs(10)));
        }
    }
    for (state, done, second) in [
        ("started", 0, 10),
        ("running", 100_000_000, 12),
        ("running", 400_000_000, 14),
    ] {
        assert!(pipeline.observe_nested(&record(serde_json::json!({"stage":"media-progress","event":"progress","task":"packaging","state":state,"batch_id":1,"completed_bytes":done,"total_bytes":857_447_176_u64,"estimated":true,"output_index":0,"output_total":2,"output_format":"dash","url":"private-sentinel"})),Duration::from_secs(second)));
    }
    pipeline
}

#[test]
fn observed_output_work_and_remaining_time_fit_narrow_terminal_rows() {
    let pipeline = started();
    let now = Duration::from_secs(15);
    let header = pipeline_message(&pipeline, None, now);
    let detail = output_details(&pipeline, now, true);
    assert!(
        header.contains("% overall work") && header.contains("estimating remaining time"),
        "{header}"
    );
    assert!(header.contains("0/2 checked | 2 active"), "{header}");
    assert!(
        detail.contains("Output 1 · DASH fMP4: Writing media"),
        "{detail}"
    );
    assert!(
        detail.contains("Output 2 · HLS fMP4: Preparing output"),
        "{detail}"
    );
    assert!(
        detail.contains("381.47 MiB / ~817.73 MiB generated"),
        "{detail}"
    );
    for line in header.lines().chain(detail.lines()) {
        assert!(
            line.chars().count() <= 78,
            "row exceeds an 80-column terminal: {line}"
        );
    }
    assert!(!detail.contains("private-sentinel") && !detail.contains("Working"));
    for now in [Duration::from_hours(168), Duration::MAX] {
        for line in pipeline_message(&pipeline, None, now).lines() {
            assert!(
                line.chars().count() <= 78,
                "long-duration row exceeds an 80-column terminal: {line}"
            );
        }
    }
}

#[test]
fn plain_output_uses_bounded_snapshots_and_failure_has_no_success_history() {
    let mut pipeline = started();
    let mut display = Display::default();
    let mut out = Vec::new();
    for second in 15..75 {
        display
            .render(
                &mut out,
                None,
                "convert",
                &View {
                    activity: "packaging",
                    task: "",
                    downloads: None,
                    units: None,
                    pipeline: Some(&pipeline),
                },
                Duration::from_secs(second),
                None,
            )
            .unwrap();
    }
    for (index, format) in [(0, "dash"), (1, "hls")] {
        assert!(pipeline.observe(&record(serde_json::json!({"scope":"output","state":"aborted","output_index":index,"output_total":2,"output_format":format})),Duration::from_secs(75)));
    }
    assert!(pipeline.observe(
        &record(serde_json::json!({"scope":"outputs","state":"aborted"})),
        Duration::from_secs(75)
    ));
    display
        .render(
            &mut out,
            None,
            "convert",
            &View {
                activity: "packaging",
                task: "",
                downloads: None,
                units: None,
                pipeline: Some(&pipeline),
            },
            Duration::from_secs(76),
            Some(("failed", Some("cancelled"))),
        )
        .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.lines().count() <= 45, "{text}");
    assert!(
        !text.contains("Finished") && !text.contains("Outputs checked") && !text.contains("100%"),
        "{text}"
    );
    assert!(!text.contains('\u{1b}') && !text.contains('\r') && !text.contains("private-sentinel"));
    assert!(
        text.ends_with("Failed convert in 1m 16s | cancelled\n"),
        "{text}"
    );
}

#[test]
fn preparation_counts_and_current_task_eta_are_separate_from_an_unknown_global_eta() {
    let mut pipeline = State::default();
    for (value, second) in [
        (serde_json::json!({"scope":"source","state":"started"}), 0),
        (
            serde_json::json!({"scope":"work-plan","state":"ready","planned_outputs":1,"estimates_enabled":true,"source_completed":2}),
            1,
        ),
        (serde_json::json!({"scope":"source","state":"finished"}), 10),
        (serde_json::json!({"scope":"outputs","state":"started"}), 10),
        (
            serde_json::json!({"scope":"output","state":"started","output_index":0,"output_total":1,"output_format":"dash"}),
            10,
        ),
        (
            serde_json::json!({"scope":"output-step","state":"started","step":"conversion","output_index":0,"output_total":1,"output_format":"dash"}),
            10,
        ),
    ] {
        assert!(pipeline.observe(&record(value), Duration::from_secs(second)));
    }
    for (state, done, second) in [("started", 0, 10), ("running", 1, 12), ("running", 2, 14)] {
        assert!(pipeline.observe_nested(&record(serde_json::json!({"stage":"media-progress","event":"progress","task":"preparation","subject":"output","purpose":"conversion","unit":"tracks","state":state,"batch_id":1,"completed_units":done,"total_units":4,"estimated":false,"output_index":0,"output_total":1,"output_format":"dash","url":"private-sentinel"})),Duration::from_secs(second)));
    }
    let header = pipeline_message(&pipeline, None, Duration::from_secs(14));
    let detail = output_details(&pipeline, Duration::from_secs(14), true);
    assert!(header.contains("estimating remaining time"), "{header}");
    assert!(
        detail.contains("Preparing output tracks") && detail.contains("2/4 tracks prepared"),
        "{detail}"
    );
    assert!(detail.contains("~4s for current task (est.)"), "{detail}");
    assert!(!detail.contains("MiB") && !detail.contains("private-sentinel"));
    for line in header.lines().chain(detail.lines()) {
        assert!(line.chars().count() <= 78, "{line}");
    }
}

#[test]
fn finished_packaging_does_not_leave_a_stale_generated_size_tally() {
    let mut pipeline = started();
    assert!(pipeline.observe_nested(&record(serde_json::json!({"stage":"media-progress","event":"progress","task":"packaging","state":"finished","batch_id":1,"completed_bytes":410_000_000,"total_bytes":857_447_176_u64,"estimated":true,"output_index":0,"output_total":2,"output_format":"dash"})),Duration::from_secs(15)));
    let detail = output_details(&pipeline, Duration::from_secs(16), true);
    assert!(detail.contains("Checking converted media"), "{detail}");
    assert!(
        !detail.contains("generated") && !detail.contains("MiB"),
        "{detail}"
    );
    let value = pipeline.json(Duration::from_secs(16));
    assert_eq!(value["outputs"][0]["media"]["completed_bytes"], 410_000_000);
    assert_eq!(value["outputs"][0]["media"]["state"], "finished");
}

#[test]
fn terminal_coalesces_bursts_until_the_next_readable_snapshot() {
    let mut pipeline = started();
    let mut display = Display::default();
    let bar = ProgressBar::hidden();
    let mut out = Vec::new();
    let render = |display: &mut Display, pipeline: &State, now: Duration| {
        display
            .render(
                &mut Vec::new(),
                Some(&bar),
                "convert",
                &View {
                    activity: "packaging",
                    task: "",
                    downloads: None,
                    units: None,
                    pipeline: Some(pipeline),
                },
                now,
                None,
            )
            .unwrap();
    };
    let began = Duration::from_secs(15);
    render(&mut display, &pipeline, began);
    let first = bar.message();
    assert!(first.contains("381.47 MiB"), "{first}");
    for tick in 1..1000 {
        let now = began + Duration::from_millis(tick);
        assert!(pipeline.observe_nested(
            &record(serde_json::json!({
                "stage":"media-progress", "event":"progress", "task":"packaging",
                "state":"running", "batch_id":1, "completed_bytes":400_000_000 + tick * 400_000,
                "total_bytes":857_447_176_u64, "estimated":true,
                "output_index":0, "output_total":2, "output_format":"dash"
            })),
            now
        ));
        render(&mut display, &pipeline, now);
        assert_eq!(bar.message(), first, "live text changed within one second");
    }
    render(&mut display, &pipeline, began + Duration::from_secs(1));
    assert_ne!(bar.message(), first);
    assert!(bar.message().contains("762.56 MiB"), "{}", bar.message());
    // Ending a task may not momentarily expose the fallback label or shrink its rows.
    let latest = bar.message();
    assert!(pipeline.observe_nested(
        &record(serde_json::json!({
            "stage":"media-progress", "event":"progress", "task":"packaging",
            "state":"finished", "batch_id":1, "completed_bytes":857_447_176_u64,
            "total_bytes":857_447_176_u64, "estimated":true,
            "output_index":0, "output_total":2, "output_format":"dash"
        })),
        began + Duration::from_millis(1001)
    ));
    render(&mut display, &pipeline, began + Duration::from_millis(1001));
    assert_eq!(bar.message(), latest);
    render(&mut display, &pipeline, began + Duration::from_secs(2));
    assert_eq!(bar.message().lines().count(), latest.lines().count());
    // A failure must bypass the paint cadence without producing a success line.
    display
        .render(
            &mut out,
            Some(&bar),
            "convert",
            &View {
                activity: "packaging",
                task: "",
                downloads: None,
                units: None,
                pipeline: Some(&pipeline),
            },
            began + Duration::from_millis(2001),
            Some(("failed", Some("source-changed"))),
        )
        .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("Failed convert"), "{text}");
    assert!(!text.contains("Finished convert"), "{text}");
}

#[test]
fn terminal_download_bursts_keep_latest_counters_for_the_next_snapshot() {
    let mut display = Display::default();
    let bar = ProgressBar::hidden();
    let mut out = Vec::new();
    for tick in 0..=1000 {
        let view = View {
            activity: "acquisition",
            task: "",
            downloads: Some([1000, tick, tick * 1000]),
            units: Some(Units {
                completed: tick,
                total: 1000,
                initial: 0,
                began: Duration::ZERO,
                segments: false,
                checking: false,
            }),
            pipeline: None,
        };
        display
            .render(
                &mut out,
                Some(&bar),
                "analyze",
                &view,
                Duration::from_millis(tick),
                None,
            )
            .unwrap();
        if tick < 1000 {
            assert!(bar.message().contains("0/1000 files"), "{}", bar.message());
        } else {
            assert!(
                bar.message().contains("1000/1000 files"),
                "{}",
                bar.message()
            );
        }
    }
}

#[test]
fn terminal_eight_outputs_keep_two_rows_each_when_task_estimates_change() {
    let mut pipeline = State::default();
    for value in [
        serde_json::json!({"scope":"source","state":"started"}),
        serde_json::json!({"scope":"work-plan","state":"ready","planned_outputs":8,"estimates_enabled":true,"source_completed":2}),
        serde_json::json!({"scope":"source","state":"finished"}),
        serde_json::json!({"scope":"outputs","state":"started"}),
    ] {
        assert!(pipeline.observe(&record(value), Duration::ZERO));
    }
    for index in 0..8 {
        for value in [
            serde_json::json!({"scope":"output","state":"started","output_index":index,"output_total":8,"output_format":"dash"}),
            serde_json::json!({"scope":"output-step","state":"started","step":"conversion","output_index":index,"output_total":8,"output_format":"dash"}),
        ] {
            assert!(pipeline.observe(&record(value), Duration::ZERO));
        }
    }
    for state in ["started", "running", "finished"] {
        let now = Duration::from_secs(if state == "started" { 1 } else { 10 });
        for index in 0..8 {
            assert!(pipeline.observe_nested(&record(serde_json::json!({
                "stage":"media-progress", "event":"progress", "task":"packaging",
                "state":state, "batch_id":1,
                "completed_bytes":if state == "started" { 0 } else if state == "running" { 400_000_000 } else { 857_447_176_u64 },
                "total_bytes":857_447_176_u64, "estimated":true,
                "output_index":index, "output_total":8, "output_format":"dash"
            })), now));
        }
        let detail = output_details(&pipeline, now, true);
        assert_eq!(detail.lines().count(), 16, "{detail}");
        assert!(
            pipeline_message(&pipeline, None, now).lines().count() + detail.lines().count() <= 24
        );
        for line in detail.lines() {
            assert!(line.chars().count() <= 78, "{line}");
        }
    }
}

#[test]
fn terminal_outcomes_bypass_the_readable_snapshot_interval() {
    for (event, code, status) in [
        ("failed", Some("source-changed"), "Failed"),
        ("cancelled", Some("cancelled"), "Cancelled"),
        ("finished", None, "Finished"),
    ] {
        let mut display = Display::default();
        let bar = ProgressBar::hidden();
        let mut out = Vec::new();
        let view = View {
            activity: "verification",
            task: "Checking media",
            downloads: None,
            units: None,
            pipeline: None,
        };
        display
            .render(&mut out, Some(&bar), "verify", &view, Duration::ZERO, None)
            .unwrap();
        display
            .render(
                &mut out,
                Some(&bar),
                "verify",
                &view,
                Duration::from_millis(1),
                Some((event, code)),
            )
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains(&format!("{status} verify")), "{text}");
        assert_eq!(
            text.contains("Finished verify"),
            event == "finished",
            "{text}"
        );
    }
}
