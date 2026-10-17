mod support;

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Barrier};
use support::{
    B_IV, B_KEY, B_KID, Harness, IV, KEY, KID, Stage, audio_rep, b64url, error, h264_rep, h265_rep,
    sse_events, stage, stage_entry, vp9_rep,
};

fn base_url_of(text: &str) -> String {
    let start = text.find("<BaseURL>").unwrap() + "<BaseURL>".len();
    text[start..start + text[start..].find("</BaseURL>").unwrap()].to_string()
}

fn initialization_of(text: &str) -> String {
    let start = text.find("initialization=\"").unwrap() + "initialization=\"".len();
    text[start..start + text[start..].find('"').unwrap()].to_string()
}

/// `urljoin(base, relative)` for a base ending in `/` and a relative path (all these tests need).
fn join(base: &str, relative: &str) -> String {
    assert!(base.ends_with('/') && !relative.starts_with('/'));
    format!("{base}{relative}")
}

// ---- the catalog report -------------------------------------------------------------------

#[test]
fn catalog_detail_report_requires_saved_checks_and_scrubs_private_fields() {
    for rendition_id in ["v0", "video:1"] {
        let h = Harness::new();
        stage(&h.catalog, "report-one");
        let asset = h.output.join("report-one");
        support::private_dir(asset.join("manifests/local")).unwrap();
        std::fs::write(asset.join("manifests/local/manifest.mpd"), "<MPD/>").unwrap();
        // An output manifest alone is not evidence that validation passed.
        assert_eq!(h.client.get("/api/reports/catalog/report-one").status, 410);
        support::private_dir(asset.join("reports")).unwrap();
        let report = json!({"schema_version": "1", "asset_id": "report-one", "all_pass": false,
            "full_enumeration": true, "round_trip_verified": false,
            "renditions": [{"rendition_id": rendition_id, "content_type": "video",
                "source_segment_count": 3, "output_segment_count": 3, "requested_segment_duration_s": 6,
                "clear_path": format!("/private/{KEY}"),
                "checks": {"6-1.boundary-agreement": "PASS", "6-4.framemd5": format!("SKIP: {KEY}"),
                           "6-1.iv-agreement": format!("FAIL: {IV}"), KEY: "PASS"}}],
            "manifests": {"checks": {"6-6.dash-conformance": "SKIP: no validator"}},
            "key": KEY, "error": IV});
        std::fs::write(
            asset.join("reports/phase1-summary.json"),
            report.to_string(),
        )
        .unwrap();
        let answer = h.client.get("/api/reports/catalog/report-one");
        assert_eq!(answer.status, 200);
        let expected = json!({"asset_id": "report-one", "report_kind": "catalog", "all_pass": false,
            "full_enumeration": true, "round_trip_verified": false, "production_verified": false,
            "tracks": [{"rep_id": rendition_id, "content_type": "video", "source_segment_count": 3,
                        "output_segment_count": 3, "requested_segment_duration_s": 6,
                        "checks": [{"id": "6-1.boundary-agreement", "status": "PASS"},
                                   {"id": "6-4.framemd5", "status": "SKIP"},
                                   {"id": "6-1.iv-agreement", "status": "FAIL"}]}],
            "checks": [{"id": "6-6.dash-conformance", "status": "SKIP"}],
            "dash_url": "/output/report-one/manifests/local/manifest.mpd", "hls_url": null});
        assert_eq!(answer.json(), expected);
        assert_eq!(answer.header("cache-control"), Some("no-store"));
        let text = answer.text();
        assert!(!text.contains(KEY) && !text.contains(IV) && !text.contains("/private"));
        assert_eq!(h.client.get("/reports/catalog/report-one").status, 200);
        let (_restarted, again) = h.second();
        assert_eq!(
            again.get("/api/reports/catalog/report-one").json(),
            expected
        );
        assert_eq!(h.client.get("/api/reports/catalog/unknown").status, 404);
        assert_eq!(h.client.get("/reports/catalog/unsafe%20id").status, 404);
    }
}

#[test]
fn catalog_detail_report_cannot_follow_report_symlink() {
    let h = Harness::new();
    stage(&h.catalog, "report-link");
    let reports = h.output.join("report-link/reports");
    support::private_dir(&reports).unwrap();
    let private = h.root.path().join("private-report.json");
    std::fs::write(&private, KEY).unwrap();
    support::file_link(&private, &reports.join("phase1-summary.json"));
    let answer = h.client.get("/api/reports/catalog/report-link");
    assert_eq!(answer.status, 410);
    assert!(!answer.text().contains(KEY));
}

#[test]
fn catalog_detail_report_rejects_malformed_shapes() {
    let malformed = json!({"schema_version": "1", "asset_id": "malformed", "all_pass": true,
        "full_enumeration": true, "round_trip_verified": true, "renditions": [], "manifests": {"checks": []}});
    for report in [Value::Null, json!([]), malformed] {
        let h = Harness::new();
        stage(&h.catalog, "malformed");
        let reports = h.output.join("malformed/reports");
        support::private_dir(&reports).unwrap();
        std::fs::write(reports.join("phase1-summary.json"), report.to_string()).unwrap();
        let answer = h.client.get("/api/reports/catalog/malformed");
        assert_eq!(answer.status, 410, "{report}");
        assert_eq!(answer.json(), error("result-unavailable"));
        assert_eq!(answer.header("cache-control"), Some("no-store"));
    }
}

// ---- /api/catalog -------------------------------------------------------------------------

#[test]
fn catalog_empty_when_no_entries() {
    let h = Harness::new();
    let answer = h.client.get("/api/catalog");
    assert_eq!((answer.status, answer.json()), (200, json!([])));
}

#[test]
fn catalog_lists_staged_entries_never_leaking_key_material() {
    let h = Harness::new();
    stage_entry(
        &h.catalog,
        "asset-a",
        &Stage {
            label: "Asset A".into(),
            ..Stage::default()
        },
    );
    stage_entry(
        &h.catalog,
        "asset-b",
        &Stage {
            label: "Asset B".into(),
            ..Stage::default()
        },
    );
    let answer = h.client.get("/api/catalog");
    assert_eq!(answer.status, 200);
    let body = answer.json();
    let rows: Vec<(String, String, bool, String)> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["asset_id"].as_str().unwrap().into(),
                row["label"].as_str().unwrap().into(),
                row["converted"].as_bool().unwrap(),
                row["entry"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("asset-a".into(), "Asset A".into(), false, "asset-a".into()),
            ("asset-b".into(), "Asset B".into(), false, "asset-b".into())
        ]
    );
    for row in body.as_array().unwrap() {
        let mut keys: Vec<&String> = row.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["asset_id", "converted", "entry", "label", "paths"]);
        assert!(support::has_extension(
            row["paths"]["mpd"].as_str().unwrap(),
            "mpd"
        ));
        assert!(support::has_extension(
            row["paths"]["hls_master"].as_str().unwrap(),
            "m3u8"
        ));
    }
    let raw = body.to_string();
    for leaked in [
        KID,
        KEY,
        IV,
        "key_hex",
        "iv_hex",
        "key_material",
        "keys.env",
        "keys_file",
    ] {
        assert!(!raw.contains(leaked), "{leaked}");
    }
    assert!(answer.text().ends_with("}]\n") && answer.text().starts_with("[{\"asset_id\":"));
}

fn stage_skippable_entries(catalog: &std::path::Path) -> [&'static str; 3] {
    stage(catalog, "good-one");
    let (bad_keys, _) = stage(catalog, "bad-keys");
    std::fs::write(
        bad_keys.join("keys.env"),
        format!("DRM_KID=0badc0de\nDRM_KEY={KEY}\nDRM_IV={IV}\n"),
    )
    .unwrap();
    support::private_dir(catalog.join("broken-json")).unwrap();
    std::fs::write(catalog.join("broken-json/asset.json"), "{not valid json").unwrap();
    ["0badc0de", KEY, IV]
}

#[test]
fn catalog_logs_each_skipped_entry_to_stderr_without_key_material() {
    let h = Harness::new();
    let secrets = stage_skippable_entries(&h.catalog);
    let listing = h.client.get("/api/catalog").json();
    let ids: Vec<&str> = listing
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["asset_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["good-one"]);
    let logs = h.logs();
    let records: Vec<Value> = logs
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let skipped: HashMap<String, Value> = records
        .iter()
        .filter(|record| record["event"] == "catalog-entry-skipped")
        .map(|record| {
            let name = std::path::Path::new(record["entry_dir"].as_str().unwrap())
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (name, record.clone())
        })
        .collect();
    let mut names: Vec<&String> = skipped.keys().collect();
    names.sort();
    assert_eq!(names, ["bad-keys", "broken-json"]);
    let reason = skipped["bad-keys"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("keys.env") && reason.contains("key_id_hex must decode to 16 bytes, got 4"),
        "{reason}"
    );
    assert!(
        skipped["broken-json"]["reason"]
            .as_str()
            .unwrap()
            .contains("invalid JSON")
    );
    let all = logs.join("\n");
    for secret in secrets {
        assert!(!all.contains(secret), "stderr leaked {secret}");
    }
}

#[test]
fn skipped_entries_are_logged_by_the_listing_only_not_every_lookup() {
    let h = Harness::new();
    stage_skippable_entries(&h.catalog);
    h.client.get("/clearkey/license?asset_id=good-one");
    h.client.get("/dash/old?asset_id=good-one");
    h.client.get("/api/assets/good-one");
    assert!(!h.logs().join("\n").contains("catalog-entry-skipped"));
}

#[test]
fn catalog_with_an_unreadable_descriptor_still_lists_the_valid_entries() {
    let h = Harness::new();
    stage(&h.catalog, "good-one");
    support::private_dir(h.catalog.join("binary-json")).unwrap();
    std::fs::write(
        h.catalog.join("binary-json/asset.json"),
        b"\xff\xfe\x00 not utf-8",
    )
    .unwrap();
    let answer = h.client.get("/api/catalog");
    assert_eq!(answer.status, 200);
    assert_eq!(answer.json().as_array().unwrap().len(), 1);
    assert_eq!(answer.json()[0]["asset_id"], "good-one");
}

// ---- conversion, job status, SSE log -------------------------------------------------------

#[test]
fn convert_unknown_asset_id_is_404() {
    let h = Harness::new();
    let answer = h
        .client
        .post_json("/api/convert", &json!({"asset_id": "does-not-exist"}));
    assert_eq!(
        (answer.status, answer.json()),
        (404, error("unknown asset_id"))
    );
}

#[test]
fn convert_launches_a_job_and_runs_to_completion() {
    let h = Harness::new();
    h.cli.convert();
    stage(&h.catalog, "job1");
    let answer = h
        .client
        .post_json("/api/convert", &json!({"asset_id": "job1"}));
    assert_eq!(answer.status, 202);
    let job_id = answer.json()["job_id"].as_str().unwrap().to_string();
    let stream = h.client.get(&format!("/api/jobs/{job_id}/log")).text();
    let status = h.client.get(&format!("/api/jobs/{job_id}")).json();
    assert!(matches!(
        status["state"].as_str(),
        Some("succeeded" | "failed")
    ));
    assert!(!status["exit_code"].is_null());
    let events = sse_events(&stream);
    assert_eq!(events.last().unwrap().0, "done");
    assert_eq!(
        serde_json::from_str::<Value>(&events.last().unwrap().1).unwrap(),
        status
    );
}

#[test]
fn convert_second_submit_while_running_is_409() {
    let h = Harness::new();
    h.cli.hold();
    stage(&h.catalog, "job2");
    let first = h
        .client
        .post_json("/api/convert", &json!({"asset_id": "job2"}));
    assert_eq!(first.status, 202);
    let second = h
        .client
        .post_json("/api/convert", &json!({"asset_id": "job2"}));
    assert_eq!(
        (second.status, second.json()),
        (409, error("a conversion job is already running"))
    );
    h.cli.release(0);
    h.client.get(&format!(
        "/api/jobs/{}/log",
        first.json()["job_id"].as_str().unwrap()
    ));
}

#[test]
fn job_status_and_log_of_an_unknown_job_id_are_404() {
    let h = Harness::new();
    for path in [
        "/api/jobs/does-not-exist",
        "/api/jobs/does-not-exist/log",
        "/api/jobs/does-not-exist/log.jsonl",
    ] {
        let answer = h.client.get(path);
        assert_eq!(
            (answer.status, answer.json()),
            (404, error("no such job")),
            "{path}"
        );
    }
}

#[test]
fn job_log_stream_ends_with_a_done_event_matching_the_job_status() {
    for (code, state) in [(0, "succeeded"), (1, "failed"), (2, "failed")] {
        let h = Harness::new();
        stage(&h.catalog, "sse1");
        h.cli.lines(&[
            r#"{"stage": "fake", "message": "one"}"#,
            r#"{"stage": "fake", "message": "two"}"#,
        ]);
        h.cli.exit_code(code);
        let job_id = h
            .client
            .post_json("/api/convert", &json!({"asset_id": "sse1"}))
            .json()["job_id"]
            .as_str()
            .unwrap()
            .to_string();
        let answer = h.client.get(&format!("/api/jobs/{job_id}/log"));
        assert_eq!(
            answer.header("content-type"),
            Some("text/event-stream; charset=utf-8")
        );
        let text = answer.text();
        assert!(
            text.ends_with(&format!(
                "event: done\ndata: {{\"state\": \"{state}\", \"exit_code\": {code}}}\n\n"
            )),
            "{text}"
        );
        let events = sse_events(&text);
        assert_eq!(
            events.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(),
            ["message", "message", "done"]
        );
        let messages: Vec<String> = events[..2]
            .iter()
            .map(|e| {
                serde_json::from_str::<Value>(&e.1).unwrap()["message"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(messages, ["one", "two"]);
        let done: Value = serde_json::from_str(&events[2].1).unwrap();
        assert_eq!(done, json!({"state": state, "exit_code": code}));
        assert_eq!(done, h.client.get(&format!("/api/jobs/{job_id}")).json());
    }
}

#[test]
fn job_log_stream_for_a_job_with_no_output_still_ends_with_done() {
    let h = Harness::new();
    stage(&h.catalog, "sse2");
    let job_id = h
        .client
        .post_json("/api/convert", &json!({"asset_id": "sse2"}))
        .json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    let text = h.client.get(&format!("/api/jobs/{job_id}/log")).text();
    assert_eq!(
        text,
        "event: done\ndata: {\"state\": \"succeeded\", \"exit_code\": 0}\n\n"
    );
}

#[test]
fn job_log_of_a_superseded_job_still_ends_with_its_own_done_event() {
    let h = Harness::new();
    stage(&h.catalog, "sse3");
    h.cli.exit_code(1);
    let first = h
        .client
        .post_json("/api/convert", &json!({"asset_id": "sse3"}))
        .json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    h.client.get(&format!("/api/jobs/{first}/log"));
    let second = h
        .client
        .post_json("/api/convert", &json!({"asset_id": "sse3"}))
        .json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(second, first);
    let text = h.client.get(&format!("/api/jobs/{first}/log")).text();
    assert!(
        text.ends_with("event: done\ndata: {\"state\": \"failed\", \"exit_code\": 1}\n\n"),
        "{text}"
    );
    assert_eq!(
        h.client.get(&format!("/api/jobs/{first}")).json(),
        json!({"state": "failed", "exit_code": 1})
    );
    h.client.get(&format!("/api/jobs/{second}/log"));
}

#[test]
fn convert_is_race_free_under_concurrent_submits() {
    let h = Harness::new();
    h.cli.hold();
    stage(&h.catalog, "race");
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let client = h.client.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                client.post_json("/api/convert", &json!({"asset_id": "race"}))
            })
        })
        .collect();
    let answers: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let mut statuses: Vec<u16> = answers.iter().map(|answer| answer.status).collect();
    statuses.sort_unstable();
    assert_eq!(statuses, [202, 409, 409, 409, 409, 409, 409, 409]);
    h.cli.wait_calls(1);
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(h.cli.calls(), 1, "exactly one subprocess ever spawned");
    let winner = answers
        .iter()
        .find(|answer| answer.status == 202)
        .unwrap()
        .json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    h.cli.release(0);
    assert_eq!(
        h.wait_job(&winner),
        json!({"state": "succeeded", "exit_code": 0})
    );
}

// ---- /source ------------------------------------------------------------------------------

#[test]
fn source_serves_a_file_under_download() {
    let h = Harness::new();
    let (entry, mpd) = stage(&h.catalog, "src1");
    let rel = mpd
        .strip_prefix(entry.join("download"))
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(h.client.get(&format!("/source/src1/{rel}")).status, 200);
}

#[test]
fn source_rejects_traversal() {
    let h = Harness::new();
    stage(&h.catalog, "src2");
    let answer = h.client.get("/source/src2/../asset.json");
    assert_eq!((answer.status, answer.json()), (404, error("not found")));
}

#[test]
fn source_never_serves_keys_env_or_asset_json() {
    let h = Harness::new();
    stage(&h.catalog, "src3");
    for rel in ["keys.env", "asset.json", "../keys.env", "../asset.json"] {
        assert_eq!(
            h.client.get(&format!("/source/src3/{rel}")).status,
            404,
            "{rel}"
        );
    }
}

#[test]
fn source_rejects_catalog_with_keys_under_download() {
    for through_symlink in [false, true] {
        let h = Harness::new();
        let (entry, _) = stage(&h.catalog, "asset-a");
        let keys = entry.join("download/secret.env");
        std::fs::rename(entry.join("keys.env"), &keys).unwrap();
        let mut descriptor: Value =
            serde_json::from_slice(&std::fs::read(entry.join("asset.json")).unwrap()).unwrap();
        if through_symlink {
            support::file_link(&keys, &entry.join("keys.env"));
        } else {
            descriptor["keys_file"] = json!("download/secret.env");
        }
        std::fs::write(entry.join("asset.json"), descriptor.to_string()).unwrap();
        assert_eq!(h.client.get("/api/catalog").json(), json!([]));
        let answer = h.client.get("/source/asset-a/secret.env");
        assert_eq!(answer.status, 404);
        assert!(!answer.text().contains(KEY) && !answer.text().contains(IV));
    }
}

#[test]
fn source_unknown_asset_id_is_404() {
    let h = Harness::new();
    assert_eq!(
        h.client.get("/source/does-not-exist/whatever.mpd").status,
        404
    );
}

#[test]
fn source_serves_old_segments_when_the_entry_directory_name_differs_from_the_asset_id() {
    let h = Harness::new();
    let (entry, mpd) = stage_entry(
        &h.catalog,
        "alpha",
        &Stage {
            dir_name: Some("zzz-dir".into()),
            ..Stage::default()
        },
    );
    assert_eq!(entry.file_name().unwrap(), "zzz-dir");
    assert!(!h.catalog.join("alpha").exists());
    let manifest = h.client.get("/dash/old?asset_id=alpha");
    assert_eq!(manifest.status, 200);
    let text = manifest.text();
    let base = base_url_of(&text);
    assert!(base.starts_with("/source/alpha/"), "{base}");
    let init = h.client.get(&join(&base, &initialization_of(&text)));
    assert_eq!(
        (init.status, init.body.as_slice()),
        (200, b"INIT:v0".as_slice())
    );
    let rel = mpd
        .strip_prefix(entry.join("download"))
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(h.client.get(&format!("/source/alpha/{rel}")).status, 200);
    for blocked in ["keys.env", "asset.json", "../keys.env", "../asset.json"] {
        assert_eq!(
            h.client.get(&format!("/source/alpha/{blocked}")).status,
            404,
            "{blocked}"
        );
    }
    assert_eq!(
        h.client.get(&format!("/source/zzz-dir/{rel}")).status,
        404,
        "a directory name is not an id"
    );
}

#[test]
fn old_manifest_host_only_base_keeps_hostname_when_resolving_segments() {
    let h = Harness::new();
    let (entry, mpd) = stage(&h.catalog, "host-only");
    let old_base = "https://cdn.example.test/host-only/";
    let text = std::fs::read_to_string(&mpd).unwrap();
    assert!(text.contains(old_base));
    std::fs::write(&mpd, text.replace(old_base, "https://cdn.example.test")).unwrap();
    let cdn = entry.join("download/cdn.example.test");
    for content_type in ["video", "audio"] {
        std::fs::rename(
            cdn.join("host-only").join(content_type),
            cdn.join(content_type),
        )
        .unwrap();
    }
    let text = h.client.get("/dash/old?asset_id=host-only").text();
    let init = h
        .client
        .get(&join(&base_url_of(&text), &initialization_of(&text)));
    assert_eq!(
        (init.status, init.body.as_slice()),
        (200, b"INIT:v0".as_slice())
    );
}

// ---- /output ------------------------------------------------------------------------------

fn write_output_tree(output: &std::path::Path, asset_id: &str) {
    for (rel, data) in [
        ("cbcs/video/v0/init.mp4", b"CBCS-INIT".as_slice()),
        ("cbcs/video/v0/seg_1.m4s", b"CBCS-SEG"),
        ("manifests/local/host/title/dash/manifest.mpd", b"<MPD/>"),
        ("clear/video/v0/clear.mp4", b"DECRYPTED-CLEAR-MEDIA"),
        ("_work/v0/source_merged.mp4", b"MERGED-SOURCE"),
        ("reports/report.json", b"{}"),
    ] {
        let target = output.join(asset_id).join(rel);
        support::private_dir(target.parent().unwrap()).unwrap();
        std::fs::write(target, data).unwrap();
    }
}

#[test]
fn output_rejects_traversal_and_unsafe_ids() {
    let h = Harness::new();
    assert_eq!(
        h.client.get("/output/anything/../../etc/passwd").status,
        404
    );
    for bad in ["..", ".", "a/b"] {
        assert_eq!(
            h.client.get(&format!("/output/{bad}/manifest.mpd")).status,
            404,
            "{bad}"
        );
    }
}

#[cfg(windows)]
#[test]
fn output_does_not_serve_alternate_data_streams() {
    let h = Harness::new();
    write_output_tree(&h.output, "streams");
    std::fs::write(
        h.output.join("streams/cbcs/video/v0/init.mp4:secret"),
        b"PRIVATE-STREAM",
    )
    .unwrap();
    let answer = h
        .client
        .get("/output/streams/cbcs/video/v0/init.mp4:secret");
    assert_eq!(answer.status, 404);
    assert!(!answer.text().contains("PRIVATE-STREAM"));
}

#[test]
fn output_serves_only_the_cbcs_and_manifests_subtrees() {
    let h = Harness::new();
    write_output_tree(&h.output, "out2");
    for (rel, body) in [
        ("cbcs/video/v0/init.mp4", b"CBCS-INIT".as_slice()),
        ("cbcs/video/v0/seg_1.m4s", b"CBCS-SEG"),
        ("manifests/local/host/title/dash/manifest.mpd", b"<MPD/>"),
    ] {
        let answer = h.client.get(&format!("/output/out2/{rel}"));
        assert_eq!(
            (answer.status, answer.body.as_slice()),
            (200, body),
            "{rel}"
        );
    }
    for rel in [
        "clear/video/v0/clear.mp4",
        "_work/v0/source_merged.mp4",
        "reports/report.json",
    ] {
        let answer = h.client.get(&format!("/output/out2/{rel}"));
        assert_eq!(answer.status, 404, "{rel}");
        assert!(!answer.text().contains("DECRYPTED") && !answer.text().contains("MERGED"));
    }
}

#[test]
fn output_subtree_check_cannot_be_bypassed_by_a_traversal_spelling() {
    let h = Harness::new();
    write_output_tree(&h.output, "out3");
    for rel in [
        "cbcs/../clear/video/v0/clear.mp4",
        "manifests/../_work/v0/source_merged.mp4",
        "cbcs/%2e%2e/clear/video/v0/clear.mp4",
        "./clear/video/v0/clear.mp4",
        "cbcs/video/v0/../../../clear/video/v0/clear.mp4",
    ] {
        let answer = h.client.get(&format!("/output/out3/{rel}"));
        assert_eq!(answer.status, 404, "{rel}");
        assert!(!answer.text().contains("DECRYPTED") && !answer.text().contains("MERGED"));
    }
}

#[test]
fn output_never_follows_a_symlink_from_cbcs_into_the_clear_copy() {
    let h = Harness::new();
    write_output_tree(&h.output, "out4");
    #[cfg(unix)]
    let request = {
        support::file_link(
            &h.output.join("out4/clear/video/v0/clear.mp4"),
            &h.output.join("out4/cbcs/video/v0/leak.mp4"),
        );
        "/output/out4/cbcs/video/v0/leak.mp4"
    };
    #[cfg(windows)]
    let request = {
        support::directory_link(
            &h.output.join("out4/clear/video/v0"),
            &h.output.join("out4/cbcs/video/v0/leak"),
        );
        "/output/out4/cbcs/video/v0/leak/clear.mp4"
    };
    let answer = h.client.get(request);
    assert_eq!(answer.status, 404);
    assert!(!answer.text().contains("DECRYPTED"));
}

#[test]
fn output_never_serves_job_logs_through_a_jobs_asset_id() {
    let h = Harness::new();
    std::fs::write(h.output.join("_jobs/abc123.jsonl"), "{\"stage\": \"x\"}\n").unwrap();
    assert_eq!(h.client.get("/output/_jobs/abc123.jsonl").status, 404);
}

#[test]
fn output_serves_a_file_that_exists() {
    let h = Harness::new();
    support::private_dir(h.output.join("out1/manifests/local")).unwrap();
    std::fs::write(h.output.join("out1/manifests/local/manifest.mpd"), "<MPD/>").unwrap();
    let answer = h.client.get("/output/out1/manifests/local/manifest.mpd");
    assert_eq!(answer.status, 200);
    assert_eq!(
        answer.header("content-type"),
        Some("application/dash+xml; charset=utf-8")
    );
}

// ---- /dash/old ----------------------------------------------------------------------------

#[test]
fn dash_old_rewrites_baseurl_onto_source_route() {
    let h = Harness::new();
    stage(&h.catalog, "old1");
    let answer = h.client.get("/dash/old?asset_id=old1");
    assert_eq!(answer.status, 200);
    assert!(base_url_of(&answer.text()).starts_with("/source/old1/"));
}

#[test]
fn dash_old_unknown_asset_id_is_404() {
    let h = Harness::new();
    assert_eq!(
        h.client.get("/dash/old?asset_id=does-not-exist").status,
        404
    );
}

// ---- /clearkey/license --------------------------------------------------------------------

#[test]
fn clearkey_license_returns_the_right_asset_key() {
    let h = Harness::new();
    stage(&h.catalog, "ck1");
    let answer = h.client.get("/clearkey/license?asset_id=ck1");
    assert_eq!(answer.status, 200);
    assert_eq!(answer.header("cache-control"), Some("no-store"));
    let body = answer.json();
    assert_eq!(body["type"], "temporary");
    assert_eq!(
        body["keys"],
        json!([{"kty": "oct", "kid": b64url(KID), "k": b64url(KEY)}])
    );
    assert_eq!(
        answer.text(),
        format!(
            "{{\"keys\": [{{\"kty\": \"oct\", \"k\": \"{}\", \"kid\": \"{}\"}}], \"type\": \"temporary\"}}",
            b64url(KEY),
            b64url(KID)
        )
    );
    for encoded in [b64url(KID), b64url(KEY)] {
        assert!(!encoded.contains('=') && !encoded.contains('+') && !encoded.contains('/'));
    }
}

fn stage_two(catalog: &std::path::Path) {
    stage(catalog, "ck-a");
    stage_entry(
        catalog,
        "ck-b",
        &Stage {
            kid: B_KID.into(),
            key: B_KEY.into(),
            iv: B_IV.into(),
            ..Stage::default()
        },
    );
}

#[test]
fn clearkey_license_serves_each_assets_own_key_and_never_the_others() {
    let h = Harness::new();
    stage_two(&h.catalog);
    let a = h.client.get("/clearkey/license?asset_id=ck-a");
    let b = h.client.get("/clearkey/license?asset_id=ck-b");
    assert_eq!(
        a.json()["keys"],
        json!([{"kty": "oct", "kid": b64url(KID), "k": b64url(KEY)}])
    );
    assert_eq!(
        b.json()["keys"],
        json!([{"kty": "oct", "kid": b64url(B_KID), "k": b64url(B_KEY)}])
    );
    let (a_raw, b_raw) = (a.text(), b.text());
    assert!(!a_raw.contains(&b64url(B_KID)) && !a_raw.contains(&b64url(B_KEY)));
    assert!(!b_raw.contains(&b64url(KID)) && !b_raw.contains(&b64url(KEY)));
    for raw in [&a_raw, &b_raw] {
        for hex_value in [KID, KEY, IV, B_KID, B_KEY, B_IV] {
            assert!(!raw.contains(hex_value));
        }
    }
}

#[test]
fn clearkey_license_post_challenge_cannot_select_another_assets_key() {
    let h = Harness::new();
    stage_two(&h.catalog);
    let answer = h.client.post_json(
        "/clearkey/license?asset_id=ck-a",
        &json!({"kids": [b64url(B_KID)], "type": "temporary"}),
    );
    assert_eq!(answer.status, 200);
    assert_eq!(answer.header("cache-control"), Some("no-store"));
    assert_eq!(
        answer.json()["keys"],
        json!([{"kty": "oct", "kid": b64url(KID), "k": b64url(KEY)}])
    );
    assert!(!answer.text().contains(&b64url(B_KEY)));
}

#[test]
fn clearkey_license_unknown_asset_id_is_404() {
    let h = Harness::new();
    let answer = h.client.get("/clearkey/license?asset_id=does-not-exist");
    assert_eq!(
        (answer.status, answer.json()),
        (404, error("unknown asset_id"))
    );
}

// ---- /api/assets --------------------------------------------------------------------------

#[test]
fn api_assets_old_url_always_present_new_url_none_before_conversion() {
    let h = Harness::new();
    stage(&h.catalog, "urls1");
    let body = h.client.get("/api/assets/urls1").json();
    assert_eq!(body["old_url"], "/dash/old?asset_id=urls1");
    assert!(body["new_url"].is_null());
}

#[test]
fn api_assets_unknown_asset_id_is_404() {
    let h = Harness::new();
    assert_eq!(h.client.get("/api/assets/does-not-exist").status, 404);
}

#[test]
fn api_assets_old_summary_reflects_the_source_manifest() {
    let h = Harness::new();
    stage(&h.catalog, "sum1");
    let body = h.client.get("/api/assets/sum1").json();
    assert_eq!(body["old_summary"]["protection_scheme"], "cenc");
    assert_eq!(body["old_summary"]["playready_version"], "4.3.0.0");
    assert_eq!(body["old_summary"]["playready_algid"], "AESCBC");
    assert_eq!(
        body["old_summary"]["drm_systems"],
        json!(["PlayReady", "Widevine"])
    );
    assert!(body["new_summary"].is_null());
}

fn convert(h: &Harness, asset_id: &str) {
    h.cli.convert();
    let job_id = h
        .client
        .post_json("/api/convert", &json!({"asset_id": asset_id}))
        .json()["job_id"]
        .as_str()
        .unwrap()
        .to_string();
    h.client.get(&format!("/api/jobs/{job_id}/log"));
}

#[test]
fn api_assets_new_summary_shows_cbcs_after_conversion() {
    let h = Harness::new();
    stage(&h.catalog, "sum2");
    convert(&h, "sum2");
    let body = h.client.get("/api/assets/sum2").json();
    assert_eq!(body["new_summary"]["protection_scheme"], "cbcs");
    assert_eq!(
        body["new_summary"]["default_kid"],
        body["old_summary"]["default_kid"]
    );
}

#[test]
fn api_assets_renditions_lists_in_scope_representations_in_manifest_order() {
    let h = Harness::new();
    stage_entry(
        &h.catalog,
        "rend1",
        &Stage {
            reps: vec![
                h264_rep("v0"),
                h265_rep("h0"),
                audio_rep("a0"),
                vp9_rep("p0"),
            ],
            ..Stage::default()
        },
    );
    let body = h.client.get("/api/assets/rend1").json();
    let identity: Vec<Value> = body["renditions"]["old"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| json!({"rep_id": row["rep_id"], "content_type": row["content_type"], "codec": row["codec"]}))
        .collect();
    assert_eq!(
        identity,
        [
            json!({"rep_id": "v0", "content_type": "video", "codec": "avc1.42C00C"}),
            json!({"rep_id": "h0", "content_type": "video", "codec": "hvc1.1.6.L60.90"}),
            json!({"rep_id": "a0", "content_type": "audio", "codec": "mp4a.40.2"}),
        ]
    );
    assert!(
        body["renditions"]["new"].is_null(),
        "null (not []) before a conversion"
    );
}

#[test]
fn api_assets_renditions_new_describes_the_converted_manifest() {
    let h = Harness::new();
    stage(&h.catalog, "rend2");
    convert(&h, "rend2");
    let body = h.client.get("/api/assets/rend2").json();
    let renditions = &body["renditions"];
    let ids: Vec<&str> = renditions["new"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["rep_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["v0", "a0"]);
    assert_eq!(renditions["new"], renditions["old"]);
    let first = renditions["new"][0].as_object().unwrap();
    for key in [
        "rep_id",
        "content_type",
        "codec",
        "container",
        "segment_count",
        "bandwidth",
    ] {
        assert!(first.contains_key(key), "{key}");
    }
    assert!(!first.contains_key("key"));
}

#[test]
fn api_assets_renditions_survive_an_unparseable_manifest() {
    let h = Harness::new();
    let (_, mpd) = stage(&h.catalog, "rend3");
    for garbage in [b"<MPD".as_slice(), b"\xff\xfe not utf-8 \x00".as_slice()] {
        std::fs::write(&mpd, garbage).unwrap();
        let answer = h.client.get("/api/assets/rend3");
        assert_eq!(answer.status, 200);
        let body = answer.json();
        assert_eq!(body["renditions"], json!({"old": [], "new": null}));
        assert!(body["old_summary"].is_null());
    }
}

#[test]
fn api_assets_writes_a_bandwidth_beyond_64_bits_as_its_digits() {
    let h = Harness::new();
    let (_, mpd) = stage(&h.catalog, "big1");
    let text = std::fs::read_to_string(&mpd).unwrap();
    let wide = text.replacen("bandwidth=\"1\"", "bandwidth=\"99999999999999999999\"", 1);
    assert_ne!(wide, text, "the staged manifest carries a bandwidth");
    std::fs::write(&mpd, wide).unwrap();
    let answer = h.client.get("/api/assets/big1");
    assert_eq!(answer.status, 200);
    let body = answer.text();
    assert!(
        body.contains("\"bandwidth\":99999999999999999999,"),
        "{body}"
    );
    assert!(
        !body.contains("int:"),
        "no marker reaches the client: {body}"
    );
}

// ---- a synthetic round trip ---------------------------------------------------------------

#[test]
fn full_round_trip_convert_then_serve_the_new_manifest() {
    let h = Harness::new();
    stage(&h.catalog, "rt1");
    h.cli.convert();
    let answer = h
        .client
        .post_json("/api/convert", &json!({"asset_id": "rt1"}));
    assert_eq!(answer.status, 202);
    let job_id = answer.json()["job_id"].as_str().unwrap().to_string();
    h.client.get(&format!("/api/jobs/{job_id}/log"));
    let status = h.client.get(&format!("/api/jobs/{job_id}")).json();
    assert!(matches!(
        status["state"].as_str(),
        Some("succeeded" | "failed")
    ));
    let catalog = h.client.get("/api/catalog").json();
    assert_eq!(catalog[0]["converted"], json!(true));
    let assets = h.client.get("/api/assets/rt1").json();
    let new_url = assets["new_url"].as_str().unwrap().to_string();
    assert!(
        new_url.starts_with("/output/rt1/manifests/local/"),
        "{new_url}"
    );
    let manifest = h.client.get(&new_url);
    assert_eq!(manifest.status, 200);
    let text = manifest.text();
    assert!(text.contains("value=\"cbcs\""));
    // The <BaseURL> must end in "/" (RFC 3986 §5.2.3), resolved the way a player resolves it.
    let base = base_url_of(&text);
    assert!(base.ends_with('/'), "{base}");
    let resolved = join(&base, &initialization_of(&text));
    assert!(resolved.starts_with("/output/rt1/cbcs/"), "{resolved}");
    assert_eq!(h.client.get(&resolved).status, 200);
}

// ---- the static pages ---------------------------------------------------------------------

#[test]
fn every_static_reference_of_both_pages_resolves_locally() {
    let h = Harness::new();
    let pattern = regex::Regex::new(r#"(?:src|href)="(/static/[^"]+)""#).unwrap();
    let mut refs: Vec<String> = Vec::new();
    for page in ["/", "/static/report.html"] {
        let html = h.client.get(page).text();
        refs.extend(pattern.captures_iter(&html).map(|c| c[1].to_string()));
    }
    let css = h.client.get("/static/app.css").text();
    let url = regex::Regex::new(r#"url\("(/static/[^"]+)"\)"#).unwrap();
    refs.extend(url.captures_iter(&css).map(|c| c[1].to_string()));
    refs.sort();
    refs.dedup();
    assert!(
        refs.iter().any(|r| support::has_extension(r, "woff2"))
            && refs.iter().any(|r| r.ends_with("shaka-player.compiled.js"))
    );
    assert!(
        !refs
            .iter()
            .any(|r| r[1..].contains("//") || r.starts_with("http")),
        "{refs:?}"
    );
    for reference in &refs {
        assert_eq!(h.client.get(reference).status, 200, "{reference}");
    }
}
