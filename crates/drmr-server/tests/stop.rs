mod support;

use serde_json::json;
use support::{Harness, error, sse_events, stage};

#[test]
fn stop_is_scoped_idempotent_and_keeps_the_actual_terminal_outcome() {
    let mut h = Harness::new();
    stage(&h.catalog, "stoppable");
    h.cli.hold();
    let start = h
        .client
        .post_json("/api/convert", &json!({"asset_id": "stoppable"}));
    assert_eq!(start.status, 202);
    let job_id = start.json()["job_id"].as_str().unwrap().to_string();
    h.cli.wait_calls(1);
    let path = format!("/api/jobs/{job_id}/stop");
    for _ in 0..3 {
        let answer = h.client.post_json(&path, &json!({}));
        assert!(matches!(answer.status, 200 | 202));
        assert_eq!(answer.json()["job_id"], job_id);
        assert_eq!(answer.json()["stop_requested"], true);
    }
    let stopped = h.wait_job(&job_id);
    assert_eq!(stopped["state"], "interrupted");
    assert_eq!(stopped["stop_requested"], true);
    assert!(stopped["exit_code"].is_i64());
    let done = h.client.post_json(&path, &json!({}));
    assert_eq!(done.status, 200);
    let events = sse_events(&h.client.get(&format!("/api/jobs/{job_id}/log")).text());
    let final_event = events.last().unwrap();
    assert_eq!(final_event.0, "done");
    let final_payload: serde_json::Value = serde_json::from_str(&final_event.1).unwrap();
    assert_eq!(final_payload["state"], "interrupted");
    assert_eq!(final_payload["exit_code"], stopped["exit_code"]);
    h.start();
    let history = h.client.get("/api/hls/jobs").json();
    assert_eq!(history["jobs"][0]["state"], "interrupted");
    assert_eq!(history["jobs"][0]["exit_code"], stopped["exit_code"]);
    assert_eq!(history["jobs"][0]["stop_requested"], true);
    assert_eq!(h.client.post_json(&path, &json!({})).status, 200);
    assert_eq!(h.cli.calls(), 1);
}

#[test]
fn stop_rejects_foreign_non_json_large_and_command_shaped_requests_before_work() {
    let h = Harness::new();
    let path = "/api/jobs/unknown/stop";
    for headers in [
        [("Origin", "https://other.invalid")],
        [("Sec-Fetch-Site", "cross-site")],
    ] {
        let response = h.client.post_json_with(path, &json!({}), &headers);
        assert_eq!(
            (response.status, response.json()),
            (403, error("same-origin-required"))
        );
    }
    let response = h
        .client
        .post_raw(path, b"{}", &[("Content-Type", "text/plain")]);
    assert_eq!(
        (response.status, response.json()),
        (415, error("json-required"))
    );
    for body in [
        json!({"pid": 1}),
        json!({"command": "stop"}),
        json!({"path": "/tmp"}),
        json!([]),
        json!(null),
    ] {
        assert_eq!(h.client.post_json(path, &body).status, 400);
    }
    let oversized = vec![b' '; 256 * 1024 + 1];
    let response = h
        .client
        .post_raw(path, &oversized, &[("Content-Type", "application/json")]);
    assert_eq!(
        (response.status, response.json()),
        (413, error("request-too-large"))
    );
    assert_eq!(h.client.post_json(path, &json!({})).status, 404);
    assert_eq!(h.cli.calls(), 0);
}
