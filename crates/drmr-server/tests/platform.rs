mod support;

use drmr_server::config::QueueRunner;
use serde_json::{Value, json};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use support::{Harness, KID, audio_rep, error, h264_rep, sse_events, stage, stage_entry};

const TWO_LINES: [&str; 2] = [
    r#"{"stage": "merge", "message": "merging 3 source segments", "rendition_id": "v0"}"#,
    r#"{"stage": "report", "message": "done", "rendition_id": "v0"}"#,
];

fn records(output: &Path, asset_id: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(output.join(asset_id).join("reports")) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_str().unwrap();
            name.starts_with("job-") && support::has_extension(name, "json")
        })
        .collect();
    files.sort();
    files
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .map(|path| path.file_name().unwrap().to_str().unwrap().to_string())
        .collect()
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

use support::set_mode;

/// A server whose fake CLI prints the two conversion lines and exits 0.
fn two_lines(configure: impl FnOnce(&mut support::HarnessSetup)) -> Harness {
    let h = Harness::with(configure);
    h.cli.lines(&TWO_LINES);
    h
}

fn convert(h: &Harness, asset_id: &str) -> String {
    let answer = h
        .client
        .post_json("/api/convert", &json!({"asset_id": asset_id}));
    assert_eq!(answer.status, 202, "{}", answer.text());
    answer.json()["job_id"].as_str().unwrap().to_string()
}

/// Read the job's live log to its end (the job has exited then).
fn drain(h: &Harness, job_id: &str) -> String {
    h.client.get(&format!("/api/jobs/{job_id}/log")).text()
}

/// Let a held job exit, then drain its log.
fn release(h: &Harness, job_id: &str) -> String {
    h.cli.release(0);
    drain(h, job_id)
}

fn event_names(text: &str) -> Vec<String> {
    sse_events(text).into_iter().map(|(name, _)| name).collect()
}

fn listed_assets(h: &Harness) -> Vec<String> {
    let page = h.client.get("/api/hls/jobs").json();
    page["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["asset_id"].as_str().unwrap().to_string())
        .collect()
}

fn queue(h: &Harness) -> Value {
    h.client.get("/api/queue").json()
}

fn pending(h: &Harness) -> Vec<String> {
    queue(h)["pending"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["asset_id"].as_str().unwrap().to_string())
        .collect()
}

fn succeeded() -> Value {
    json!({"state": "succeeded", "exit_code": 0})
}

#[test]
fn conversion_is_recorded_listed_and_replayable_after_restart() {
    let h = two_lines(|_| {});
    stage(&h.catalog, "rec1");
    let job_id = convert(&h, "rec1");
    let live = drain(&h, &job_id);
    assert_eq!(event_names(&live), ["message", "message", "done"]);
    let files = records(&h.output, "rec1");
    assert_eq!(names(&files), [format!("job-{job_id}.json")]);
    let record = read_json(&files[0]);
    assert_eq!(
        (
            record["kind"].clone(),
            record["asset_id"].clone(),
            record["state"].clone()
        ),
        (json!("convert"), json!("rec1"), json!("succeeded"))
    );
    assert!(record["analysis_id"].is_null() && record["result_id"].is_null());
    assert_eq!(record["exit_code"], 0);
    let mut keys: Vec<&String> = record.as_object().unwrap().keys().collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "analysis_id",
            "asset_id",
            "created_at",
            "exit_code",
            "hidden",
            "job_id",
            "kind",
            "result_id",
            "schema_version",
            "state"
        ]
    );
    // The catalog contract of /api/jobs/<id> is unchanged for a conversion.
    assert_eq!(
        h.client.get(&format!("/api/jobs/{job_id}")).json(),
        succeeded()
    );
    let listed = h.client.get("/api/hls/jobs").json()["jobs"].clone();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(
        (
            listed[0]["job_id"].clone(),
            listed[0]["kind"].clone(),
            listed[0]["state"].clone()
        ),
        (json!(job_id), json!("convert"), json!("succeeded"))
    );
    let (_server, again) = h.second();
    assert_eq!(
        again.get("/api/hls/jobs").json()["jobs"][0]["job_id"],
        json!(job_id)
    );
    assert_eq!(
        again.get(&format!("/api/jobs/{job_id}")).json(),
        succeeded()
    );
    let replayed = sse_events(&again.get(&format!("/api/jobs/{job_id}/log")).text());
    assert_eq!(
        replayed
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>(),
        ["message", "message", "done"]
    );
    assert_eq!(
        serde_json::from_str::<Value>(&replayed[0].1).unwrap()["message"],
        "merging 3 source segments",
        "conversion lines replay verbatim"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&replayed[2].1).unwrap(),
        succeeded()
    );
}

#[test]
fn recorded_conversion_never_reserves_the_asset_or_blocks_a_second_run() {
    let h = two_lines(|_| {});
    stage(&h.catalog, "rec2");
    let first = convert(&h, "rec2");
    drain(&h, &first);
    assert!(!h.app().service.asset_reserved("rec2"));
    let second = convert(&h, "rec2");
    assert_ne!(second, first);
    drain(&h, &second);
    assert_eq!(records(&h.output, "rec2").len(), 2);
    assert_eq!(
        h.client.delete(&format!("/api/hls/jobs/{first}")).status,
        204
    );
    let ids: Vec<Value> = h.client.get("/api/hls/jobs").json()["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["job_id"].clone())
        .collect();
    assert_eq!(ids, [json!(second)]);
}

#[test]
fn interrupted_conversion_is_reported_after_restart() {
    let h = Harness::new();
    h.cli.hold();
    stage(&h.catalog, "rec3");
    let job_id = convert(&h, "rec3");
    // A new server over the same output root: the child is gone, the record must not claim it runs.
    let (_server, restarted) = h.second();
    let interrupted = json!({"state": "interrupted", "exit_code": null});
    assert_eq!(
        restarted.get(&format!("/api/jobs/{job_id}")).json(),
        interrupted
    );
    let events = sse_events(&restarted.get(&format!("/api/jobs/{job_id}/log")).text());
    assert_eq!(
        *events.last().unwrap(),
        (
            "done".to_string(),
            r#"{"state": "interrupted", "exit_code": null}"#.to_string()
        )
    );
}

fn write_profiles(base: &Path, name: &str, profiles: &Value) -> PathBuf {
    let path = base.join(name);
    support::write_private(&path, profiles.to_string()).unwrap();
    path
}

#[test]
fn platform_facts_name_roots_and_profiles_but_never_credentials() {
    let h = Harness::with(|setup| {
        let profiles = json!({
            "default": {"policy": {"allowed_origins": ["https://media.example"], "allow_http": false,
                                   "headers_by_origin": {"https://media.example": {"Authorization": "Bearer PRIVATE"}}}},
            "lab": {"policy": {"allowed_origins": ["http://127.0.0.1:8098"], "allow_http": true}},
            "bad name/": {"policy": {}},
        });
        setup.options.hls_profiles = Some(write_profiles(&setup.base, "profiles.json", &profiles));
    });
    let response = h.client.get("/api/platform");
    assert_eq!(response.status, 200);
    assert_eq!(response.header("cache-control"), Some("no-store"));
    let body = response.json();
    assert_eq!(body["catalog_root"], json!(h.catalog.to_str().unwrap()));
    assert_eq!(body["output_root"], json!(h.output.to_str().unwrap()));
    assert_eq!(
        body["jobs_dir"],
        json!(h.output.join("_jobs").to_str().unwrap())
    );
    assert_eq!(body["concurrency"], 1);
    assert!(body["batch_available"].is_boolean());
    assert_eq!(
        body["profiles"],
        json!([{"name": "default", "allowed_origins": ["https://media.example"], "allow_http": false},
               {"name": "lab", "allowed_origins": ["http://127.0.0.1:8098"], "allow_http": true}])
    );
    let text = response.text();
    assert!(!text.contains("PRIVATE") && !text.contains("headers"));
}

#[test]
fn platform_without_profiles() {
    let h = Harness::new();
    let body = h.client.get("/api/platform").json();
    assert!(body["profiles"].is_null());
    assert_eq!(body["conventions_version"], "1");
}

fn queued(configure: impl FnOnce(&mut support::HarnessSetup)) -> Harness {
    let h = two_lines(|setup| {
        setup.options.queue_runner = QueueRunner::Disabled;
        configure(setup);
    });
    for asset in ["q1", "q2", "q3"] {
        stage(&h.catalog, asset);
    }
    h
}

fn enqueue(h: &Harness, ids: &[&str]) -> support::Answer {
    h.client.post_json("/api/queue", &json!({"asset_ids": ids}))
}

#[test]
fn queue_runs_catalog_conversions_one_at_a_time() {
    let h = queued(|_| {});
    h.cli.hold();
    let queue = h.app().queue.clone();
    assert_eq!(enqueue(&h, &["nope"]).status, 404);
    assert_eq!(enqueue(&h, &[]).status, 422);
    assert_eq!(enqueue(&h, &["q1", "q1"]).status, 422);
    let response = enqueue(&h, &["q1", "q2"]);
    assert_eq!(response.status, 202);
    let body = response.json();
    let listed: Vec<&str> = body["pending"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["asset_id"].as_str().unwrap())
        .collect();
    assert_eq!(listed, ["q1", "q2"]);
    assert_eq!(enqueue(&h, &["q2"]).status, 409);
    assert_eq!(h.client.delete("/api/queue/q2").status, 204);
    assert_eq!(h.client.delete("/api/queue/q2").status, 404);
    assert_eq!(enqueue(&h, &["q2", "q3"]).status, 202);

    let first = queue.tick().unwrap();
    let snapshot = queue_snapshot(&h);
    assert_eq!(snapshot["running"]["job_id"], json!(first));
    assert_eq!(snapshot["running"]["asset_id"], "q1");
    assert_eq!(snapshot["running"]["kind"], "convert");
    assert_eq!(pending(&h), ["q2", "q3"]);
    let recent = &snapshot["recent"][0];
    assert_eq!(
        (
            recent["asset_id"].clone(),
            recent["job_id"].clone(),
            recent["outcome"].clone(),
            recent["reason"].clone()
        ),
        (json!("q1"), json!(first), json!("launched"), Value::Null)
    );
    assert_eq!(
        queue.tick(),
        None,
        "the lease is taken: nothing launches until the running job ends"
    );
    assert_eq!(pending(&h), ["q2", "q3"]);
    // While q1 runs, a manual conversion of a queued asset is refused by the lease, not the queue.
    assert_eq!(
        h.client
            .post_json("/api/convert", &json!({"asset_id": "q2"}))
            .status,
        409
    );
    release(&h, &first);
    let second = queue.tick().unwrap();
    assert_ne!(second, first);
    assert_eq!(queue_snapshot(&h)["running"]["asset_id"], "q2");
    assert_eq!(
        names(&records(&h.output, "q1")),
        [format!("job-{first}.json")]
    );
    release(&h, &second);
    let third = queue.tick().unwrap();
    release(&h, &third);
    assert_eq!(pending(&h), Vec::<String>::new());
    assert_eq!(listed_assets(&h), ["q3", "q2", "q1"]);
}

fn queue_snapshot(h: &Harness) -> Value {
    queue(h)
}

#[test]
fn queue_skips_an_entry_that_disappeared_and_refuses_hls_reserved_assets() {
    let h = queued(|setup| {
        let reports = setup.output.join("q3").join("reports");
        support::private_dir(&reports).unwrap();
        let id = "c".repeat(32);
        let record = json!({"schema_version": "1", "job_id": id, "kind": "analysis", "asset_id": "q3",
                            "analysis_id": id, "result_id": null, "state": "failed", "exit_code": 1,
                            "created_at": "2026-10-16T00:00:00+00:00", "hidden": false});
        std::fs::write(
            reports.join(format!("hls-job-{id}.json")),
            record.to_string(),
        )
        .unwrap();
    });
    let queue = h.app().queue.clone();
    assert_eq!(enqueue(&h, &["q1", "q2"]).status, 202);
    std::fs::remove_dir_all(h.catalog.join("q1")).unwrap();
    assert_eq!(queue.tick(), None);
    let recent = &queue_snapshot(&h)["recent"][0];
    assert_eq!(
        (
            recent["asset_id"].clone(),
            recent["outcome"].clone(),
            recent["reason"].clone()
        ),
        (json!("q1"), json!("skipped"), json!("asset-not-found"))
    );
    assert_eq!(pending(&h), ["q2"]);
    let refused = enqueue(&h, &["q3"]);
    assert_eq!(
        (refused.status, refused.json()),
        (
            409,
            json!({"error": "asset-already-exists", "asset_id": "q3"})
        )
    );
    let launched = queue.tick().unwrap();
    drain(&h, &launched);
}

#[test]
fn queue_mutations_require_same_origin_json() {
    let h = queued(|_| {});
    let evil = [("Origin", "https://evil.test")];
    assert_eq!(
        h.client
            .post_json_with("/api/queue", &json!({"asset_ids": ["q1"]}), &evil)
            .status,
        403
    );
    assert_eq!(
        h.client
            .post_raw(
                "/api/queue",
                b"asset_ids=q1",
                &[("Content-Type", "text/plain")]
            )
            .status,
        415
    );
    assert_eq!(
        h.client
            .request("DELETE", "/api/queue/q1", &evil, None)
            .status,
        403
    );
    assert_eq!(
        queue(&h),
        json!({"running": null, "pending": [], "recent": []})
    );
}

fn launched_job_for(h: &Harness, asset: &str) -> String {
    let mut launched = None;
    support::wait_until(|| {
        launched = queue(h)["recent"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["asset_id"] == asset && row["outcome"] == "launched")
            .map(|row| row["job_id"].as_str().unwrap().to_string());
        launched.is_some()
    });
    launched.unwrap()
}

#[test]
fn queue_runner_thread_launches_without_manual_ticks() {
    let h = two_lines(|_| {});
    stage(&h.catalog, "thread1");
    assert_eq!(enqueue(&h, &["thread1"]).status, 202);
    let launched = launched_job_for(&h, "thread1");
    drain(&h, &launched);
    assert_eq!(pending(&h), Vec::<String>::new());
}

#[test]
fn catalog_valid_ids_outside_the_hls_rule_are_recorded_and_queueable() {
    let h = two_lines(|_| {});
    stage(&h.catalog, "_lead");
    let job_id = convert(&h, "_lead");
    drain(&h, &job_id);
    assert_eq!(
        names(&records(&h.output, "_lead")),
        [format!("job-{job_id}.json")]
    );
    assert_eq!(listed_assets(&h)[0], "_lead");
    assert_eq!(
        h.client.get(&format!("/api/jobs/{job_id}")).json(),
        succeeded()
    );
    h.app().queue.set_runner_mode(QueueRunner::Disabled);
    assert_eq!(enqueue(&h, &["_lead"]).status, 202);
    assert_eq!(h.client.delete("/api/queue/_lead").status, 204);
    assert_eq!(enqueue(&h, &["../x"]).status, 422);
}

#[test]
fn queue_runner_restarts_after_draining() {
    let h = two_lines(|_| {});
    for asset in ["again1", "again2"] {
        stage(&h.catalog, asset);
    }
    assert_eq!(enqueue(&h, &["again1"]).status, 202);
    let first = launched_job_for(&h, "again1");
    drain(&h, &first);
    support::wait_until(|| !h.app().queue.runner_active());
    assert_eq!(enqueue(&h, &["again2"]).status, 202);
    let second = launched_job_for(&h, "again2");
    drain(&h, &second);
}

fn latest_job(h: &Harness) -> String {
    queue(h)["recent"][0]["job_id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn queue_runner_slot_is_released_atomically_with_an_empty_queue() {
    let h = two_lines(|setup| setup.options.queue_runner = QueueRunner::Inert);
    for asset in ["s1", "s2", "s3"] {
        stage(&h.catalog, asset);
    }
    let queue = h.app().queue.clone();
    assert_eq!(enqueue(&h, &["s1"]).status, 202);
    assert!(
        queue.runners_started() == 1 && queue.runner_active(),
        "the first enqueue starts a runner"
    );
    assert_eq!(enqueue(&h, &["s2"]).status, 202);
    assert_eq!(queue.runners_started(), 1, "a live runner is reused");
    assert!(queue.run_once());
    drain(&h, &latest_job(&h));
    assert!(queue.run_once());
    drain(&h, &latest_job(&h));
    // The runner sees an empty queue and releases its slot in the same critical section ...
    assert!(!queue.run_once() && !queue.runner_active());
    // ... so an enqueue that lands right after it always gets a fresh runner: no stranded item.
    assert_eq!(enqueue(&h, &["s3"]).status, 202);
    assert!(queue.runners_started() == 2 && queue.runner_active());
    assert!(queue.run_once());
    drain(&h, &latest_job(&h));
    assert_eq!(listed_assets(&h), ["s3", "s2", "s1"]);
}

#[test]
fn a_long_catalog_id_is_recorded_listed_and_replayed_after_restart() {
    let h = two_lines(|_| {});
    let asset = "x".repeat(129);
    stage(&h.catalog, &asset);
    let job_id = convert(&h, &asset);
    drain(&h, &job_id);
    assert_eq!(listed_assets(&h)[0], asset);
    let (server, again) = h.second();
    let row = again.get("/api/hls/jobs").json()["jobs"][0].clone();
    assert_eq!(
        (
            row["asset_id"].clone(),
            row["kind"].clone(),
            row["state"].clone()
        ),
        (json!(asset), json!("convert"), json!("succeeded"))
    );
    let replayed = again.get(&format!("/api/jobs/{job_id}/log")).text();
    assert_eq!(event_names(&replayed), ["message", "message", "done"]);
    server.app.queue.set_runner_mode(QueueRunner::Disabled);
    assert_eq!(
        again
            .post_json("/api/queue", &json!({"asset_ids": [asset]}))
            .status,
        202
    );
}

#[test]
fn a_launched_conversion_is_answered_202_even_when_its_record_cannot_be_written() {
    if support::is_root() {
        return; // permissions are not enforced for root
    }
    let h = two_lines(|_| {});
    stage(&h.catalog, "norecord");
    let asset_dir = h.output.join("norecord");
    support::private_dir(&asset_dir).unwrap();
    set_mode(&asset_dir, 0o500);
    let job_id = convert(&h, "norecord");
    let text = drain(&h, &job_id);
    let status = h.client.get(&format!("/api/jobs/{job_id}")).json();
    set_mode(&asset_dir, 0o700);
    assert_eq!(records(&h.output, "norecord"), Vec::<PathBuf>::new());
    // The job itself is unaffected: it runs, its status and log answer from the manager.
    assert_eq!(sse_events(&text).last().unwrap().0, "done");
    assert_eq!(status, succeeded());
    assert_eq!(
        h.client.get("/api/catalog").json()[0]["output_readiness"]["state"],
        "unverified"
    );
    assert!(h.client.get("/api/assets/norecord").json()["new_url"].is_null());
    assert!(
        h.logs()
            .iter()
            .any(|line| line.contains("conversion-record-failed"))
    );
}

#[test]
fn a_record_write_that_fails_at_completion_is_retried_on_a_later_refresh() {
    if support::is_root() {
        return;
    }
    let h = two_lines(|setup| setup.options.monitor = false);
    stage(&h.catalog, "retry1");
    let job_id = convert(&h, "retry1");
    let reports = h.output.join("retry1").join("reports");
    set_mode(&reports, 0o500); // every terminal write fails until restored
    drain(&h, &job_id); // completion: the terminal write fails
    let on_disk = read_json(&records(&h.output, "retry1")[0]);
    set_mode(&reports, 0o700);
    assert_eq!(
        on_disk["state"], "running",
        "the failed write left the launch-time record"
    );
    assert_eq!(
        h.client.get(&format!("/api/jobs/{job_id}")).json(),
        succeeded()
    );
    let on_disk = read_json(&records(&h.output, "retry1")[0]);
    assert_eq!(
        (on_disk["state"].clone(), on_disk["exit_code"].clone()),
        (json!("succeeded"), json!(0)),
        "the next refresh retried the write"
    );
    let (_server, restarted) = h.second();
    assert_eq!(
        restarted.get(&format!("/api/jobs/{job_id}")).json(),
        succeeded()
    );
}

#[test]
fn a_queue_head_that_cannot_be_examined_is_skipped_with_a_reason_instead_of_blocking() {
    if support::is_root() {
        return;
    }
    let h = queued(|_| {});
    h.cli.hold();
    let queue = h.app().queue.clone();
    enqueue(&h, &["q1", "q2"]);
    set_mode(&h.catalog, 0o000);
    let ticked = queue.tick();
    set_mode(&h.catalog, 0o700);
    assert_eq!(ticked, None);
    let snapshot = queue_snapshot(&h);
    assert_eq!(pending(&h), ["q2"], "the bad head is removed");
    let recent = &snapshot["recent"][0];
    assert_eq!(
        (
            recent["asset_id"].clone(),
            recent["outcome"].clone(),
            recent["reason"].clone()
        ),
        (json!("q1"), json!("skipped"), json!("catalog-scan-failed"))
    );
    assert!(
        h.logs()
            .iter()
            .any(|line| line.contains("conversion-queue-skip") && line.contains("PermissionError"))
    );
    let launched = queue.tick().unwrap();
    assert_eq!(queue_snapshot(&h)["running"]["asset_id"], "q2");
    h.cli.release(0);
    drain(&h, &launched);
}

#[test]
fn queue_names_the_conflicting_asset_and_never_double_launches() {
    let h = queued(|_| {});
    h.cli.hold();
    let queue = h.app().queue.clone();
    enqueue(&h, &["q1"]);
    let first = queue.tick().unwrap();
    let response = enqueue(&h, &["q2", "q1"]);
    assert_eq!(
        (response.status, response.json()),
        (
            409,
            json!({"error": "asset-already-queued", "asset_id": "q1"})
        )
    );
    assert!(
        pending(&h).is_empty(),
        "an all-or-nothing request leaves the queue unchanged"
    );
    assert_eq!(
        enqueue(&h, &["zz"]).json(),
        json!({"error": "asset-not-found", "asset_id": "zz"})
    );
    release(&h, &first);
}

#[test]
fn queue_tick_does_not_scan_the_catalog_while_the_lease_is_taken() {
    let h = queued(|_| {});
    h.cli.hold();
    let queue = h.app().queue.clone();
    enqueue(&h, &["q1", "q2"]);
    let first = queue.tick().unwrap();
    std::fs::remove_dir_all(h.catalog.join("q2")).unwrap();
    assert_eq!(
        queue.tick(),
        None,
        "a busy lease is detected before any catalog work"
    );
    assert_eq!(pending(&h), ["q2"]);
    assert_eq!(queue_snapshot(&h)["recent"].as_array().unwrap().len(), 1);
    stage(&h.catalog, "q2");
    release(&h, &first);
    let second = queue.tick().unwrap();
    assert_eq!(queue_snapshot(&h)["running"]["asset_id"], "q2");
    release(&h, &second);
}

#[test]
fn runner_slot_stays_free_when_the_thread_cannot_start() {
    let h = two_lines(|setup| setup.options.queue_runner = QueueRunner::FailToStart);
    stage(&h.catalog, "t1");
    let queue = h.app().queue.clone();
    assert_eq!(enqueue(&h, &["t1"]).status, 202);
    assert!(
        !queue.runner_active(),
        "a runner that failed to start never holds the slot"
    );
    assert!(
        h.logs()
            .iter()
            .any(|line| line.contains("conversion-queue-runner-failed"))
    );
    queue.set_runner_mode(QueueRunner::Inert);
    assert_eq!(h.client.delete("/api/queue/t1").status, 204);
    assert_eq!(enqueue(&h, &["t1"]).status, 202);
    assert!(queue.runner_active() && queue.runners_started() == 1);
    assert!(queue.run_once());
    drain(&h, &latest_job(&h));
}

#[test]
fn platform_tolerates_malformed_profiles() {
    let h = Harness::with(|setup| {
        let profiles = json!({"default": {"policy": {"allowed_origins": "https://not-a-list", "allow_http": "yes"}}, "odd": {"policy": 5}});
        setup.options.hls_profiles =
            Some(write_profiles(&setup.base, "bad-profiles.json", &profiles));
    });
    let response = h.client.get("/api/platform");
    assert_eq!(response.status, 200);
    assert_eq!(
        response.json()["profiles"],
        json!([{"name": "default", "allowed_origins": [], "allow_http": false}, {"name": "odd", "allowed_origins": [], "allow_http": false}])
    );
}

#[test]
fn a_queue_launch_whose_record_fails_is_logged_and_still_runs() {
    if support::is_root() {
        return;
    }
    let h = queued(|_| {});
    let asset_dir = h.output.join("q1");
    support::private_dir(&asset_dir).unwrap();
    set_mode(&asset_dir, 0o500);
    enqueue(&h, &["q1"]);
    let launched = h.app().queue.tick();
    let state = launched
        .as_ref()
        .map(|job| h.client.get(&format!("/api/jobs/{job}")).json()["state"].clone());
    set_mode(&asset_dir, 0o700);
    let launched = launched.unwrap();
    assert!(
        h.logs()
            .iter()
            .any(|line| line.contains("conversion-record-failed"))
    );
    assert!(matches!(
        state.unwrap().as_str(),
        Some("running" | "succeeded")
    ));
    drain(&h, &launched);
}

#[test]
fn convert_requires_same_origin_like_every_other_mutation() {
    let h = Harness::new();
    stage(&h.catalog, "guard1");
    let body = json!({"asset_id": "guard1"});
    assert_eq!(
        h.client
            .post_json_with("/api/convert", &body, &[("Origin", "https://evil.test")])
            .status,
        403
    );
    assert_eq!(
        h.client
            .post_json_with("/api/convert", &body, &[("Sec-Fetch-Site", "cross-site")])
            .status,
        403
    );
    let plain = h.client.post_raw(
        "/api/convert",
        b"asset_id=guard1",
        &[("Content-Type", "text/plain")],
    );
    assert_eq!((plain.status, plain.json()), (415, error("json-required")));
    assert_eq!(h.cli.calls(), 0);
}

#[test]
fn duplicate_job_id_across_record_prefixes_resolves_to_the_hls_record() {
    let h = Harness::with(|setup| {
        let reports = setup.output.join("dup").join("reports");
        support::private_dir(&reports).unwrap();
        let id = "a".repeat(32);
        let common = json!({"schema_version": "1", "job_id": id, "asset_id": "dup", "state": "succeeded", "exit_code": 0,
                            "created_at": "2026-10-16T00:00:00+00:00", "hidden": false});
        let mut convert = common.clone();
        convert["kind"] = json!("convert");
        convert["analysis_id"] = Value::Null;
        convert["result_id"] = Value::Null;
        std::fs::write(reports.join(format!("job-{id}.json")), convert.to_string()).unwrap();
        let mut analysis = common;
        analysis["kind"] = json!("analysis");
        analysis["analysis_id"] = json!(id);
        analysis["result_id"] = Value::Null;
        std::fs::write(
            reports.join(format!("hls-job-{id}.json")),
            analysis.to_string(),
        )
        .unwrap();
    });
    let rows = h.client.get("/api/hls/jobs").json()["jobs"].clone();
    let rows: Vec<(Value, Value)> = rows
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["job_id"].clone(), row["kind"].clone()))
        .collect();
    assert_eq!(
        rows,
        [(json!("a".repeat(32)), json!("analysis"))],
        "the clear-HLS record wins"
    );
}

#[test]
fn read_log_refuses_symlinks_and_unsafe_ids() {
    let h = Harness::new();
    let jobs = h.output.join("_jobs");
    let manager = drmr_server::jobs::ConversionJobManager::new(
        &jobs,
        vec!["true".into()],
        drmr_server::jobs::process_env(),
    )
    .unwrap();
    let outside = h.root.path().join("outside.jsonl");
    std::fs::write(&outside, "{\"stage\": \"raw\", \"message\": \"leak\"}\n").unwrap();
    support::file_link(&outside, &jobs.join("link.jsonl"));
    assert_eq!(manager.read_log("link").count(), 0);
    assert_eq!(manager.read_log("../outside").count(), 0);
    assert_eq!(manager.read_log("").count(), 0);
    support::write_private(
        &jobs.join("plain.jsonl"),
        "{\"stage\": \"merge\", \"message\": \"ok\"}\nnot json\n",
    )
    .unwrap();
    let messages: Vec<String> = manager
        .read_log("plain")
        .map(|line| {
            serde_json::from_str::<Value>(&line).unwrap()["message"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(messages, ["ok", "not json"]);
}

#[test]
fn platform_origin_filter_rejects_userinfo_paths_and_wildcards() {
    let h = Harness::with(|setup| {
        let profiles = json!({"default": {"policy": {"allowed_origins": [
            "https://user:pw@media.example", "https://media.example/path", "*", "https://ok.example:8443", "http://127.0.0.1:8098"]}}});
        setup.options.hls_profiles = Some(write_profiles(&setup.base, "origins.json", &profiles));
    });
    let body = h.client.get("/api/platform").json();
    assert_eq!(
        body["profiles"][0]["allowed_origins"],
        json!(["https://ok.example:8443", "http://127.0.0.1:8098"])
    );
}

#[test]
fn queue_rejects_more_than_five_hundred_ids() {
    let h = queued(|_| {});
    let ids: Vec<String> = (0..501).map(|i| format!("q{i}")).collect();
    let answer = h.client.post_json("/api/queue", &json!({"asset_ids": ids}));
    assert_eq!(
        (answer.status, answer.json()),
        (422, error("queue-parameters-invalid"))
    );
    let five_hundred: Vec<String> = (0..500).map(|i| format!("q{i}")).collect();
    let answer = h
        .client
        .post_json("/api/queue", &json!({"asset_ids": five_hundred}));
    assert_eq!(
        answer.status, 404,
        "500 ids pass the bound (and then fail on an unknown id)"
    );
}

#[test]
fn catalog_rule_check_ids_in_the_page_are_exactly_the_servers() {
    let model = drmr_server::static_files::EMBEDDED
        .get_file("js/model.js")
        .unwrap()
        .contents_utf8()
        .unwrap();
    let start = model.find("var CATALOG_RULES").unwrap();
    let end = model.find("var HLS_RULES").unwrap();
    let pattern = regex::Regex::new(r"'(6-\d\.[a-z0-9.-]+)'").unwrap();
    let mut page: Vec<&str> = pattern
        .captures_iter(&model[start..end])
        .map(|c| c.get(1).unwrap().as_str())
        .collect();
    page.sort_unstable();
    page.dedup();
    let mut server: Vec<&str> = drmr_server::routes::report_checks().to_vec();
    server.sort_unstable();
    assert_eq!(
        page, server,
        "every catalog rule check is a report check and vice versa"
    );
}

#[test]
fn failed_record_writes_leave_no_temp_files_and_a_startup_rewrite_failure_keeps_the_record() {
    if support::is_root() {
        return;
    }
    let h = two_lines(|setup| setup.options.monitor = false);
    stage(&h.catalog, "tmp1");
    let job_id = convert(&h, "tmp1");
    let reports = h.output.join("tmp1").join("reports");
    let record = reports.join(format!("job-{job_id}.json"));
    std::fs::remove_file(&record).unwrap();
    support::private_dir(record.join("blocker")).unwrap();
    drain(&h, &job_id);
    for _ in 0..5 {
        h.client.get(&format!("/api/jobs/{job_id}")); // every refresh retries the write
    }
    let temporaries: Vec<String> = std::fs::read_dir(&reports)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| support::has_extension(name, "tmp"))
        .collect();
    assert!(
        temporaries.is_empty(),
        "a failed write never leaves its temp file behind: {temporaries:?}"
    );
    std::fs::remove_dir_all(&record).unwrap();
    assert_eq!(
        h.client.get(&format!("/api/jobs/{job_id}")).json(),
        succeeded()
    );
    assert_eq!(read_json(&record)["state"], "succeeded");
    // A record left "running" by a crash whose interrupted rewrite fails at startup is still loaded.
    let stale_id = "b".repeat(32);
    let stale = reports.join(format!("job-{stale_id}.json"));
    let body = json!({"schema_version": "1", "job_id": stale_id, "kind": "convert", "asset_id": "tmp1", "analysis_id": null,
                      "result_id": null, "state": "running", "exit_code": null, "created_at": "2026-10-16T00:00:00+00:00", "hidden": false});
    std::fs::write(&stale, body.to_string()).unwrap();
    set_mode(&reports, 0o500);
    let (_server, again) = h.second();
    let interrupted = again.get(&format!("/api/jobs/{stale_id}")).json();
    set_mode(&reports, 0o700);
    assert_eq!(
        interrupted,
        json!({"state": "interrupted", "exit_code": null})
    );
    again.get(&format!("/api/jobs/{stale_id}"));
    assert_eq!(
        read_json(&stale)["state"],
        "interrupted",
        "the rewrite is retried once writes work again"
    );
}

#[test]
fn the_queue_runner_survives_a_closed_stderr() {
    let h = two_lines(|setup| {
        setup.options.queue_runner = QueueRunner::Inert;
        setup.options.log_fails = true;
    });
    // Harness cleanup also terminates the held child if an assertion unwinds.
    h.cli.hold();
    for asset in ["q1", "q2"] {
        stage(&h.catalog, asset);
    }
    let queue = h.app().queue.clone();
    queue.enqueue(Some(&json!(["q1", "q2"]))).unwrap();
    assert!(queue.runner_active());
    std::fs::rename(&h.cli.script, h.cli.script.with_extension("disabled")).unwrap(); // q1 cannot be launched: a skip, logged
    assert!(queue.run_once(), "the iteration survives the failed write");
    std::fs::rename(h.cli.script.with_extension("disabled"), &h.cli.script).unwrap();
    assert_eq!(h.cli.calls(), 0, "q1 never entered the fake CLI");
    assert!(
        h.logs()
            .iter()
            .any(|line| line.contains("conversion-queue-skip")),
        "the skip record was written and failed"
    );
    assert!(queue.run_once());
    h.cli.wait_calls(1);
    assert_eq!(h.cli.calls(), 1, "only q2 reached the fake CLI");
    let snapshot = queue.snapshot();
    assert_eq!(snapshot["running"]["asset_id"], "q2", "{snapshot}");
    let job_id = snapshot["running"]["job_id"].as_str().unwrap();
    assert_eq!(snapshot["recent"][1]["asset_id"], "q1");
    assert_eq!(snapshot["recent"][1]["outcome"], "skipped");
    assert_eq!(snapshot["recent"][1]["reason"], "job-launch-failed");
    assert!(queue.runner_active(), "the slot is still the live runner's");
    assert_eq!(
        h.client.get("/api/queue").status,
        500,
        "the queue diagnostic failure becomes an HTTP error"
    );
    assert!(h.app().manager.is_busy(), "q2 remains held after the error");
    h.cli.release(0);
    support::wait_until(|| !h.app().manager.is_busy());
    let finished = h.app().manager.status(job_id).unwrap();
    assert_eq!(finished.state, "succeeded");
    assert_eq!(finished.exit_code, Some(0));
    assert!(!h.app().manager.is_busy());
}

#[test]
fn a_queue_launch_that_raises_unexpectedly_is_skipped() {
    let h = queued(|_| {});
    h.cli.hold();
    let queue = h.app().queue.clone();
    enqueue(&h, &["q1", "q2"]);
    std::fs::rename(&h.cli.script, h.cli.script.with_extension("disabled")).unwrap();
    let ticked = queue.tick();
    std::fs::rename(h.cli.script.with_extension("disabled"), &h.cli.script).unwrap();
    assert_eq!(ticked, None);
    let recent = &queue_snapshot(&h)["recent"][0];
    assert_eq!(
        (
            recent["asset_id"].clone(),
            recent["outcome"].clone(),
            recent["reason"].clone()
        ),
        (json!("q1"), json!("skipped"), json!("job-launch-failed"))
    );
    let launched = queue.tick().unwrap();
    assert_eq!(queue_snapshot(&h)["running"]["asset_id"], "q2");
    h.cli.release(0);
    drain(&h, &launched);
}

#[test]
fn tool_records_reach_the_page_verbatim_for_conversions_and_through_the_allowlist_for_hls() {
    let h = Harness::new();
    let command = format!(
        r#"{{"ts": "2026-10-16T00:00:00+00:00", "stage": "tool", "level": "info", "message": "command", "event": "command", "tool": "packager", "argv": ["packager", "in=/output/x/clear.mp4", "--keys", "label=:key_id={KID}:key=<redacted>"]}}"#
    );
    let output = r#"{"ts": "2026-10-16T00:00:01+00:00", "stage": "tool", "level": "info", "message": "tool-output", "event": "tool-output", "tool": "packager", "returncode": 0, "duration_s": 1.5, "stdout": "done", "stderr": "", "truncated": false}"#;
    let merge = r#"{"stage": "merge", "message": "merging", "rendition_id": "v0"}"#;
    h.cli.lines(&[&command, output, merge]);
    stage(&h.catalog, "tool1");
    let job_id = convert(&h, "tool1");
    let events = sse_events(&drain(&h, &job_id));
    let records: Vec<Value> = events
        .iter()
        .filter(|(name, _)| name == "message")
        .map(|(_, data)| serde_json::from_str(data).unwrap())
        .collect();
    let kinds: Vec<Value> = records
        .iter()
        .map(|r| r.get("event").cloned().unwrap_or(Value::Null))
        .collect();
    assert_eq!(kinds, [json!("command"), json!("tool-output"), Value::Null]);
    assert!(
        records[0]["argv"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_str()
            .unwrap()
            .ends_with("key=<redacted>")
    );
    // The download carries the same lines as a file.
    let response = h.client.get(&format!("/api/jobs/{job_id}/log.jsonl"));
    assert_eq!(response.status, 200);
    assert!(
        response
            .header("content-disposition")
            .unwrap()
            .ends_with(&format!("job-{job_id}.jsonl\""))
    );
    let lines: Vec<Value> = response
        .text()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let kinds: Vec<Value> = lines
        .iter()
        .map(|r| r.get("event").cloned().unwrap_or(Value::Null))
        .collect();
    assert_eq!(kinds, [json!("command"), json!("tool-output"), Value::Null]);
    assert_eq!(
        h.client
            .get(&format!("/api/jobs/{}/log.jsonl", "z".repeat(32)))
            .status,
        404
    );
}

#[test]
fn tool_records_pass_the_clear_hls_allowlist_with_bounded_fields() {
    let safe = |value: &Value| -> Value {
        serde_json::from_str(&drmr_server::safelog::safe_log(&value.to_string()).unwrap()).unwrap()
    };
    let command = safe(
        &json!({"stage": "tool", "event": "command", "tool": "ffprobe", "argv": ["ffprobe", "-i", "/output/a/seg.mp4", 5], "level": "info"}),
    );
    assert_eq!(
        command,
        json!({"stage": "tool", "event": "command", "message": "command", "level": "info", "tool": "ffprobe", "argv": []}),
        "a malformed argv is dropped, not echoed"
    );
    let long = safe(
        &json!({"stage": "tool", "event": "tool-output", "tool": "ffmpeg", "returncode": 1, "duration_s": 0.2,
                            "stdout": "x".repeat(300_000), "stderr": "boom", "level": "error", "ts": "2026-10-16T00:00:02+00:00"}),
    );
    assert_eq!(
        (
            long["tool"].clone(),
            long["returncode"].clone(),
            long["stderr"].clone(),
            long["truncated"].clone()
        ),
        (json!("ffmpeg"), json!(1), json!("boom"), json!(true))
    );
    assert!(long["stdout"].as_str().unwrap().chars().count() <= 256 * 1024 + 256);
    let weird = safe(
        &json!({"stage": "tool", "event": "tool-output", "tool": "curl", "stdout": "fetched https://origin.test/?token=PRIVATE ok", "call_id": 7}),
    );
    assert_eq!(
        (
            weird["tool"].clone(),
            weird["stdout"].clone(),
            weird["call_id"].clone()
        ),
        (json!("tool"), json!("fetched <url> ok"), json!(7)),
        "an unknown tool is named 'tool'; URL-shaped text never passes; the pairing id does"
    );
    let paired = safe(
        &json!({"stage": "tool", "event": "command", "tool": "packager", "argv": ["packager", "--hls_key_uri", "skd://secret", "in=/output/a/x.mp4"], "call_id": "7"}),
    );
    assert_eq!(
        paired["argv"],
        json!(["packager", "--hls_key_uri", "<url>", "in=/output/a/x.mp4"])
    );
    assert!(
        paired.get("call_id").is_none(),
        "a non-integer call_id is dropped"
    );
    assert_eq!(
        safe(&json!({"stage": "analysis", "event": "command", "argv": ["x"]}))["message"],
        "job-progress",
        "tool records are admitted only under the tool stage"
    );
    // The URL filter is linear on a long alphanumeric run and leaves a closing quote in place.
    let started = std::time::Instant::now();
    let long_run = safe(
        &json!({"stage": "tool", "event": "tool-output", "tool": "ffprobe", "stdout": format!("{} key=\"https://x.test/k?t=1\" end", "a".repeat(200_000))}),
    );
    assert!(started.elapsed().as_secs_f64() < 1.0);
    let stdout = long_run["stdout"].as_str().unwrap();
    assert!(stdout.ends_with(" key=\"<url>\" end") && stdout.starts_with("aaaa"));
}

#[test]
fn asset_renditions_carry_their_shape_for_the_input_table() {
    let h = Harness::new();
    let mut video = h264_rep("v0");
    video.segment_count = 3;
    video.width = Some(640);
    video.height = Some(360);
    let mut audio = audio_rep("a0");
    audio.segment_count = 4;
    audio.lang = Some("ja".into());
    stage_entry(
        &h.catalog,
        "shape1",
        &support::Stage {
            reps: vec![video, audio],
            ..support::Stage::default()
        },
    );
    let body = h.client.get("/api/assets/shape1").json();
    let rows = body["renditions"]["old"].as_array().unwrap();
    let row = |id: &str| rows.iter().find(|row| row["rep_id"] == id).unwrap().clone();
    let v0 = row("v0");
    assert_eq!(
        (
            v0["width"].clone(),
            v0["height"].clone(),
            v0["segment_count"].clone(),
            v0["bandwidth"].clone(),
            v0["container"].clone()
        ),
        (json!(640), json!(360), json!(3), json!(1), json!("fmp4")),
        "{v0}"
    );
    let a0 = row("a0");
    assert_eq!(
        (a0["language"].clone(), a0["segment_count"].clone()),
        (json!("ja"), json!(4)),
        "{a0}"
    );
    assert!(a0.get("width").is_none());
    let text = body
        .to_string()
        .to_lowercase()
        .replace("key_id", "")
        .replace("kid", "");
    assert!(!text.contains("key"));
}

#[test]
fn an_old_successful_manifest_is_not_current_generation_readiness() {
    let h = two_lines(|_| {});
    stage(&h.catalog, "stale-output");
    let manifest = h.output.join("stale-output/manifests/local/source.mpd");
    drmr_core::native::create_media_dir_all(manifest.parent().unwrap()).unwrap();
    drmr_core::native::create_media_new(&manifest)
        .unwrap()
        .write_all(b"<MPD/>")
        .unwrap();
    let before = std::fs::read(&manifest).unwrap();
    let row = h.client.get("/api/catalog").json()[0].clone();
    assert_eq!(row["converted"], true);
    assert_eq!(row["output_readiness"]["state"], "unverified");
    let id = convert(&h, "stale-output");
    drain(&h, &id);
    assert_eq!(h.client.get(&format!("/api/jobs/{id}")).json(), succeeded());
    let row = h.client.get("/api/catalog").json()[0].clone();
    assert_eq!(row["converted"], true);
    assert_eq!(row["output_readiness"]["state"], "unverified");
    assert!(h.client.get("/api/assets/stale-output").json()["new_url"].is_null());
    assert_eq!(std::fs::read(&manifest).unwrap(), before);
    let (_server, again) = h.second();
    assert_eq!(
        again.get("/api/catalog").json()[0]["output_readiness"]["state"],
        "unverified"
    );
}
