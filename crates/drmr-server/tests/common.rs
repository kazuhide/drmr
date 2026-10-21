mod support;

use drmr_server::{ServerConfig, spawn};
use serde_json::Value;
use support::{Client, stage};

#[test]
fn capabilities_issue_role_bound_refs_without_host_paths() {
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    let output = root.path().join("output");
    std::fs::create_dir(&catalog).unwrap();
    let mut config = ServerConfig::new(&catalog, &output);
    config.bind_port = 0;
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let answer = client.get("/api/common/capabilities");
    assert_eq!(answer.status, 200, "{}", answer.text());
    let value = answer.json();
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["limits"]["sessions"], 8);
    assert_eq!(value["limits"]["requests_per_session"], 32);
    assert!(!answer.text().contains(root.path().to_str().unwrap()));
    let roots = value["roots"].as_array().unwrap();
    assert_ne!(roots.as_slice(), &[] as &[Value]);
    for item in roots {
        assert_eq!(item["root_ref"].as_str().unwrap().len(), 32);
        assert!(item["roles"].as_array().is_some());
    }
    assert_eq!(answer.header("cache-control"), Some("no-store"));
    server.stop().unwrap();
}

#[test]
fn nested_output_jobs_are_never_served_through_legacy_source() {
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    let (entry, _) = stage(&catalog, "asset");
    let output = entry.join("download/results");
    let mut config = ServerConfig::new(&catalog, &output);
    config.bind_port = 0;
    let server = spawn(config).unwrap();
    let marker = "COMMON_PRIVATE_SOURCE_MARKER";
    support::write_private(&output.join("_jobs/marker.json"), marker).unwrap();
    let client = Client::new(&server.url());
    let answer = client.get("/source/asset/results/_jobs/marker.json");
    assert_eq!(answer.status, 404, "private marker reached HTTP");
    assert!(!answer.text().contains(marker));
    server.stop().unwrap();
}

#[test]
fn common_control_bodies_are_strict_at_every_depth() {
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    let mut config = ServerConfig::new(&catalog, root.path().join("output"));
    config.bind_port = 0;
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let cap = client.get("/api/common/capabilities");
    assert_eq!(cap.status, 200);
    let sid = cap.json()["server_id"].as_str().unwrap().to_owned();
    for raw in [
        format!(
            r#"{{"schema_version":1,"server_id":"{sid}","creation_request_id":"{}","source":{{"kind":"local","input_ref":"x","input_ref":"y"}}}}"#,
            "a".repeat(32)
        ),
        format!(
            r#"{{"schema_version":1,"server_id":"{sid}","creation_request_id":"{}","source":{{"kind":"local","input_ref":"x","path":"/private"}}}}"#,
            "a".repeat(32)
        ),
        format!(
            r#"{{"schema_version":1,"server_id":"{sid}","creation_request_id":"{}","source":{{"kind":"hls-url","url":"https://example.invalid/a"}}}}"#,
            "a".repeat(32)
        ),
    ] {
        let reply = client.post_raw(
            "/api/common/sessions",
            raw.as_bytes(),
            &[("Content-Type", "application/json")],
        );
        assert_eq!(reply.status, 400, "{}", reply.text());
        assert!(reply.json()["error"]["code"].as_str().is_some());
    }
    let sessions: Value = client.get("/api/common/sessions").json();
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 0);
    server.stop().unwrap();
}

#[test]
fn url_sessions_never_return_credentials_or_unbounded_source_labels() {
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    let mut config = ServerConfig::new(&catalog, root.path().join("output"));
    config.bind_port = 0;
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let run = client.get("/api/common/capabilities").json()["server_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let sensitive = "https://PRIVATE_USER:PRIVATE_PASSWORD@source.example.invalid/PRIVATE_PATH/master.m3u8?auth=PRIVATE_QUERY#PRIVATE_FRAGMENT";
    let long = format!(
        "https://source.example.invalid/{}/master.m3u8",
        "x".repeat(7000)
    );
    for (index, url) in [sensitive, &long].into_iter().enumerate() {
        let body = serde_json::json!({"schema_version":1,"server_id":run,"creation_request_id":format!("{index:032x}"),"source":{"kind":"hls-url","url":url,"fetch_policy_ref":null}});
        let created = client.post_json("/api/common/sessions", &body);
        assert_eq!(created.status, 201);
        let value = created.json();
        assert_eq!(
            value["source"],
            serde_json::json!({"kind":"hls-url","label":"HLS URL"})
        );
        let id = value["session_id"].as_str().unwrap();
        assert_eq!(
            client.get(&format!("/api/common/sessions/{id}")).json(),
            value
        );
        let replay = client.post_json("/api/common/sessions", &body);
        assert_eq!(replay.status, 200);
        assert_eq!(replay.json(), value);
    }
    let listed = client.get("/api/common/sessions");
    assert_eq!(listed.status, 200);
    assert_eq!(listed.json()["sessions"].as_array().unwrap().len(), 2);
    for private in [
        "PRIVATE_USER",
        "PRIVATE_PASSWORD",
        "PRIVATE_PATH",
        "PRIVATE_QUERY",
        "PRIVATE_FRAGMENT",
        &long,
    ] {
        assert!(!listed.text().contains(private));
    }
    server.stop().unwrap();
}

fn media_ref(client: &Client) -> (String, String) {
    let cap = client.get("/api/common/capabilities").json();
    let root = cap["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["label"] == "Input folder")
        .unwrap();
    let page = client
        .get(&format!(
            "/api/common/files?root_ref={}&role=media",
            root["root_ref"].as_str().unwrap()
        ))
        .json();
    (
        cap["server_id"].as_str().unwrap().to_owned(),
        page["entries"][0]["file_ref"].as_str().unwrap().to_owned(),
    )
}
fn local_session(client: &Client, server_id: &str, input_ref: &str, creation: char) -> Value {
    let reply=client.post_json("/api/common/sessions", &serde_json::json!({"schema_version":1,"server_id":server_id,"creation_request_id":creation.to_string().repeat(32),"source":{"kind":"local","input_ref":input_ref}}));
    assert_eq!(reply.status, 201, "{}", reply.text());
    reply.json()
}
#[test]
fn atomic_request_replay_owns_one_supervised_child_and_retains_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    std::fs::write(catalog.join("clip.mp4"), b"test fixture input").unwrap();
    let cli = support::FakeCli::new(root.path());
    cli.hold();
    cli.lines(&["COMMON_RAW_SECRET_MARKER"]);
    let mut config = ServerConfig::new(&catalog, root.path().join("output"));
    config.bind_port = 0;
    config.cli_argv = vec![cli.script.clone().into_os_string()];
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let (run, input) = media_ref(&client);
    let session = local_session(&client, &run, &input, 'a');
    let sid = session["session_id"].as_str().unwrap();
    let body = serde_json::json!({"schema_version":1,"server_id":run,"session_id":sid,"request_id":"b".repeat(32),"operation":{"kind":"analyze"},"runtime":{"keys_ref":null,"max_workers":null}});
    let first = client.post_json("/api/common/jobs", &body);
    assert_eq!(first.status, 202, "{}", first.text());
    let job = first.json();
    let id = job["job_id"].as_str().unwrap();
    cli.wait_calls(1);
    let replay = client.post_json("/api/common/jobs", &body);
    assert_eq!(replay.status, 202, "{}", replay.text());
    assert_eq!(replay.json()["job_id"], id);
    assert_eq!(cli.calls(), 1);
    assert_eq!(
        client.delete(&format!("/api/common/sessions/{sid}")).status,
        409
    );
    let mut conflict = body.clone();
    conflict["runtime"]["max_workers"] = serde_json::json!(1);
    assert_eq!(client.post_json("/api/common/jobs", &conflict).status, 409);
    let receipt = client
        .get(&format!(
            "/api/common/sessions/{sid}/requests/{}",
            "b".repeat(32)
        ))
        .json();
    assert_eq!(receipt["state"], "accepted");
    assert_eq!(receipt["job_id"], id);
    let stop = client.post_json(
        &format!("/api/common/jobs/{id}/stop"),
        &serde_json::json!({}),
    );
    assert!(matches!(stop.status, 200 | 202), "{}", stop.text());
    support::wait_until(|| {
        client.get(&format!("/api/common/jobs/{id}")).json()["state"] == "interrupted"
    });
    let ended = client.get(&format!("/api/common/jobs/{id}")).json();
    assert_eq!(ended["cleanup_confirmed"], true);
    assert_eq!(ended["supervision_confirmed"], true);
    assert!(
        !client
            .get(&format!("/api/jobs/{id}/log.jsonl"))
            .text()
            .contains("COMMON_RAW_SECRET_MARKER")
    );
    let argv = cli.argv(0);
    assert!(
        argv.iter()
            .any(|x| x == drmr_server::common_protocol::JOB_ARGUMENT)
    );
    assert!(!argv.iter().any(|x| x == catalog.to_str().unwrap()));
    assert_eq!(
        client.delete(&format!("/api/common/sessions/{sid}")).status,
        204
    );
    assert_eq!(client.post_json("/api/common/jobs", &body).status, 404);
    assert_eq!(cli.calls(), 1);
    server.stop().unwrap();
}
#[test]
fn output_parent_refs_are_explicit_and_not_directory_or_root_aliases() {
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    let mut config = ServerConfig::new(&catalog, root.path().join("output"));
    config.bind_port = 0;
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let cap = client.get("/api/common/capabilities").json();
    let r = cap["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| {
            r["roles"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("output-parent"))
        })
        .unwrap();
    let page = client.get(&format!(
        "/api/common/files?root_ref={}&role=output-parent",
        r["root_ref"].as_str().unwrap()
    ));
    assert_eq!(page.status, 200);
    assert!(page.json()["output_parent_ref"].as_str().is_some());
    assert_ne!(page.json()["output_parent_ref"], r["root_ref"]);
    assert_eq!(
        client
            .get(&format!(
                "/api/common/files?root_ref={}&role=output-parent&role=media",
                r["root_ref"].as_str().unwrap()
            ))
            .status,
        400
    );
    server.stop().unwrap();
}

fn analysis_payload(catalog: &std::path::Path) -> Value {
    serde_json::json!({"kind":"input-analysis","schema_version":1,"input_admitted":true,"input_checks":{"stream_metadata":"pass","packet_payloads_equal":true,"packet_timing_equal":true,"decoded_content_equal":true},"input":{"schema_version":1,"format":"mp4","evidence":"container-metadata","protection":"clear","tracks":[{"id":"private-label","kind":"video","codec":"avc1.640028","protection":"clear","key_ids":["0123456789abcdef0123456789abcdef"],"pattern":null}],"drm_systems":[],"findings":[{"code":"media-samples-not-verified"}],"media_verified":false},"actual_format":"mp4","track_protection":["clear"],"compatibility_notes":[],"eligible_formats":["mp4"],"source":{"schema_version":1,"root":catalog.canonicalize().unwrap(),"entrypoint":"clip.mp4","files":[{"path":"clip.mp4","bytes":18,"sha256":"a".repeat(64)}],"graph_digest":"b".repeat(64)}})
}
fn common_child_request(cli: &support::FakeCli) -> Value {
    cli.wait_calls(1);
    let args = cli.argv(0);
    let request_path = &args
        .windows(2)
        .find(|p| p[0] == drmr_server::common_protocol::JOB_ARGUMENT)
        .unwrap()[1];
    serde_json::from_slice(&std::fs::read(request_path).unwrap()).unwrap()
}
fn assert_analysis_report(client: &Client, ended: &Value, catalog: &std::path::Path) -> String {
    let report_ref = ended["report_ref"].as_str().unwrap();
    let report = client.get(&format!("/api/common/reports/{report_ref}"));
    assert_eq!(report.status, 200);
    assert!(!report.text().contains(catalog.to_str().unwrap()));
    assert!(!report.text().contains("0123456789abcdef0123456789abcdef"));
    assert!(!report.text().contains("private-label"));
    assert_eq!(report.json()["report"]["input"]["media_verified"], false);
    assert_eq!(report.json()["analysis_ref"], ended["analysis_ref"]);
    report_ref.into()
}
#[test]
fn owned_result_projection_rejects_stale_nonce_replacement_and_duplicate_fields() {
    for control in [
        "valid",
        "nonce",
        "operation",
        "replace",
        "duplicate",
        "partial",
        "oversize",
    ] {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        std::fs::create_dir(&catalog).unwrap();
        std::fs::write(catalog.join("clip.mp4"), b"test fixture input").unwrap();
        let cli = support::FakeCli::new(root.path());
        cli.hold();
        let mut config = ServerConfig::new(&catalog, root.path().join("output"));
        config.bind_port = 0;
        config.cli_argv = vec![cli.script.clone().into_os_string()];
        config.hooks.log = std::sync::Arc::new(|_| Ok(()));
        let server = spawn(config).unwrap();
        let client = Client::new(&server.url());
        let (run, input) = media_ref(&client);
        let session = local_session(&client, &run, &input, 'a');
        let sid = session["session_id"].as_str().unwrap();
        let body = serde_json::json!({"schema_version":1,"server_id":run,"session_id":sid,"request_id":"b".repeat(32),"operation":{"kind":"analyze"},"runtime":{"keys_ref":null,"max_workers":null}});
        let started = client.post_json("/api/common/jobs", &body);
        assert_eq!(started.status, 202, "{}", started.text());
        let job = started.json()["job_id"].as_str().unwrap().to_owned();
        let request = common_child_request(&cli);
        let result = std::path::Path::new(request["channels"]["result_file"].as_str().unwrap());
        let envelope = serde_json::json!({"protocol_version":1,"nonce":if control=="nonce" {"c".repeat(32)}else{request["nonce"].as_str().unwrap().to_owned()},"operation":if control=="operation" {"plan"}else{"analyze"},"payload":analysis_payload(&catalog)});
        let bytes = if control == "duplicate" {
            format!(
                "{{\"protocol_version\":1,\"protocol_version\":1,{}",
                serde_json::to_string(&envelope)
                    .unwrap()
                    .trim_start_matches('{')
            )
            .into_bytes()
        } else if control == "partial" {
            b"{\"protocol_version\":1".to_vec()
        } else {
            serde_json::to_vec(&envelope).unwrap()
        };
        let _old_object = if control == "replace" {
            let held = drmr_core::native::open_private_shared_read(result).unwrap();
            std::fs::remove_file(result).unwrap();
            Some(held)
        } else {
            None
        };
        support::write_private(result, &bytes).unwrap();
        if control == "oversize" {
            drmr_core::native::create_private_update(result)
                .unwrap()
                .set_len(drmr_server::common_protocol::RESULT_LIMIT as u64 + 1)
                .unwrap();
        }
        cli.release(0);
        support::wait_until(|| {
            client.get(&format!("/api/common/jobs/{job}")).json()["state"] != "running"
        });
        let ended = client.get(&format!("/api/common/jobs/{job}")).json();
        assert_eq!(ended["cleanup_confirmed"], true);
        assert_eq!(ended["supervision_confirmed"], true);
        if control == "valid" {
            assert_eq!(ended["state"], "succeeded", "{ended}");
            let report_ref = assert_analysis_report(&client, &ended, &catalog);
            assert_eq!(
                client.post_json("/api/common/jobs", &body).json()["job_id"],
                job
            );
            assert_eq!(cli.calls(), 1);
            let analysis = client
                .get(&format!(
                    "/api/common/analyses/{}",
                    ended["analysis_ref"].as_str().unwrap()
                ))
                .json();
            assert_eq!(analysis["job_id"], job);
            assert_eq!(analysis["session_id"], sid);
            assert_eq!(
                client.delete(&format!("/api/common/sessions/{sid}")).status,
                204
            );
            assert_eq!(client.get(&format!("/api/common/jobs/{job}")).status, 404);
            assert_eq!(
                client
                    .get(&format!("/api/common/reports/{report_ref}"))
                    .status,
                404
            );
        } else {
            assert_eq!(ended["state"], "failed", "{control}: {ended}");
            assert!(ended["report_ref"].is_null());
        }
        server.stop().unwrap();
    }
}
#[test]
fn capacity_and_closed_session_fences_do_not_evict_an_active_owner() {
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    std::fs::write(catalog.join("clip.mp4"), b"fixture").unwrap();
    let cli = support::FakeCli::new(root.path());
    cli.hold();
    let mut config = ServerConfig::new(&catalog, root.path().join("output"));
    config.bind_port = 0;
    config.cli_argv = vec![cli.script.clone().into_os_string()];
    config.hooks.log = std::sync::Arc::new(|_| Ok(()));
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let (run, input) = media_ref(&client);
    let first = local_session(&client, &run, &input, 'a');
    let sid = first["session_id"].as_str().unwrap();
    assert_eq!(
        client
            .get(&format!(
                "/api/common/sessions/{sid}/requests/{}",
                "b".repeat(32)
            ))
            .status,
        404
    );
    let mut body = serde_json::json!({"schema_version":1,"server_id":run,"session_id":sid,"request_id":"b".repeat(32),"operation":{"kind":"analyze"},"runtime":{"keys_ref":null,"max_workers":null}});
    let started = client.post_json("/api/common/jobs", &body);
    assert_eq!(started.status, 202);
    let job = started.json()["job_id"].as_str().unwrap().to_owned();
    for n in 0..31 {
        body["request_id"] = serde_json::json!(format!("{n:032x}"));
        let rejected = client.post_json("/api/common/jobs", &body);
        assert_eq!(rejected.status, 409);
        assert_eq!(rejected.json()["error"]["code"], "session-busy");
    }
    body["request_id"] = serde_json::json!("f".repeat(32));
    assert_eq!(
        client.post_json("/api/common/jobs", &body).json()["error"]["code"],
        "request-limit-reached"
    );
    assert_eq!(cli.calls(), 1);
    assert_eq!(
        client.get(&format!("/api/common/sessions/{sid}")).json()["requests_used"],
        32
    );
    assert_eq!(
        client.delete(&format!("/api/common/sessions/{sid}")).status,
        409
    );
    for c in ['b', 'c', 'd', 'e', 'f', '0', '1'] {
        local_session(&client, &run, &input, c);
    }
    let extra=client.post_json("/api/common/sessions",&serde_json::json!({"schema_version":1,"server_id":run,"creation_request_id":"2".repeat(32),"source":{"kind":"local","input_ref":input}}));
    assert_eq!(extra.json()["error"]["code"], "session-limit-reached");
    client.post_json(
        &format!("/api/common/jobs/{job}/stop"),
        &serde_json::json!({}),
    );
    support::wait_until(|| {
        client.get(&format!("/api/common/jobs/{job}")).json()["state"] == "interrupted"
    });
    assert_eq!(
        client.delete(&format!("/api/common/sessions/{sid}")).status,
        204
    );
    let fresh = local_session(&client, &run, &input, 'a');
    assert_ne!(fresh["session_id"], sid);
    assert_eq!(fresh["requests_used"], 0);
    assert_eq!(client.post_json("/api/common/jobs", &body).status, 404);
    assert_eq!(cli.calls(), 1);
    server.stop().unwrap();
}
#[cfg(unix)]
#[test]
fn insecure_original_configuration_is_refused_before_copy_or_launch() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    std::fs::write(catalog.join("clip.mp4"), b"fixture").unwrap();
    let private = root.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let key = private.join("DO_NOT_DISPLAY_SECRET_NAME.json");
    std::fs::write(&key, b"{}").unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let cli = support::FakeCli::new(root.path());
    let mut config = ServerConfig::new(&catalog, root.path().join("output"));
    config.common_config_root = Some(private);
    config.bind_port = 0;
    config.cli_argv = vec![cli.script.clone().into_os_string()];
    config.hooks.log = std::sync::Arc::new(|_| Ok(()));
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let (run, input) = media_ref(&client);
    let sid = local_session(&client, &run, &input, 'a')["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let cap = client.get("/api/common/capabilities").json();
    let r = cap["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| {
            r["roles"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("keys"))
        })
        .unwrap();
    let page = client.get(&format!(
        "/api/common/files?root_ref={}&role=keys",
        r["root_ref"].as_str().unwrap()
    ));
    assert!(!page.text().contains("DO_NOT_DISPLAY"));
    let key_ref = page.json()["entries"][0]["file_ref"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = serde_json::json!({"schema_version":1,"server_id":run,"session_id":sid,"request_id":"b".repeat(32),"operation":{"kind":"analyze"},"runtime":{"keys_ref":key_ref,"max_workers":null}});
    let rejected = client.post_json("/api/common/jobs", &body);
    assert_eq!(rejected.status, 422, "{}", rejected.text());
    assert_eq!(cli.calls(), 0);
    assert_eq!(
        std::fs::metadata(&key).unwrap().permissions().mode() & 0o777,
        0o644
    );
    server.stop().unwrap();
}

#[test]
fn dedicated_configuration_root_is_disjoint_in_both_ancestor_directions() {
    for direction in ["below", "above"] {
        let root = tempfile::tempdir().unwrap();
        let catalog = root.path().join("catalog");
        std::fs::create_dir(&catalog).unwrap();
        let output = root.path().join("output");
        let mut config = ServerConfig::new(&catalog, &output);
        let private = if direction == "below" {
            catalog.join("private")
        } else {
            root.path().to_path_buf()
        };
        std::fs::create_dir_all(&private).unwrap();
        config.common_config_root = Some(private);
        assert!(
            drmr_server::App::new(config).is_err(),
            "{direction} root overlap was admitted"
        );
    }
}
#[cfg(unix)]
#[test]
fn original_copies_result_progress_and_retained_plans_are_denied_by_custom_static_aliases() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    let (entry, _) = stage(&catalog, "asset");
    let output = entry.join("download/results");
    let private = root.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let key = private.join("keys.json");
    support::write_private(&key, b"PRIVATE_ORIGINAL_MARKER").unwrap();
    let static_dir = root.path().join("static");
    std::fs::create_dir(&static_dir).unwrap();
    symlink(&private, static_dir.join("secret")).unwrap();
    symlink(&output, static_dir.join("results")).unwrap();
    std::fs::write(static_dir.join("public.txt"), b"public").unwrap();
    let mut config = ServerConfig::new(&catalog, &output);
    config.common_config_root = Some(private);
    config.static_dir = Some(static_dir);
    config.bind_port = 0;
    config.hooks.log = std::sync::Arc::new(|_| Ok(()));
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    for name in ["request", "keys", "fetch-policy", "result", "progress"] {
        support::write_private(
            &output.join("_jobs/common").join(format!("{name}.json")),
            format!("PRIVATE_COPY_MARKER_{name}"),
        )
        .unwrap();
    }
    support::write_private(
        &output.join("_common/retained/plan.json"),
        b"PRIVATE_PLAN_MARKER",
    )
    .unwrap();
    for path in [
        "/static/secret/keys.json",
        "/static/results/_jobs/common/request.json",
        "/static/results/_jobs/common/keys.json",
        "/static/results/_jobs/common/fetch-policy.json",
        "/static/results/_jobs/common/result.json",
        "/static/results/_jobs/common/progress.json",
        "/static/results/_common/retained/plan.json",
        "/source/asset/results/_jobs/common/result.json",
    ] {
        let reply = client.get(path);
        assert_eq!(reply.status, 404, "{path}");
        assert!(!reply.text().contains("PRIVATE_"));
    }
    assert_eq!(client.get("/static/public.txt").text(), "public");
    server.stop().unwrap();
}
#[cfg(unix)]
#[test]
fn private_copy_cleanup_failure_holds_the_shared_execution_lease_until_recovered() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    std::fs::write(catalog.join("clip.mp4"), b"fixture").unwrap();
    let cli = support::FakeCli::new(root.path());
    cli.hold();
    let mut config = ServerConfig::new(&catalog, root.path().join("output"));
    config.bind_port = 0;
    config.cli_argv = vec![cli.script.clone().into_os_string()];
    config.hooks.log = std::sync::Arc::new(|_| Ok(()));
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let (run, input) = media_ref(&client);
    let sid = local_session(&client, &run, &input, 'a')["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let body = serde_json::json!({"schema_version":1,"server_id":run,"session_id":sid,"request_id":"b".repeat(32),"operation":{"kind":"analyze"},"runtime":{"keys_ref":null,"max_workers":null}});
    let started = client.post_json("/api/common/jobs", &body).json();
    let job = started["job_id"].as_str().unwrap();
    let argv = cli.argv(0);
    let request = std::path::PathBuf::from(
        &argv
            .windows(2)
            .find(|a| a[0] == drmr_server::common_protocol::JOB_ARGUMENT)
            .unwrap()[1],
    );
    let parent = request.parent().unwrap();
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o500)).unwrap();
    client.post_json(
        &format!("/api/common/jobs/{job}/stop"),
        &serde_json::json!({}),
    );
    support::wait_until(|| {
        client.get(&format!("/api/common/jobs/{job}")).json()["error"]["code"]
            == "cleanup-unconfirmed"
    });
    assert!(server.app.manager.is_busy());
    assert_eq!(
        client.delete(&format!("/api/common/sessions/{sid}")).status,
        409
    );
    assert!(request.exists());
    assert!(matches!(
        server.app.manager.submit_command(&["doctor".into()], &[]),
        Err(drmr_server::jobs::JobError::AlreadyRunning)
    ));
    std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    support::wait_until(|| {
        client.get(&format!("/api/common/jobs/{job}")).json()["state"] == "interrupted"
    });
    assert!(!request.exists());
    assert!(!server.app.manager.is_busy());
    server.stop().unwrap();
}

#[test]
fn progress_is_bounded_and_bound_to_the_live_nonce_operation_and_monotone_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    std::fs::write(catalog.join("clip.mp4"), b"test fixture input").unwrap();
    let cli = support::FakeCli::new(root.path());
    cli.hold();
    let mut config = ServerConfig::new(&catalog, root.path().join("output"));
    config.bind_port = 0;
    config.cli_argv = vec![cli.script.clone().into_os_string()];
    config.hooks.log = std::sync::Arc::new(|_| Ok(()));
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let (run, input) = media_ref(&client);
    let session = local_session(&client, &run, &input, 'a');
    let started = client.post_json("/api/common/jobs", &serde_json::json!({
        "schema_version":1,"server_id":run,"session_id":session["session_id"],"request_id":"b".repeat(32),
        "operation":{"kind":"analyze"},"runtime":{"keys_ref":null,"max_workers":null}
    }));
    assert_eq!(started.status, 202);
    let job = started.json()["job_id"].as_str().unwrap().to_owned();
    cli.wait_calls(1);
    let args = cli.argv(0);
    let request_file = &args
        .windows(2)
        .find(|p| p[0] == drmr_server::common_protocol::JOB_ARGUMENT)
        .unwrap()[1];
    let request: Value = serde_json::from_slice(&std::fs::read(request_file).unwrap()).unwrap();
    let path = std::path::Path::new(request["channels"]["progress_file"].as_str().unwrap());
    let valid = serde_json::json!({"protocol_version":1,"nonce":request["nonce"],"operation":"analyze",
        "sequence":1,"elapsed_ms":10,"last_activity":"inspection","scopes_started":2,"scopes_closed":1,"workers":2});
    for (field, value) in [
        ("nonce", serde_json::json!("0".repeat(32))),
        ("operation", serde_json::json!("convert")),
        (
            "last_activity",
            serde_json::json!("PRIVATE_PROGRESS_MARKER"),
        ),
        ("scopes_closed", serde_json::json!(3)),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = value;
        support::write_private(path, invalid.to_string()).unwrap();
        let reply = client.get(&format!("/api/common/jobs/{job}"));
        assert!(
            reply.json()["progress"].is_null(),
            "{field}: {}",
            reply.text()
        );
        assert!(!reply.text().contains("PRIVATE_PROGRESS_MARKER"));
    }
    support::write_private(path, valid.to_string()).unwrap();
    assert_eq!(
        client.get(&format!("/api/common/jobs/{job}")).json()["progress"]["sequence"],
        1
    );
    for (sequence, elapsed) in [(0, 20), (2, 0)] {
        let mut invalid = valid.clone();
        invalid["sequence"] = serde_json::json!(sequence);
        invalid["elapsed_ms"] = serde_json::json!(elapsed);
        support::write_private(path, invalid.to_string()).unwrap();
        assert_eq!(
            client.get(&format!("/api/common/jobs/{job}")).json()["progress"]["sequence"],
            1
        );
    }
    drmr_core::native::create_private_update(path)
        .unwrap()
        .set_len(drmr_server::common_protocol::PROGRESS_LIMIT as u64 + 1)
        .unwrap();
    assert_eq!(
        client.get(&format!("/api/common/jobs/{job}")).json()["progress"]["sequence"],
        1
    );
    client.post_json(
        &format!("/api/common/jobs/{job}/stop"),
        &serde_json::json!({}),
    );
    support::wait_until(|| {
        client.get(&format!("/api/common/jobs/{job}")).json()["cleanup_confirmed"] == true
    });
    server.stop().unwrap();
}

#[test]
fn pending_admission_replay_and_delete_share_the_claim_fence_before_spawn() {
    use std::sync::{Arc, Condvar, Mutex, mpsc};
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    std::fs::write(catalog.join("clip.mp4"), b"test fixture input").unwrap();
    let cli = support::FakeCli::new(root.path());
    cli.hold();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let wait = gate.clone();
    let (entered, ready) = mpsc::channel();
    let mut config = ServerConfig::new(&catalog, root.path().join("output"));
    config.bind_port = 0;
    config.cli_argv = vec![cli.script.clone().into_os_string()];
    config.hooks.log = Arc::new(|_| Ok(()));
    config.hooks.base_env = Arc::new(move || {
        entered.send(()).unwrap();
        let (mutex, changed) = &*wait;
        let _guard = changed
            .wait_timeout_while(mutex.lock().unwrap(), Duration::from_secs(5), |go| !*go)
            .unwrap();
        std::env::vars_os().collect()
    });
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let (run, input) = media_ref(&client);
    let session = local_session(&client, &run, &input, 'a');
    let sid = session["session_id"].as_str().unwrap().to_owned();
    let body = serde_json::json!({"schema_version":1,"server_id":run,"session_id":sid,"request_id":"b".repeat(32),
        "operation":{"kind":"analyze"},"runtime":{"keys_ref":null,"max_workers":null}});
    let url = server.url();
    let submitted = body.clone();
    let first =
        std::thread::spawn(move || Client::new(&url).post_json("/api/common/jobs", &submitted));
    ready.recv_timeout(Duration::from_secs(5)).unwrap();
    let receipt = client
        .get(&format!(
            "/api/common/sessions/{sid}/requests/{}",
            "b".repeat(32)
        ))
        .json();
    assert_eq!(receipt["state"], "pending");
    assert!(receipt["job_id"].is_null());
    let replay = client.post_json("/api/common/jobs", &body);
    assert_eq!(replay.status, 409);
    assert_eq!(replay.json()["error"]["code"], "request-pending");
    assert_eq!(
        client.delete(&format!("/api/common/sessions/{sid}")).status,
        409
    );
    assert_eq!(
        client.get(&format!("/api/common/sessions/{sid}")).json()["requests_used"],
        1
    );
    assert_eq!(cli.calls(), 0);
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let admitted = first.join().unwrap();
    assert_eq!(admitted.status, 202, "{}", admitted.text());
    cli.wait_calls(1);
    assert_eq!(
        client.post_json("/api/common/jobs", &body).json()["job_id"],
        admitted.json()["job_id"]
    );
    let id = admitted.json()["job_id"].as_str().unwrap().to_owned();
    client.post_json(
        &format!("/api/common/jobs/{id}/stop"),
        &serde_json::json!({}),
    );
    support::wait_until(|| {
        client.get(&format!("/api/common/jobs/{id}")).json()["cleanup_confirmed"] == true
    });
    assert_eq!(cli.calls(), 1);
    server.stop().unwrap();
}

#[cfg(unix)]
struct RestorePrivateMode(std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>);
#[cfg(unix)]
impl Drop for RestorePrivateMode {
    fn drop(&mut self) {
        if let Some(path) = self.0.lock().unwrap().as_ref().filter(|p| p.exists()) {
            support::set_mode(path, 0o700);
        }
    }
}
#[cfg(unix)]
fn race_pending_observers(server: &drmr_server::RunningServer, body: &Value, receipt_path: &str) {
    use std::sync::Arc;
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let threads: Vec<_> = (0..3)
        .map(|mode| {
            let barrier = barrier.clone();
            let url = server.url();
            let body = body.clone();
            let path = receipt_path.to_owned();
            let app = server.app.clone();
            std::thread::spawn(move || {
                barrier.wait();
                match mode {
                    0 => {
                        let reply = Client::new(&url).get(&path);
                        assert!(matches!(reply.status, 200 | 503));
                    }
                    1 => {
                        let reply = Client::new(&url).post_json("/api/common/jobs", &body);
                        assert_eq!(reply.status, 503);
                    }
                    _ => assert!(app.shutdown().is_err()),
                }
            })
        })
        .collect();
    barrier.wait();
    for thread in threads {
        thread.join().unwrap();
    }
}
#[cfg(unix)]
#[test]
fn launch_failure_retains_prepared_copies_pending_claim_and_shared_lease_until_cleanup() {
    use std::sync::{Arc, Mutex};
    let root = tempfile::tempdir().unwrap();
    let catalog = root.path().join("catalog");
    std::fs::create_dir(&catalog).unwrap();
    std::fs::write(catalog.join("clip.mp4"), b"test fixture input").unwrap();
    let output = root.path().join("output");
    let common = output.join("_jobs/common");
    let held = Arc::new(Mutex::new(None));
    let capture = held.clone();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let called = calls.clone();
    let mut config = ServerConfig::new(&catalog, &output);
    config.bind_port = 0;
    config.cli_argv = vec![root.path().join("absent-cli").into_os_string()];
    config.hooks.log = Arc::new(|_| Ok(()));
    config.hooks.base_env = Arc::new(move || {
        called.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let server = std::fs::read_dir(&common)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let attempt = std::fs::read_dir(server)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(attempt.join("request.json").is_file());
        support::set_mode(&attempt, 0o500);
        *capture.lock().unwrap() = Some(attempt);
        std::env::vars_os().collect()
    });
    let mut server = spawn(config).unwrap();
    let _restore = RestorePrivateMode(held.clone());
    let client = Client::new(&server.url());
    let (run, input) = media_ref(&client);
    let session = local_session(&client, &run, &input, 'a');
    let sid = session["session_id"].as_str().unwrap();
    let body = serde_json::json!({"schema_version":1,"server_id":run,"session_id":sid,"request_id":"b".repeat(32),
        "operation":{"kind":"analyze"},"runtime":{"keys_ref":null,"max_workers":null}});
    let rejected = client.post_json("/api/common/jobs", &body);
    assert_eq!(rejected.status, 503, "{}", rejected.text());
    assert_eq!(rejected.json()["error"]["code"], "cleanup-unconfirmed");
    let receipt_path = format!("/api/common/sessions/{sid}/requests/{}", "b".repeat(32));
    let pending = client.get(&receipt_path).json();
    assert_eq!(pending["state"], "pending", "{pending}");
    assert!(pending["job_id"].is_null());
    assert_eq!(
        client.delete(&format!("/api/common/sessions/{sid}")).status,
        409
    );
    assert!(matches!(
        server.app.manager.submit_command(&["noop".into()], &[]),
        Err(drmr_server::jobs::JobError::AlreadyRunning)
    ));
    assert_eq!(
        client.get("/api/common/capabilities").json()["execution"]["availability"],
        "cleanup-unconfirmed"
    );
    let attempt = held.lock().unwrap().clone().unwrap();
    assert!(attempt.join("request.json").is_file());
    assert_eq!(
        std::fs::read(catalog.join("clip.mp4")).unwrap(),
        b"test fixture input"
    );
    race_pending_observers(&server, &body, &receipt_path);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(server.app.manager.execution().busy);
    assert!(server.shutdown().is_err());
    let pending = client.get(&receipt_path);
    assert_eq!(pending.status, 200, "{}", pending.text());
    assert_eq!(pending.json()["state"], "pending");
    support::set_mode(&attempt, 0o700);
    let receipt = client.get(&receipt_path).json();
    assert_eq!(receipt["state"], "rejected", "{receipt}");
    assert_eq!(receipt["error"]["code"], "common-service-unavailable");
    assert!(!attempt.join("request.json").exists());
    assert!(!server.app.manager.execution().busy);
    assert_eq!(
        client.get(&format!("/api/common/sessions/{sid}")).json()["requests_used"],
        1
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    server.shutdown().unwrap();
}

#[test]
fn directory_references_remain_bound_to_the_issuing_root_even_for_nested_public_roots() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("output");
    let catalog = output.join("input");
    std::fs::create_dir_all(catalog.join("folder")).unwrap();
    std::fs::write(catalog.join("folder/clip.mp4"), b"fixture").unwrap();
    let mut config = ServerConfig::new(&catalog, &output);
    config.bind_port = 0;
    config.hooks.log = std::sync::Arc::new(|_| Ok(()));
    let server = spawn(config).unwrap();
    let client = Client::new(&server.url());
    let cap = client.get("/api/common/capabilities").json();
    let roots = cap["roots"].as_array().unwrap();
    let input = roots.iter().find(|r| r["label"] == "Input folder").unwrap()["root_ref"]
        .as_str()
        .unwrap();
    let output = roots
        .iter()
        .find(|r| r["label"] == "Output folder")
        .unwrap()["root_ref"]
        .as_str()
        .unwrap();
    let page = client
        .get(&format!("/api/common/files?root_ref={input}&role=media"))
        .json();
    let directory = page["entries"][0]["directory_ref"].as_str().unwrap();
    let original = client.get(&format!(
        "/api/common/files?root_ref={input}&role=media&directory_ref={directory}"
    ));
    assert_eq!(original.status, 200);
    let crossed = client.get(&format!(
        "/api/common/files?root_ref={output}&role=media&directory_ref={directory}"
    ));
    assert_eq!(crossed.status, 404, "{}", crossed.text());
    server.stop().unwrap();
}
