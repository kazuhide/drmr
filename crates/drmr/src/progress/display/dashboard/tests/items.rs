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

#[test]
fn source_rendition_states_do_not_add_console_rows() {
    let mut state = source(true);
    let before = draw_height(&state, 120, 24);
    start_items(
        &mut state,
        &["waiting", "active", "completed", "stopped"],
        None,
        Some(&descriptions(4)),
    );
    let current = draw_height(&state, 120, 24);
    assert_eq!(current.lines().count(), before.lines().count(), "{current}");
    assert!(item_rows(&current).is_empty(), "{current}");
    assert!(
        !current.contains("Renditions") && !current.contains("1920x1080"),
        "{current}"
    );
    assert_eq!(
        state.json(Duration::from_secs(20))["source_renditions"]["unit_states"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert_bounded(&current, 120);
}

#[test]
fn output_rendition_updates_keep_only_the_fixed_summary_rows() {
    for columns in [40, 80, 120, 300] {
        let mut state = many_outputs(2);
        let before = draw_height(&state, columns, 24);
        for index in 0..2 {
            start_items(
                &mut state,
                &["active", "waiting", "waiting", "waiting"],
                Some((index, 2)),
                Some(&descriptions(4)),
            );
        }
        for states in [
            ["completed", "active", "waiting", "waiting"],
            ["completed", "completed", "active", "waiting"],
            ["completed", "completed", "completed", "completed"],
        ] {
            for index in 0..2 {
                let status = if states.iter().all(|state| *state == "completed") {
                    "finished"
                } else {
                    "running"
                };
                observe(
                    &mut state,
                    item_event(status, &states, Some((index, 2)), Some(&descriptions(4))),
                    13,
                );
            }
            let current = draw_height(&state, columns, 24);
            assert_eq!(current.lines().count(), before.lines().count(), "{current}");
            assert!(
                item_rows(&current).is_empty() && !current.contains("Renditions"),
                "{current}"
            );
            for index in 0..2 {
                assert!(
                    current
                        .lines()
                        .nth(3 + index)
                        .unwrap()
                        .starts_with(&format!("{} DASH fMP4", index + 1)),
                    "{current}"
                );
            }
            assert_bounded(&current, columns);
        }
    }
}

#[test]
fn fixed_summary_rows_obey_width_and_height_without_rendition_details() {
    let mut state = many_outputs(8);
    let states: Vec<_> = ["waiting", "active", "completed", "stopped"]
        .into_iter()
        .cycle()
        .take(64)
        .collect();
    for index in 0..8 {
        start_items(&mut state, &states, Some((index, 8)), None);
    }
    for columns in [40, 80, 120, 300] {
        for height in [1, 3, 8, 12, 24] {
            let current = draw_height(&state, columns, height);
            assert!(
                !current.is_empty() && current.lines().count() <= height,
                "{columns}x{height}:\n{current}"
            );
            assert!(
                item_rows(&current).is_empty() && !current.contains("Renditions"),
                "{current}"
            );
            if height < 11 {
                assert!(current.contains("rows hidden"), "{current}");
            } else {
                assert_eq!(current.lines().count(), 11);
                assert!(!current.contains("hidden"), "{current}");
            }
            assert_bounded(&current, columns);
        }
    }
    assert_eq!(draw_height(&state, 80, 0), "");
    for output in state.json(Duration::from_secs(20))["outputs"]
        .as_array()
        .unwrap()
    {
        assert_eq!(
            output["renditions"]["unit_states"]
                .as_array()
                .unwrap()
                .len(),
            64
        );
    }
}
