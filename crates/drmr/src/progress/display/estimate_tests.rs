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
        header.contains("% overall work") && header.contains("remaining (est.)"),
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
