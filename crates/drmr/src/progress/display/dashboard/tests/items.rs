use super::*;

fn draw_height(state: &State, columns: usize, height: usize) -> String {
    frame_with_height(
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
        height,
    )
}

fn many_outputs(total: usize) -> State {
    let mut state = State::default();
    for value in [
        json!({"scope":"source","state":"started"}),
        json!({"scope":"work-plan","state":"ready","planned_outputs":total,"estimates_enabled":true,"source_completed":2}),
        json!({"scope":"source","state":"finished"}),
        json!({"scope":"outputs","state":"started"}),
    ] {
        assert!(state.observe(&record(value), Duration::from_secs(10)));
    }
    for index in 0..total {
        for scope in ["output", "output-step"] {
            assert!(state.observe(
                &record(json!({"scope":scope,"state":"started","step":"conversion",
                    "output_index":index,"output_total":total,"output_format":"dash"})),
                Duration::from_secs(10),
            ));
        }
    }
    state
}

fn descriptions(total: usize) -> Vec<Value> {
    (0..total)
        .map(|ordinal| match ordinal % 4 {
            0 => json!({"kind":"video","resolution":[1920,1080],"label":"private-sentinel"}),
            1 => json!({"kind":"audio","label":"private-sentinel"}),
            2 => json!({"kind":"text","label":"private-sentinel"}),
            _ => Value::Null,
        })
        .collect()
}

fn item_event(
    batch_state: &str,
    states: &[&str],
    output: Option<(usize, usize)>,
    descriptions: Option<&[Value]>,
) -> Value {
    let count = |state| states.iter().filter(|value| **value == state).count();
    let waiting = count("waiting");
    let mut value = json!({
        "stage":"rendition-progress","event":"progress","batch_id":20,
        "state":batch_state,"operation":"snapshot","subject":"source","purpose":"conversion",
        "unit":"tracks","total_units":states.len(),"started_units":states.len()-waiting,
        "active_units":count("active"),"waiting_units":waiting,
        "completed_units":count("completed"),"stopped_units":count("stopped"),
        "unit_states":states,"url":"private-sentinel","path":"private-sentinel",
    });
    if let Some(descriptions) = descriptions {
        value["unit_descriptions"] = json!(descriptions);
    }
    if let Some((index, total)) = output {
        value["output_index"] = index.into();
        value["output_total"] = total.into();
        value["output_format"] = "dash".into();
    }
    value
}

fn start_items(
    state: &mut State,
    states: &[&str],
    output: Option<(usize, usize)>,
    descriptions: Option<&[Value]>,
) {
    let waiting = vec!["waiting"; states.len()];
    observe(
        state,
        item_event("started", &waiting, output, descriptions),
        11,
    );
    observe(
        state,
        item_event("running", states, output, descriptions),
        12,
    );
}

fn item_rows(frame: &str) -> Vec<&str> {
    frame
        .lines()
        .filter(|line| {
            let mut tokens = line.split_whitespace();
            let prefix = tokens.next().unwrap_or_default();
            let kind = tokens.next().unwrap_or_default();
            (prefix == "S" || prefix.parse::<usize>().is_ok())
                && (matches!(kind, "Video" | "Audio" | "Text") || kind.starts_with('#'))
        })
        .collect()
}

fn find_item<'a>(frame: &'a str, prefix: &str) -> &'a str {
    item_rows(frame)
        .into_iter()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("missing {prefix}:\n{frame}"))
}

#[test]
fn source_item_rows_show_actual_states_and_only_typed_descriptions() {
    let mut state = source(true);
    start_items(
        &mut state,
        &["waiting", "active", "completed", "stopped"],
        None,
        Some(&descriptions(4)),
    );
    let current = draw_height(&state, 120, 24);
    assert!(current.contains("Renditions"), "{current}");
    for (prefix, status) in [
        ("S Video 1", "Waiting"),
        ("S Audio 2", "Active"),
        ("S Text 3", "Done (step)"),
        ("S #4", "Stopped"),
    ] {
        let line = find_item(&current, prefix);
        assert!(line.contains(status), "{current}");
        assert!(line.contains("Copying source tracks"), "{current}");
        assert!(!line.contains('%') && !line.contains('['), "{current}");
    }
    assert!(
        find_item(&current, "S Video 1").contains("1920x1080"),
        "{current}"
    );
    assert_eq!(item_rows(&current).len(), 4);
    assert_bounded(&current, 120);
}

#[test]
fn output_items_append_below_fixed_parent_rows_in_round_robin_order() {
    let mut state = many_outputs(2);
    let before = draw_height(&state, 80, 24);
    let states = [
        "waiting",
        "active",
        "completed",
        "stopped",
        "waiting",
        "active",
        "waiting",
    ];
    for index in 0..2 {
        start_items(
            &mut state,
            &states,
            Some((index, 2)),
            Some(&descriptions(7)),
        );
    }
    let current = draw_height(&state, 80, 24);
    assert_eq!(item_rows(&current).len(), 14, "{current}");
    assert_eq!(before.lines().next(), current.lines().next());
    for index in 0..2 {
        let before_line = before.lines().nth(3 + index).unwrap();
        let current_line = current.lines().nth(3 + index).unwrap();
        assert!(
            current_line.starts_with(&format!("{} DASH fMP4", index + 1)),
            "{current}"
        );
        assert_eq!(before_line.find('['), current_line.find('['));
    }
    let rows = item_rows(&current);
    for (ordinal, pair) in rows.as_chunks::<2>().0.iter().enumerate() {
        let kind = match ordinal % 4 {
            0 => "Video",
            1 => "Audio",
            2 => "Text",
            _ => "#",
        };
        for (index, line) in pair.iter().enumerate() {
            let label = if kind == "#" {
                format!("{} #{}", index + 1, ordinal + 1)
            } else {
                format!("{} {} {}", index + 1, kind, ordinal + 1)
            };
            assert!(line.starts_with(&label), "{current}");
        }
    }
    assert!(current.lines().count() <= 24, "{current}");
    assert_bounded(&current, 80);
}

#[test]
fn item_frames_obey_width_and_height_including_tiny_viewports() {
    let mut state = many_outputs(2);
    for index in 0..2 {
        start_items(
            &mut state,
            &[
                "waiting",
                "active",
                "completed",
                "stopped",
                "waiting",
                "active",
                "waiting",
            ],
            Some((index, 2)),
            Some(&descriptions(7)),
        );
    }
    for columns in [40, 80, 120, 300] {
        for height in [1, 8, 12, 24] {
            let current = draw_height(&state, columns, height);
            assert!(
                !current.is_empty() && current.lines().count() <= height,
                "{columns}x{height}:\n{current}"
            );
            assert_bounded(&current, columns);
            if height == 24 {
                assert_eq!(item_rows(&current).len(), 14, "{current}");
            } else {
                assert!(current.to_ascii_lowercase().contains("hidden"), "{current}");
            }
        }
    }
    assert_eq!(draw_height(&state, 80, 0), "");
}

#[test]
fn clipped_items_keep_stable_output_visibility_and_exact_hidden_counts() {
    let mut state = many_outputs(8);
    let states: Vec<_> = ["waiting", "active", "completed", "stopped"]
        .into_iter()
        .cycle()
        .take(64)
        .collect();
    for index in 0..8 {
        start_items(&mut state, &states, Some((index, 8)), None);
    }
    let current = draw_height(&state, 80, 24);
    let rows = item_rows(&current);
    assert_eq!(rows.len(), 11, "{current}");
    for (index, row) in rows.iter().take(8).enumerate() {
        assert!(row.starts_with(&format!("{} #1", index + 1)), "{current}");
    }
    let summary = current.lines().last().unwrap();
    assert!(
        summary.contains("501 hidden")
            && summary.contains("125 active")
            && summary.contains("120 waiting"),
        "{current}"
    );
    assert_eq!(current.lines().count(), 24, "{current}");
    for (index, output) in state.json(Duration::from_secs(20))["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        assert_eq!(
            output["renditions"]["unit_states"]
                .as_array()
                .unwrap()
                .len(),
            64,
            "output {index}"
        );
    }
    assert_bounded(&current, 80);
}

#[test]
fn legacy_and_opaque_work_never_invent_individual_item_states() {
    let mut legacy = outputs(true);
    observe(&mut legacy, work("started", [0, 0, 0], 7, Some(0)), 11);
    observe(&mut legacy, work("running", [3, 2, 1], 7, Some(0)), 12);
    let current = draw_height(&legacy, 80, 24);
    assert!(
        item_rows(&current).is_empty() && !current.contains("Renditions"),
        "{current}"
    );
    let opaque = draw_height(&outputs(true), 80, 24);
    assert!(
        item_rows(&opaque).is_empty() && !opaque.contains("Renditions"),
        "{opaque}"
    );
}

#[test]
fn completed_or_stopped_batches_and_terminal_contexts_remove_item_rows() {
    for terminal in ["finished", "aborted", "source-finished", "output-finished"] {
        let output = (terminal != "source-finished").then_some((0, 2));
        let mut state = if output.is_some() {
            many_outputs(2)
        } else {
            source(true)
        };
        start_items(
            &mut state,
            &["active", "waiting"],
            output,
            Some(&descriptions(2)),
        );
        assert_eq!(item_rows(&draw_height(&state, 80, 24)).len(), 2);
        match terminal {
            "finished" => observe(&mut state, item_event("finished", &["completed", "completed"], output, Some(&descriptions(2))), 13),
            "aborted" => observe(&mut state, item_event("aborted", &["stopped", "waiting"], output, Some(&descriptions(2))), 13),
            "source-finished" => assert!(state.observe(&record(json!({"scope":"source","state":"finished"})), Duration::from_secs(13))),
            _ => assert!(state.observe(&record(json!({"scope":"output","state":"finished","output_index":0,"output_total":2,"output_format":"dash"})), Duration::from_secs(13))),
        }
        let current = draw_height(&state, 80, 24);
        assert!(
            item_rows(&current).is_empty() && !current.contains("Renditions"),
            "{terminal}:\n{current}"
        );
    }
}

#[test]
fn item_state_columns_and_order_do_not_follow_active_sorting() {
    let mut state = many_outputs(1);
    start_items(
        &mut state,
        &["waiting", "active", "completed", "stopped"],
        Some((0, 1)),
        Some(&descriptions(4)),
    );
    for columns in [40, 80, 120, 300] {
        let current = draw_height(&state, columns, 24);
        let rows = item_rows(&current);
        assert_eq!(rows.len(), 4, "{current}");
        let positions: Vec<_> = rows
            .iter()
            .zip(["Waiting", "Active", "Done (step)", "Stopped"])
            .map(|(row, status)| row.find(status).unwrap())
            .collect();
        assert!(
            positions.iter().all(|position| *position == positions[0]),
            "{current}"
        );
        if columns == 40 {
            assert!(rows.iter().all(|row| row.contains("Copy")), "{current}");
        }
    }
    observe(
        &mut state,
        item_event(
            "running",
            &["active", "completed", "completed", "stopped"],
            Some((0, 1)),
            Some(&descriptions(4)),
        ),
        13,
    );
    let current = draw_height(&state, 80, 24);
    assert!(
        find_item(&current, "1 Video 1").contains("Active"),
        "{current}"
    );
    assert!(
        find_item(&current, "1 Audio 2").contains("Done (step)"),
        "{current}"
    );
    assert_bounded(&current, 80);
}
