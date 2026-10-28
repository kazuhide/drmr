use super::*;
use drmr_core::events::Record;
use serde_json::{Value, json};

fn record(value: Value) -> Record {
    let Value::Object(record) = value else {
        panic!("expected object");
    };
    record
}

fn source(estimates: bool) -> State {
    let mut state = State::default();
    for value in [
        json!({"scope":"source","state":"started"}),
        json!({"scope":"work-plan","state":"ready","planned_outputs":2,"estimates_enabled":estimates,"source_completed":2}),
    ] {
        assert!(state.observe(&record(value), Duration::ZERO));
    }
    state
}

fn outputs(estimates: bool) -> State {
    let mut state = source(estimates);
    for scope in ["source", "outputs"] {
        let status = if scope == "source" {
            "finished"
        } else {
            "started"
        };
        assert!(state.observe(
            &record(json!({"scope":scope,"state":status})),
            Duration::from_secs(10),
        ));
    }
    for (index, format) in [(0, "dash"), (1, "hls")] {
        for value in [
            json!({"scope":"output","state":"started","output_index":index,"output_total":2,"output_format":format}),
            json!({"scope":"output-step","state":"started","step":"conversion","output_index":index,"output_total":2,"output_format":format}),
        ] {
            assert!(state.observe(&record(value), Duration::from_secs(10)));
        }
    }
    state
}

fn work(state: &str, counts: [u64; 3], total: u64, output: Option<usize>) -> Value {
    let [started, active, done] = counts;
    let mut value = json!({
        "stage":"rendition-progress", "event":"progress", "batch_id":20,
        "state":state, "operation":"snapshot", "subject":"source", "purpose":"conversion",
        "unit":"tracks", "total_units":total, "started_units":started,
        "active_units":active, "waiting_units":total-started, "completed_units":done,
        "stopped_units":started-active-done, "url":"private-sentinel", "path":"private-sentinel",
    });
    if let Some(index) = output {
        value["output_index"] = index.into();
        value["output_total"] = 2.into();
        value["output_format"] = if index == 0 { "dash" } else { "hls" }.into();
    }
    value
}

fn observe(state: &mut State, value: Value, second: u64) {
    assert!(state.observe_nested(&record(value), Duration::from_secs(second)));
}

fn draw(state: &State, columns: usize) -> String {
    frame(
        "convert",
        &View {
            activity: "packaging",
            task: "",
            downloads: None,
            units: None,
            pipeline: Some(state),
        },
        Duration::from_secs(20),
        columns,
    )
}

fn output_line(frame: &str, index: usize) -> &str {
    let prefix = format!("{} ", index + 1);
    frame
        .lines()
        .find(|line| line.starts_with(&prefix))
        .unwrap()
}

fn assert_bounded(frame: &str, columns: usize) {
    assert!(
        frame.lines().all(|line| {
            !line.trim().is_empty() && console::measure_text_width(line) <= columns.min(120)
        }),
        "{frame}"
    );
    assert!(!frame.contains("private-sentinel"), "{frame}");
}

#[test]
fn output_progress_column_stays_fixed_when_rendition_work_arrives() {
    for columns in [80, 120, 300] {
        let mut state = outputs(true);
        let before = draw(&state, columns);
        let column = output_line(&before, 1).find('[').unwrap();
        for (status, counts) in [
            ("started", [0, 0, 0]),
            ("running", [3, 2, 1]),
            ("running", [7, 2, 5]),
            ("finished", [7, 0, 7]),
        ] {
            observe(&mut state, work(status, counts, 7, Some(1)), 20);
            let after = draw(&state, columns);
            assert_eq!(
                output_line(&after, 1).find('['),
                Some(column),
                "{before}\n{after}"
            );
            assert_eq!(before.lines().count(), after.lines().count());
            assert_bounded(&after, columns);
        }
    }
}

#[test]
fn compact_output_keeps_real_task_counts_without_moving_estimates() {
    let mut state = outputs(true);
    let first = draw(&state, 40);
    assert!(!output_line(&first, 1).contains('%'), "{first}");
    for (status, counts) in [
        ("started", [0, 0, 0]),
        ("running", [12, 10, 2]),
        ("running", [64, 10, 54]),
    ] {
        observe(&mut state, work(status, counts, 64, Some(1)), 20);
        let current = draw(&state, 40);
        let row = output_line(&current, 1);
        assert!(row.contains("Copy"), "{current}");
        assert!(row.contains(&format!("{} active", counts[1])), "{current}");
        assert!(
            row.contains(&format!("{} wait", 64 - counts[0])),
            "{current}"
        );
        assert!(row.contains(&format!("{}/64", counts[2])), "{current}");
        assert!(!row.contains('%') && !row.contains('['), "{current}");
        assert_bounded(&current, 40);
    }
}

#[test]
fn output_progress_column_stays_fixed_when_counter_widths_change() {
    for columns in [80, 120, 300] {
        let mut state = outputs(true);
        let first = draw(&state, columns);
        let column = output_line(&first, 1).find('[').unwrap();
        for (status, counts) in [
            ("started", [0, 0, 0]),
            ("running", [12, 10, 2]),
            ("running", [64, 10, 54]),
        ] {
            observe(&mut state, work(status, counts, 64, Some(1)), 20);
            let current = draw(&state, columns);
            let row = output_line(&current, 1);
            assert_eq!(row.find('['), Some(column), "{current}");
            assert!(row.contains(&format!("{} active", counts[1])), "{current}");
            assert!(
                row.contains(&format!("{} wait", 64 - counts[0])),
                "{current}"
            );
            assert!(row.contains(&format!("{}/64", counts[2])), "{current}");
            assert_bounded(&current, columns);
        }
    }
}

fn comparison(estimates: bool, tracks: bool) -> State {
    let mut state = outputs(estimates);
    for (status, done, second) in [
        ("started", 0, 10),
        ("running", 100_000_000, 12),
        ("running", 400_000_000, 14),
        ("finished", 857_447_176_u64, 15),
    ] {
        observe(
            &mut state,
            json!({
                "stage":"media-progress","event":"progress","task":"packaging", "state":status,
                "batch_id":1,"completed_bytes":done,"total_bytes":857_447_176_u64,
                "estimated":true,"output_index":0,"output_total":2,"output_format":"dash"
            }),
            second,
        );
    }
    for (status, done, second) in [("started", 0, 16), ("running", 1, 18), ("running", 2, 20)] {
        let mut media = json!({
            "stage":"media-progress","event":"progress","task":"comparison", "state":status,
            "batch_id":2,"completed_bytes":done*1024,"total_bytes":4096,"estimated":false,
            "output_index":0,"output_total":2,"output_format":"dash",
        });
        if tracks {
            media["completed_units"] = done.into();
            media["total_units"] = 4.into();
        }
        observe(&mut state, media, second);
        let mut units = work(status, [done * 2, done, done], 4, Some(0));
        units["batch_id"] = 3.into();
        units["operation"] = "comparison".into();
        units["subject"] = "result".into();
        units["purpose"] = "verification".into();
        observe(&mut state, units, second);
    }
    state
}

#[test]
fn wide_output_keeps_counts_media_and_eta_beside_fixed_progress() {
    for tracks in [false, true] {
        let state = comparison(true, tracks);
        for columns in [120, 300] {
            let current = draw(&state, columns);
            let row = output_line(&current, 0);
            assert!(row.contains('['), "{current}");
            assert!(
                row.contains("2 active") && row.contains("0 wait") && row.contains("2/4"),
                "{current}"
            );
            assert!(row.contains("step ~4s"), "{current}");
            let media = if tracks {
                "2/4 tracks"
            } else {
                "2.00 KiB/4.00 KiB"
            };
            assert!(row.contains(media), "{current}");
            assert!(
                row.find('[').unwrap() < row.find("Compar").unwrap(),
                "{current}"
            );
            assert_bounded(&current, columns);
        }
    }
}

#[test]
fn disabled_estimates_reserve_columns_without_inventing_percentages() {
    for columns in [40, 80, 120, 300] {
        let enabled = draw(&outputs(true), columns);
        let disabled = draw(&outputs(false), columns);
        assert_eq!(
            output_line(&enabled, 1).find("Converting"),
            output_line(&disabled, 1).find("Converting"),
            "{enabled}\n{disabled}",
        );
        let state = comparison(false, false);
        let current = draw(&state, columns);
        let row = output_line(&current, 0);
        assert!(
            row.contains("2 active") && row.contains("0 wait") && row.contains("2/4"),
            "{current}"
        );
        assert!(
            !current.contains('%') && !current.contains("ETA") && !current.contains("step ~"),
            "{current}"
        );
        if columns >= 120 {
            assert!(row.contains("2.00 KiB/4.00 KiB"), "{current}");
        }
        assert_bounded(&current, columns);
    }
}

#[test]
fn source_columns_stay_fixed_across_download_check_and_work() {
    for columns in [40, 80, 120, 300] {
        let before = frame(
            "convert",
            &View {
                activity: "acquisition",
                task: "",
                downloads: Some([3, 2, 2048]),
                units: Some(Units {
                    completed: 2,
                    total: 10,
                    initial: 0,
                    began: Duration::ZERO,
                    segments: false,
                    checking: false,
                }),
                pipeline: None,
            },
            Duration::from_secs(2),
            columns,
        );
        let downloaded = before
            .lines()
            .find(|line| line.starts_with("Source"))
            .unwrap();
        let details_column = downloaded.find("Downloading").unwrap();
        if columns >= 120 {
            assert!(
                downloaded.contains("2/10 files")
                    && downloaded.contains("2.00 KiB")
                    && downloaded.contains("ETA ~8s"),
                "{before}"
            );
        }
        let mut state = source(false);
        for (caption, progress) in [
            ("Checking", None),
            ("Copy", Some(work("started", [0, 0, 0], 7, None))),
            ("Copy", Some(work("running", [3, 2, 1], 7, None))),
        ] {
            if let Some(value) = progress {
                observe(&mut state, value, 2);
            }
            let current = draw(&state, columns);
            let row = current
                .lines()
                .find(|line| line.starts_with("Source"))
                .unwrap();
            assert_eq!(
                row.find(caption),
                Some(details_column),
                "{before}\n{current}"
            );
            assert!(!row.contains('%'), "{current}");
            assert_bounded(&current, columns);
        }
        assert!(state.observe(
            &record(json!({"scope":"source","state":"finished"})),
            Duration::from_secs(3)
        ));
        let current = draw(&state, columns);
        let row = current
            .lines()
            .find(|line| line.starts_with("Source"))
            .unwrap();
        assert_eq!(row.find("Ready"), Some(details_column), "{current}");
        assert!(!row.contains("active") && !row.contains('%'), "{current}");
    }
}

#[test]
fn long_labels_cannot_push_details_to_the_right() {
    for columns in [40, 80, 120, 300] {
        let short = row("Short", "detail-token", columns);
        let long = row(
            "An unexpectedly long representation label",
            "detail-token",
            columns,
        );
        assert_eq!(
            short.find("detail-token"),
            long.find("detail-token"),
            "{short}\n{long}"
        );
        assert_bounded(&long, columns);
    }
}

#[test]
fn lagging_acquisition_cannot_replay_download_percent_into_source_work() {
    for columns in [40, 80, 120, 300] {
        let mut state = source(false);
        observe(&mut state, work("started", [0, 0, 0], 7, None), 2);
        observe(&mut state, work("running", [3, 2, 1], 7, None), 3);
        for finished in [false, true] {
            if finished {
                assert!(state.observe(
                    &record(json!({"scope":"source","state":"finished"})),
                    Duration::from_secs(4),
                ));
            }
            let current = frame(
                "convert",
                &View {
                    activity: "acquisition",
                    task: "",
                    downloads: Some([10, 10, 10 * 1024]),
                    units: Some(Units {
                        completed: 10,
                        total: 10,
                        initial: 0,
                        began: Duration::ZERO,
                        segments: false,
                        checking: false,
                    }),
                    pipeline: Some(&state),
                },
                Duration::from_secs(4),
                columns,
            );
            let row = current
                .lines()
                .find(|line| line.starts_with("Source"))
                .unwrap();
            assert!(!row.contains('%'), "{current}");
            if finished {
                assert!(
                    row.contains("Ready") && !row.contains("active"),
                    "{current}"
                );
            } else {
                assert!(
                    row.contains("Copy")
                        && row.contains("2 active")
                        && row.contains("4 wait")
                        && row.contains("1/7"),
                    "{current}",
                );
            }
            assert_bounded(&current, columns);
        }
    }
}

#[test]
fn independent_long_caption_keeps_actual_work_counts_at_eighty_columns() {
    let mut state = outputs(true);
    assert!(state.observe(
        &record(json!({
            "scope":"output-step","state":"started","step":"verification",
            "output_index":0,"output_total":2,"output_format":"dash",
        })),
        Duration::from_secs(16),
    ));
    for (status, done, second) in [("started", 0, 16), ("running", 1, 18), ("running", 2, 20)] {
        observe(
            &mut state,
            json!({
                "stage":"media-progress","event":"progress","task":"comparison",
                "state":status,"batch_id":1,"completed_bytes":done*1024,"total_bytes":4096,
                "estimated":false,"completed_units":done,"total_units":4,
                "output_index":0,"output_total":2,"output_format":"dash",
            }),
            second,
        );
        let mut units = work(status, [done * 2, done, done], 4, Some(0));
        units["operation"] = "comparison".into();
        units["subject"] = "result".into();
        units["purpose"] = "verification".into();
        observe(&mut state, units, second);
    }
    assert_eq!(
        state.outputs()[0].progress_label(),
        "Comparing original and result",
    );
    let current = draw(&state, 80);
    let row = output_line(&current, 0);
    assert!(row.contains('['), "{current}");
    assert!(row.contains("Compar") || row.contains("Check"), "{current}");
    assert!(row.contains("2 active"), "{current}");
    assert!(row.contains("0 wait"), "{current}");
    assert!(row.contains("2/4"), "{current}");
    assert_bounded(&current, 80);
}
