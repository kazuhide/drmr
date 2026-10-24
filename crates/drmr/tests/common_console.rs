//! Actual loopback HTTP operations use the same executable and saved plans as the CLI.
use drmr_core::native;
use drmr_server::{RunningServer, ServerConfig};
use serde_json::{Value, json};
use std::{
    io::Write as _,
    path::Path,
    process::Command,
    thread,
    time::{Duration, Instant},
};

struct Client {
    url: String,
    agent: ureq::Agent,
}
impl Client {
    fn new(server: &RunningServer) -> Self {
        Self {
            url: server.url(),
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(15)))
                .build()
                .into(),
        }
    }
    fn get(&self, path: &str) -> Value {
        let mut response = self
            .agent
            .get(&format!("{}{path}", self.url))
            .call()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200, "{path}");
        assert!(
            response
                .headers()
                .get("cache-control")
                .unwrap()
                .to_str()
                .unwrap()
                .contains("no-store")
        );
        serde_json::from_str(&response.body_mut().read_to_string().unwrap()).unwrap()
    }
    fn post(&self, path: &str, value: &Value, status: u16) -> Value {
        let mut response = self
            .agent
            .post(&format!("{}{path}", self.url))
            .header("Origin", &self.url)
            .header("Content-Type", "application/json")
            .send(value.to_string())
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            status,
            "{path}: {}",
            response.body_mut().read_to_string().unwrap()
        );
        serde_json::from_str(&response.body_mut().read_to_string().unwrap()).unwrap()
    }
    fn export_plan(&self, plan_ref: &str) -> Vec<u8> {
        let mut response = self
            .agent
            .get(&format!("{}/api/common/plans/{plan_ref}/export", self.url))
            .call()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        response.body_mut().read_to_vec().unwrap()
    }
    fn selection(&self, capabilities: &Value, role: &str, name: Option<&str>) -> String {
        for root in capabilities["roots"].as_array().unwrap() {
            if !root["roles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value == role)
            {
                continue;
            }
            let root_ref = root["root_ref"].as_str().unwrap();
            let page = self.get(&format!(
                "/api/common/files?root_ref={root_ref}&role={role}"
            ));
            match name {
                None => return page["output_parent_ref"].as_str().unwrap().into(),
                Some(name) => {
                    if let Some(file) = page["entries"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|entry| entry["kind"] == "file" && entry["label"] == name)
                    {
                        return file["file_ref"].as_str().unwrap().into();
                    }
                }
            }
        }
        panic!("fixture selection was not admitted for {role}");
    }
    fn job(&self, body: &Value, expected: &str) -> Value {
        let accepted = self.post("/api/common/jobs", body, 202);
        let replay = self.post("/api/common/jobs", body, 202);
        assert_eq!(accepted["job_id"], replay["job_id"]);
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            let job = self.get(&format!(
                "/api/common/jobs/{}",
                accepted["job_id"].as_str().unwrap()
            ));
            if job["state"] != "running" {
                assert_eq!(job["state"], expected, "{job}");
                assert_eq!(job["supervision_confirmed"], true);
                assert_eq!(job["cleanup_confirmed"], true);
                if expected == "succeeded" {
                    assert_eq!(job["exit_code"], 0);
                }
                return job;
            }
            assert!(
                Instant::now() < deadline,
                "common job did not finish within its bound"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}

fn no_private_fields(value: &Value, private_root: &str) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                assert!(
                    ![
                        "key",
                        "key_ids",
                        "iv",
                        "license_url",
                        "hls_key_uri",
                        "source_inventory",
                        "download",
                        "entrypoint",
                        "input_file",
                        "plan_file",
                        "keys_file",
                        "fetch_policy_file",
                        "output_root"
                    ]
                    .contains(&name.as_str()),
                    "private field {name}"
                );
                no_private_fields(value, private_root);
            }
        }
        Value::Array(values) => {
            for value in values {
                no_private_fields(value, private_root);
            }
        }
        Value::String(text) => assert!(!text.contains(private_root)),
        _ => {}
    }
}
fn media_checks(report: &Value) {
    assert_eq!(report["passed"], true);
    assert_eq!(
        report["decode_evidence"],
        serde_json::json!({"status":"content-compared","equal":true})
    );
    assert_eq!(report["license_playback_verified"], false);
    assert_eq!(
        report["checks"],
        json!({"stream_metadata":"pass","packet_payloads_equal":true,"packet_timing_equal":true})
    );
}

fn prepare_fragmented_video(source: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let prepared = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(fixture)
        .args([
            "-map",
            "0:v:0",
            "-c",
            "copy",
            "-an",
            "-map_metadata",
            "-1",
            "-bitexact",
            "-movflags",
            "delay_moov+default_base_moof+frag_keyframe",
        ])
        .arg(source)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
}

fn verify_exported_plan(saved: &Path, plan_id: &str) {
    let external = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(["verify", "--decode", "--plan"])
        .arg(saved)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        external.status.success(),
        "{}",
        String::from_utf8_lossy(&external.stderr)
    );
    let independently_verified: Value = serde_json::from_slice(&external.stdout).unwrap();
    assert_eq!(independently_verified["plan_id"], plan_id);
    assert_eq!(independently_verified["passed"], true);
}

#[test]
fn real_http_flow_saves_exports_executes_and_independently_verifies_the_same_cli_plan() {
    let root = native::private_tempdir().unwrap();
    let input = root.path().join("input");
    let output = root.path().join("output");
    native::create_private_dir_new(&input).unwrap();
    native::create_private_dir_new(&output).unwrap();
    let source = input.join("source.mp4");
    prepare_fragmented_video(&source);
    let original_source = std::fs::read(&source).unwrap();
    let mut config = ServerConfig::new(&input, &output);
    config.bind_port = 0;
    config.cli_argv = vec![env!("CARGO_BIN_EXE_drmr").into()];
    config.max_workers = Some(2);
    let mut server = drmr_server::spawn(config).unwrap();
    let client = Client::new(&server);
    let capabilities = client.get("/api/common/capabilities");
    assert_eq!(
        capabilities["worker_policy"],
        json!({"maximum":2,"automatic":true})
    );
    let source_ref = client.selection(&capabilities, "media", Some("source.mp4"));
    let parent_ref = client.selection(&capabilities, "output-parent", None);
    let server_id = capabilities["server_id"].clone();
    let session = client.post("/api/common/sessions", &json!({"schema_version":1,"server_id":server_id,"creation_request_id":"b".repeat(32),"source":{"kind":"local","input_ref":source_ref}}), 201);
    let session_id = session["session_id"].clone();
    let submit = |ordinal: u32, operation: Value| json!({"schema_version":1,"server_id":server_id,"session_id":session_id,"request_id":format!("{ordinal:032x}"),"operation":operation,"runtime":{"decode":true,"keys_ref":null,"max_workers":null}});
    let analyzed = client.job(&submit(1, json!({"kind":"analyze"})), "succeeded");
    let analysis = client.get(&format!(
        "/api/common/reports/{}",
        analyzed["report_ref"].as_str().unwrap()
    ));
    assert_eq!(analysis["report"]["input_admitted"], true);
    let planned = client.job(&submit(2, json!({"kind":"plan","analysis_ref":analyzed["analysis_ref"],"output_parent_ref":parent_ref,"destination_name":"results","outputs":[
        {"id":"dash","format":"dash","target":"clear","drms":[],"conformance":"none"},
        {"id":"hls","format":"hls","target":"clear","drms":[],"conformance":"none"}
    ]})), "succeeded");
    let plan_ref = planned["plan_ref"].clone();
    let plan = client.get(&format!("/api/common/plans/{}", plan_ref.as_str().unwrap()));
    assert_eq!(plan["plan_schema_version"], 4);
    assert_eq!(plan["required_depth"], "decoded");
    assert_eq!(plan["outputs"].as_array().unwrap().len(), 2);
    let exported = client.export_plan(plan_ref.as_str().unwrap());
    let saved = input.join("exported.plan.json");
    native::create_private_new(&saved)
        .unwrap()
        .write_all(&exported)
        .unwrap();
    let parsed: Value = serde_json::from_slice(&exported).unwrap();
    assert_eq!(parsed["plan_id"], plan["plan_id"]);
    let converted = client.job(
        &submit(3, json!({"kind":"convert","plan_ref":plan_ref})),
        "succeeded",
    );
    let verified = client.job(
        &submit(4, json!({"kind":"verify","plan_ref":plan_ref})),
        "succeeded",
    );
    assert_eq!(converted["plan_id"], plan["plan_id"]);
    assert_eq!(verified["plan_id"], plan["plan_id"]);
    let report = client.get(&format!(
        "/api/common/reports/{}",
        verified["report_ref"].as_str().unwrap()
    ));
    assert_eq!(report["report"]["source_unchanged"], true);
    for row in report["report"]["outputs"].as_array().unwrap() {
        assert_eq!(row["output_matches_plan"], true);
        media_checks(&row["verification"]);
        assert_eq!(row["conformance"], Value::Null);
    }
    for value in [&capabilities, &session, &analysis, &plan, &report] {
        no_private_fields(value, root.path().to_str().unwrap());
    }
    verify_exported_plan(&saved, plan["plan_id"].as_str().unwrap());
    let manifest = output.join("results/dash/manifest.mpd");
    let original_manifest = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"invalid manifest").unwrap();
    let failed = client.job(
        &submit(5, json!({"kind":"verify","plan_ref":plan_ref})),
        "failed",
    );
    assert!(failed["report_ref"].is_string(), "{failed}");
    let failure = client.get(&format!(
        "/api/common/reports/{}",
        failed["report_ref"].as_str().unwrap()
    ));
    assert_ne!(failure["report"]["passed"], true);
    assert_ne!(failed["exit_code"], 0);
    std::fs::write(&manifest, original_manifest).unwrap();
    server.shutdown().unwrap();
    assert_eq!(std::fs::read(&source).unwrap(), original_source);
    assert_eq!(std::fs::read(saved).unwrap(), exported);
}

#[test]
fn a_valid_source_changed_after_analysis_cannot_publish_a_new_plan() {
    let root = native::private_tempdir().unwrap();
    let input = root.path().join("input");
    let output = root.path().join("output");
    native::create_private_dir_new(&input).unwrap();
    native::create_private_dir_new(&output).unwrap();
    let source = input.join("source.mp4");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    std::fs::copy(fixture, &source).unwrap();
    let mut config = ServerConfig::new(&input, &output);
    config.bind_port = 0;
    config.cli_argv = vec![env!("CARGO_BIN_EXE_drmr").into()];
    config.max_workers = Some(2);
    let mut server = drmr_server::spawn(config).unwrap();
    let client = Client::new(&server);
    let capabilities = client.get("/api/common/capabilities");
    let source_ref = client.selection(&capabilities, "media", Some("source.mp4"));
    let parent_ref = client.selection(&capabilities, "output-parent", None);
    let server_id = capabilities["server_id"].clone();
    let session = client.post("/api/common/sessions", &json!({"schema_version":1,"server_id":server_id,"creation_request_id":"c".repeat(32),"source":{"kind":"local","input_ref":source_ref}}), 201);
    let session_id = session["session_id"].clone();
    let submit = |ordinal: u32, operation: Value| json!({"schema_version":1,"server_id":server_id,"session_id":session_id,"request_id":format!("{ordinal:032x}"),"operation":operation,"runtime":{"decode":true,"keys_ref":null,"max_workers":null}});
    let analyzed = client.job(&submit(1, json!({"kind":"analyze"})), "succeeded");

    std::fs::OpenOptions::new()
        .append(true)
        .open(&source)
        .unwrap()
        .write_all(&[0, 0, 0, 8, b'f', b'r', b'e', b'e'])
        .unwrap();
    let changed = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("analyze")
        .arg("--decode")
        .arg(&source)
        .arg("--json")
        .output()
        .unwrap();
    assert!(changed.status.success());
    let qualified: Value = serde_json::from_slice(&changed.stdout).unwrap();
    assert_eq!(qualified["input_admitted"], true);

    let rejected = client.job(&submit(2, json!({"kind":"plan","analysis_ref":analyzed["analysis_ref"],"output_parent_ref":parent_ref,"destination_name":"rejected","outputs":[{"id":"clear","format":"mp4","target":"clear","drms":[],"conformance":"none"}]})), "failed");
    assert_eq!(rejected["error"]["code"], "analysis-source-changed");
    assert_eq!(rejected["plan_ref"], Value::Null);
    let current = client.get(&format!(
        "/api/common/sessions/{}",
        session_id.as_str().unwrap()
    ));
    assert_eq!(current["plan_refs"], json!([]));
    assert!(!output.join("rejected").exists());
    server.shutdown().unwrap();
}
