use super::*;
use crate::progress::pipeline::State;
use crate::progress::terminal::LiveTerminal;

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
        assert!(pipeline.observe_nested(
            &record(serde_json::json!({
                "stage":"rendition-progress", "event":"progress", "operation":"preparation",
                "subject":"output", "purpose":"conversion", "unit":"tracks", "state":state,
                "batch_id":2, "total_units":4, "started_units":done * 2,
                "active_units":done, "waiting_units":4 - done * 2,
                "completed_units":done, "stopped_units":0,
                "output_index":0, "output_total":1, "output_format":"dash",
                "url":"private-sentinel"
            })),
            Duration::from_secs(second)
        ));
    }
    let header = pipeline_message(&pipeline, None, Duration::from_secs(14));
    let detail = output_details(&pipeline, Duration::from_secs(14), true);
    assert!(header.contains("estimating remaining time"), "{header}");
    assert!(
        detail.contains("Copying prepared tracks")
            && detail.contains("2 active, 0 waiting, 2/4 done")
            && detail.contains("2/4 tracks prepared"),
        "{detail}"
    );
    assert!(detail.contains("~4s for current task (est.)"), "{detail}");
    assert!(!detail.contains("MiB") && !detail.contains("private-sentinel"));
    for columns in [40, 80, 120, 300] {
        let frame = dashboard::frame(
            "convert",
            &View {
                activity: "input-preparation",
                task: "",
                downloads: None,
                units: None,
                pipeline: Some(&pipeline),
            },
            Duration::from_secs(14),
            columns,
        );
        assert_eq!(frame.lines().count(), 4, "{frame}");
        assert!(
            frame.lines().all(|line| line.len() <= columns.min(120)),
            "{frame}"
        );
        let row = frame
            .lines()
            .find(|line| line.starts_with("1 DASH"))
            .unwrap();
        assert!(
            row.contains("2 active") && row.contains("0 wait"),
            "{frame}"
        );
        if columns >= 120 {
            assert!(
                row.contains("2/4 tracks") && row.contains("step ~4s"),
                "{frame}"
            );
        }
        assert!(!frame.contains("private-sentinel"), "{frame}");
    }
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
    fn wide_view(pipeline: &State) -> View<'_> {
        View {
            activity: "packaging",
            task: "",
            downloads: None,
            units: None,
            pipeline: Some(pipeline),
        }
    }
    let mut pipeline = started();
    let mut display = Display::default();
    let bar = LiveTerminal::for_test(120, 24);
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
    assert!(first.contains("~9%"), "{first}");

    assert!(dashboard::frame("convert", &wide_view(&pipeline), began, 120).contains("381.47 MiB"));
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
    assert!(
        bar.message()
            .contains(&format!("~{}%", pipeline.outputs()[0].estimated_percent())),
        "{}",
        bar.message()
    );
    assert!(
        dashboard::frame(
            "convert",
            &wide_view(&pipeline),
            began + Duration::from_secs(1),
            120
        )
        .contains("762.56 MiB")
    );
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
    let bar = LiveTerminal::for_test(120, 24);
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
fn dashboard_eight_outputs_keep_one_row_each_when_task_estimates_change() {
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
        let view = View {
            activity: "packaging",
            task: "",
            downloads: None,
            units: None,
            pipeline: Some(&pipeline),
        };
        for columns in [40, 78, 120, 300] {
            let frame = dashboard::frame("convert", &view, now, columns);
            assert_eq!(frame.lines().count(), 11, "{frame}");
            for (index, line) in frame.lines().enumerate() {
                assert!(
                    !line.trim().is_empty() && line.chars().count() <= columns.min(120),
                    "{line}"
                );
                if index >= 3 {
                    assert!(
                        line.starts_with(&format!("{} DASH fMP4", index - 2)),
                        "{line}"
                    );
                }
            }
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
        let bar = LiveTerminal::for_test(120, 24);
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
        assert!(
            text.rfind("\x1b[?2026l\r\n").unwrap()
                < text.find(&format!("{status} verify")).unwrap(),
            "inline rows must finish before the final status: {text:?}"
        );
        assert_eq!(
            text.contains("Finished verify"),
            event == "finished",
            "{text}"
        );
    }
}

#[test]
fn dashboard_has_one_nonempty_row_per_output_and_no_orphan_stage_history() {
    use crate::progress::result_tests::Terminal;
    let pipeline = started();
    let mut display = Display::default();
    let terminal = Terminal::default();
    let bar = LiveTerminal::for_test(120, 24);
    display
        .render(
            &mut terminal.clone(),
            Some(&bar),
            "convert",
            &View {
                activity: "packaging",
                task: "",
                downloads: None,
                units: None,
                pipeline: Some(&pipeline),
            },
            Duration::from_secs(15),
            None,
        )
        .unwrap();
    let message = bar.message();
    assert!(
        message.lines().all(|line| !line.trim().is_empty()),
        "blank rows: {message:?}"
    );
    assert!(message.starts_with("drmr convert"), "{message}");
    assert_eq!(message.lines().count(), 5, "{message}");
    for format in ["DASH fMP4", "HLS fMP4"] {
        assert_eq!(
            message.lines().filter(|line| line.contains(format)).count(),
            1,
            "{message}"
        );
    }
    let writes = terminal.text();
    assert!(writes.contains("drmr convert"), "{writes:?}");
    assert!(
        !writes.contains("Source checked"),
        "orphan history: {writes}"
    );
}

#[test]
fn dashboard_keeps_a_checked_output_in_its_original_row() {
    let mut pipeline = started();
    let mut display = Display::default();
    let bar = LiveTerminal::for_test(120, 24);
    let render = |display: &mut Display, pipeline: &State, now| {
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
    render(&mut display, &pipeline, Duration::from_secs(15));
    let first = bar.message();
    let row = first
        .lines()
        .position(|line| line.contains("DASH fMP4"))
        .unwrap();
    assert!(pipeline.observe(&record(serde_json::json!({"scope":"output","state":"finished","output_index":0,"output_total":2,"output_format":"dash"})),Duration::from_millis(15_001)));
    render(&mut display, &pipeline, Duration::from_millis(15_001));
    assert_eq!(
        bar.message(),
        first,
        "output completion must not repaint a short-lived intermediate frame"
    );
    render(&mut display, &pipeline, Duration::from_secs(16));
    let latest = bar.message();
    let line = latest.lines().nth(row).unwrap();
    assert!(
        line.contains("DASH fMP4") && line.contains("Checked"),
        "{latest}"
    );
    assert_eq!(latest.lines().count(), first.lines().count(), "{latest}");
}

#[test]
fn dashboard_downloads_and_source_ready_are_different_states_at_narrow_widths() {
    let pipeline = started();
    let downloads = Some([708, 708, 857_447_176]);
    for columns in [1, 2, 3, 20, 40, 78, 120, 300] {
        for active in [false, true] {
            let view = View {
                activity: if active { "acquisition" } else { "packaging" },
                task: "",
                downloads,
                units: None,
                pipeline: (!active).then_some(&pipeline),
            };
            let frame = dashboard::frame("convert", &view, Duration::MAX, columns);
            for line in frame.lines() {
                assert!(
                    line.chars().count() <= columns.min(120),
                    "{columns}: {line}"
                );
            }
            if columns >= 40 {
                assert_eq!(frame.matches("Source").count(), 1, "{frame}");
                assert_eq!(frame.matches("Overall").count(), 1, "{frame}");
                assert_eq!(frame.contains("Ready"), !active, "{frame}");
                assert_eq!(frame.contains("Waiting for source"), active, "{frame}");
            }
        }
    }
}

#[test]
fn dashboard_unknown_transfer_sizes_do_not_invent_a_percentage_or_file_total() {
    let view = View {
        activity: "acquisition",
        task: "",
        downloads: Some([1, 0, 1234]),
        units: None,
        pipeline: None,
    };
    let frame = dashboard::frame("convert", &view, Duration::from_secs(1), 78);
    assert!(frame.contains("1.21 KiB"), "{frame}");
    assert!(
        !frame.contains('%') && !frame.contains("0 files") && !frame.contains("ETA"),
        "{frame}"
    );
}

#[test]
fn dashboard_global_saved_requires_publication_after_checked_outputs() {
    let mut pipeline = started();
    for (index, format) in [(0, "dash"), (1, "hls")] {
        assert!(pipeline.observe(&record(serde_json::json!({"scope":"output","state":"finished","output_index":index,"output_total":2,"output_format":format})),Duration::from_secs(16)));
    }
    let frame = dashboard::frame(
        "convert",
        &View {
            activity: "packaging",
            task: "",
            downloads: None,
            units: None,
            pipeline: Some(&pipeline),
        },
        Duration::from_secs(16),
        78,
    );
    let overall = frame
        .lines()
        .find(|line| line.starts_with("Overall"))
        .unwrap();
    assert!(
        !overall.contains("100%") && !overall.contains("Saved"),
        "{frame}"
    );
    for (scope, state) in [
        ("outputs", "finished"),
        ("final-check", "started"),
        ("final-check", "finished"),
        ("publication", "started"),
        ("publication", "finished"),
    ] {
        assert!(pipeline.observe(
            &record(serde_json::json!({"scope":scope,"state":state,"output_total":2})),
            Duration::from_secs(17)
        ));
    }
    let frame = dashboard::frame(
        "convert",
        &View {
            activity: "publication",
            task: "",
            downloads: None,
            units: None,
            pipeline: Some(&pipeline),
        },
        Duration::from_secs(17),
        78,
    );
    let overall = frame
        .lines()
        .find(|line| line.starts_with("Overall"))
        .unwrap();
    assert!(
        overall.contains("100%") && overall.contains("Saved"),
        "{frame}"
    );
}

#[test]
fn dashboard_known_preparation_keeps_current_step_progress_without_an_overall_estimate() {
    for command in ["analyze", "plan", "convert"] {
        let view = View {
            activity: "input-preparation",
            task: "Preparing media",
            downloads: None,
            units: Some(Units {
                completed: 2,
                total: 10,
                initial: 0,
                began: Duration::ZERO,
                segments: true,
                checking: false,
            }),
            pipeline: None,
        };
        let frame = dashboard::frame(command, &view, Duration::from_secs(2), 120);
        assert!(frame.contains("2/10 segments"), "{frame}");
        assert!(frame.contains("20%"), "{frame}");
        assert!(frame.contains("step ~8s"), "{frame}");
        assert!(!frame.contains("ETA"), "{frame}");
        let mut checking = view;
        checking.units.as_mut().unwrap().completed = 10;
        checking.units.as_mut().unwrap().checking = true;
        let frame = dashboard::frame(command, &checking, Duration::from_secs(3), 120);
        assert!(frame.contains("Checking boundaries"), "{frame}");
        assert!(!frame.contains('%') && !frame.contains("step ~"), "{frame}");
    }
}

#[test]
fn dashboard_output_preparation_keeps_step_estimates_and_boundary_status() {
    let mut pipeline = State::default();
    for value in [
        serde_json::json!({"scope":"source","state":"started"}),
        serde_json::json!({"scope":"source","state":"finished"}),
        serde_json::json!({"scope":"outputs","state":"started"}),
        serde_json::json!({"scope":"output","state":"started","output_index":0,"output_total":1,"output_format":"hls"}),
    ] {
        assert!(pipeline.observe(&record(value), Duration::ZERO));
    }
    for (state, completed, second) in [("started", 0, 0), ("running", 2, 2), ("running", 10, 3)] {
        assert!(pipeline.observe_nested(&record(serde_json::json!({"stage":"analysis","event":"progress","task":"ts-preparation","state":state,"completed":completed,"total":10,"output_index":0,"output_total":1,"output_format":"hls"})),Duration::from_secs(second)));
        let frame = dashboard::frame(
            "convert",
            &View {
                activity: "input-preparation",
                task: "",
                downloads: None,
                units: None,
                pipeline: Some(&pipeline),
            },
            Duration::from_secs(second),
            120,
        );
        let output = frame
            .lines()
            .find(|line| line.starts_with("1 HLS"))
            .unwrap();
        match completed {
            0 => assert!(!output.contains("step ~"), "{frame}"),
            2 => assert!(
                output.contains("20%")
                    && output.contains("step ~8s")
                    && output.contains("2/10 segments"),
                "{frame}"
            ),
            _ => assert!(
                output.contains("Checking boundaries")
                    && !output.contains('%')
                    && !output.contains("step ~"),
                "{frame}"
            ),
        }
    }
}

#[test]
fn dashboard_shows_task_concurrency_without_adding_rows_or_equating_it_with_cpus() {
    let mut state = State::default();
    assert!(state.observe(
        &record(serde_json::json!({"scope":"source","state":"started"})),
        Duration::ZERO
    ));
    for (status, started, active, done) in [("started", 0, 0, 0), ("running", 3, 2, 1)] {
        assert!(state.observe_nested(
            &record(serde_json::json!({
                "stage":"rendition-progress","event":"progress","batch_id":20,
                "state":status,"operation":"snapshot","subject":"source",
                "purpose":"conversion","unit":"tracks","total_units":7,
                "started_units":started,"active_units":active,"waiting_units":7-started,
                "completed_units":done,"stopped_units":0,"url":"private-sentinel",
            })),
            Duration::from_secs(1)
        ));
    }
    let view = View {
        activity: "input-preparation",
        task: "",
        downloads: None,
        units: None,
        pipeline: Some(&state),
    };
    for columns in [40, 80, 120, 300] {
        let frame = dashboard::frame("convert", &view, Duration::from_secs(1), columns);
        assert_eq!(frame.lines().count(), 3, "{frame}");
        assert!(
            frame
                .lines()
                .all(|line| !line.trim().is_empty() && line.len() <= columns.min(120))
        );
        assert!(
            frame.contains("2 active") && frame.contains("4 wait"),
            "{frame}"
        );
        assert!(frame.contains("1/7"), "{frame}");
        assert!(!frame.contains("CPU") && !frame.contains("private-sentinel"));
    }
}

#[test]
fn rendition_comparison_keeps_current_media_counts_and_task_eta() {
    for tracks in [false, true] {
        let mut pipeline = started();
        assert!(pipeline.observe_nested(
            &record(serde_json::json!({
                "stage":"media-progress", "event":"progress", "task":"packaging",
                "state":"finished", "batch_id":1, "completed_bytes":857_447_176_u64,
                "total_bytes":857_447_176_u64, "estimated":true,
                "output_index":0, "output_total":2, "output_format":"dash"
            })),
            Duration::from_secs(15)
        ));
        for (state, done, second) in [("started", 0, 16), ("running", 1, 18), ("running", 2, 20)] {
            let mut media = serde_json::json!({
                "stage":"media-progress", "event":"progress", "task":"comparison",
                "state":state, "batch_id":2, "completed_bytes":done * 1024,
                "total_bytes":4096, "estimated":false,
                "output_index":0, "output_total":2, "output_format":"dash",
                "url":"private-sentinel"
            });
            if tracks {
                media["completed_units"] = done.into();
                media["total_units"] = 4.into();
            }
            assert!(pipeline.observe_nested(&record(media), Duration::from_secs(second)));
            assert!(pipeline.observe_nested(
                &record(serde_json::json!({
                    "stage":"rendition-progress", "event":"progress", "operation":"comparison",
                    "subject":"result", "purpose":"verification", "unit":"tracks", "state":state,
                    "batch_id":3, "total_units":4, "started_units":done * 2,
                    "active_units":done, "waiting_units":4 - done * 2,
                    "completed_units":done, "stopped_units":0,
                    "output_index":0, "output_total":2, "output_format":"dash"
                })),
                Duration::from_secs(second)
            ));
        }
        let now = Duration::from_secs(20);
        let detail = output_details(&pipeline, now, true);
        assert!(detail.contains("2 active, 0 waiting, 2/4 done"), "{detail}");
        assert!(detail.contains("~4s for current task (est.)"), "{detail}");
        let plain_count = if tracks {
            "2/4 tracks compared"
        } else {
            "2.00 KiB / 4.00 KiB comparison work"
        };
        assert!(detail.contains(plain_count), "{detail}");
        assert!(
            !detail.contains("generated") && !detail.contains("private-sentinel"),
            "{detail}"
        );
        for columns in [40, 80, 120, 300] {
            let frame = dashboard::frame(
                "convert",
                &View {
                    activity: "verification",
                    task: "",
                    downloads: None,
                    units: None,
                    pipeline: Some(&pipeline),
                },
                now,
                columns,
            );
            assert_eq!(frame.lines().count(), 5, "{frame}");
            assert!(
                frame.lines().all(|line| line.len() <= columns.min(120)),
                "{frame}"
            );
            let row = frame
                .lines()
                .find(|line| line.starts_with("1 DASH"))
                .unwrap();
            assert!(
                row.contains("2 active") && row.contains("0 wait"),
                "{frame}"
            );
            if columns >= 120 {
                let count = if tracks {
                    "2/4 tracks"
                } else {
                    "2.00 KiB/4.00 KiB"
                };
                assert!(row.contains(count) && row.contains("step ~4s"), "{frame}");
            }
            assert!(!frame.contains("private-sentinel"), "{frame}");
        }
    }
}

mod source_visibility;
