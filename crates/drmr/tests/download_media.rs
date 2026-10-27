//! Real downloaded HLS retains its source and remains reusable without the server.
#[path = "support/download_http.rs"]
mod download_http;

use std::{
    collections::BTreeMap,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    process::{Command, Output},
};

const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const KID: &str = "000102030405060708090a0b0c0d0e0f";

fn private_json(path: &Path, value: &serde_json::Value) {
    drmr_core::native::create_private_dir_all(path.parent().unwrap()).unwrap();
    drmr_core::native::create_private_new(path)
        .unwrap()
        .write_all(value.to_string().as_bytes())
        .unwrap();
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            assert!(!path.is_symlink());
            if path.is_dir() {
                pending.push(path);
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }
    result
}

fn fixture(root: &Path, fragmented: bool) -> PathBuf {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let mut command = Command::new("ffmpeg");
    command
        .current_dir(root)
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(source)
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-c",
            "copy",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-start_number",
            "7",
        ]);
    if fragmented {
        command.args(["-hls_segment_type", "fmp4"]);
    }
    let extension = if fragmented { "m4s" } else { "ts" };
    let result = command
        .args([
            "-hls_segment_filename",
            &format!("segment-%d.{extension}"),
            "media.m3u8",
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "fixture generation failed");
    let media = root.join("media.m3u8");
    let original = std::fs::read_to_string(&media).unwrap();
    let modified = original
        .lines()
        .map(|line| {
            if let Some(duration) = line
                .strip_prefix("#EXTINF:")
                .and_then(|s| s.strip_suffix(','))
            {
                assert!(duration.contains('.'));
                format!("#EXTINF:{duration}000,")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert!(modified.contains("#EXT-X-MEDIA-SEQUENCE:7"));
    std::fs::write(&media, modified).unwrap();
    let master = root.join("master.m3u8");
    std::fs::write(
        &master,
        "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nmedia.m3u8\n",
    )
    .unwrap();
    assert_packets(&master);
    master
}

fn assert_packets(input: &Path) {
    let result = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-count_packets",
            "-show_entries",
            "stream=nb_read_packets",
            "-of",
            "json",
        ])
        .arg(input)
        .output()
        .unwrap();
    assert!(result.status.success(), "packet count failed");
    let report: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["streams"][0]["nb_read_packets"], "240");
}

fn remote_files(source: &BTreeMap<PathBuf, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    source
        .iter()
        .map(|(name, bytes)| {
            let name = name.to_str().unwrap().replace('\\', "/");
            let bytes = if Path::new(&name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u8"))
            {
                let text = std::str::from_utf8(bytes).unwrap();
                text.lines()
                    .map(|line| {
                        if !line.starts_with('#') && !line.is_empty() {
                            format!("{line}?token={}", download_http::QUERY)
                        } else if let Some(uri) = line
                            .strip_prefix("#EXT-X-MAP:URI=\"")
                            .and_then(|s| s.strip_suffix('"'))
                        {
                            format!("#EXT-X-MAP:URI=\"{uri}?token={}\"", download_http::QUERY)
                        } else {
                            line.to_owned()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into_bytes()
                    .into_iter()
                    .chain(*b"\n")
                    .collect()
            } else {
                bytes.clone()
            };
            (format!("/{name}"), bytes)
        })
        .collect()
}

fn assert_private(result: &Output) {
    for bytes in [&result.stdout, &result.stderr] {
        let text = String::from_utf8_lossy(bytes);
        for secret in [KEY, download_http::QUERY, "download-auth-sentinel"] {
            assert!(!text.contains(secret), "secret in command output");
        }
    }
}

fn json_success(result: &Output) -> serde_json::Value {
    assert_private(result);
    assert!(
        result.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}

fn assert_verified(report: &serde_json::Value) {
    assert_eq!(report["passed"], true);
    assert_eq!(
        report["decode_evidence"],
        serde_json::json!({"status":"content-compared","equal":true})
    );
    assert_eq!(report["license_playback_verified"], false);
    assert_eq!(report["checks"]["stream_metadata"], "pass");
    for check in ["packet_payloads_equal", "packet_timing_equal"] {
        assert_eq!(report["checks"][check], true);
    }
}

fn convert(input: &Path, output: &Path, keys: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command
        .arg("convert")
        .arg("--decode")
        .arg(input)
        .args(["--to", "cbcs", "--format", "hls", "--output"])
        .arg(output)
        .arg("--keys-file")
        .arg(keys)
        .args(["--max-workers", "1", "--json"]);
    command
}

fn verify(output: &Path, source: &Path, keys: &Path) {
    let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("verify")
        .arg("--decode")
        .arg(output.join("master.m3u8"))
        .arg("--source")
        .arg(source)
        .arg("--keys-file")
        .arg(keys)
        .arg("--json")
        .output()
        .unwrap();
    assert_verified(&json_success(&result));
}

fn saved_plan_command(operation: &str, plan: &Path, keys: Option<&Path>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command
        .args([operation, "--plan"])
        .arg(plan)
        .args(["--max-workers", "1", "--json"]);
    if let Some(keys) = keys {
        command.arg("--keys-file").arg(keys);
    }
    command
}

fn preserves_raw(download: &Path, responses: &BTreeMap<String, Vec<u8>>) {
    let mut ledger =
        drmr_core::native::open_private_read(&download.join("raw/index.json")).unwrap();
    assert!(ledger.metadata().unwrap().is_file());
    let mut metadata = String::new();
    ledger.read_to_string(&mut metadata).unwrap();
    assert!(metadata.contains(download_http::QUERY));
    let saved = snapshot(download);
    for section in ["raw", "media"] {
        let mut expected: Vec<_> = responses
            .iter()
            .filter_map(|(name, bytes)| {
                let manifest = Path::new(name)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u8"));
                (manifest == (section == "raw")).then_some(bytes)
            })
            .collect();
        let mut actual: Vec<_> = saved
            .iter()
            .filter_map(|(path, bytes)| {
                let manifest = path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u8"));
                let original = if section == "raw" {
                    path.starts_with("raw") && manifest
                } else {
                    !path.starts_with("raw") && !manifest
                };
                original.then_some(bytes)
            })
            .collect();
        expected.sort();
        actual.sort();
        assert!(
            actual == expected,
            "original resource counts or bytes changed"
        );
    }
    for (name, bytes) in responses {
        let name = Path::new(name.strip_prefix('/').unwrap());
        if name.extension().is_none_or(|ext| ext != "m3u8") {
            assert!(
                saved.get(name) == Some(bytes),
                "source resource path or bytes changed"
            );
        }
    }
    let timing = |bytes: &[u8]| {
        std::str::from_utf8(bytes)
            .unwrap()
            .lines()
            .filter(|line| {
                line.starts_with("#EXTINF:") || line.starts_with("#EXT-X-MEDIA-SEQUENCE:")
            })
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let original = responses.get("/media.m3u8").unwrap();
    let expected = timing(original);
    assert!(
        saved
            .iter()
            .filter(|(path, _)| {
                !path.starts_with("raw")
                    && path
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u8"))
            })
            .any(|(_, bytes)| timing(bytes) == expected),
        "localized playlist timing literals changed"
    );
    assert_packets(&download.join("input.m3u8"));
}

fn download_and_reconvert(fragmented: bool) {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path();
    let source = fixture(&root.join("source"), fragmented);
    let original = snapshot(source.parent().unwrap());
    let responses = remote_files(&original);
    let server = download_http::Server::start(responses.clone());
    let policy = root.join("private/fetch.json");
    private_json(
        &policy,
        &serde_json::json!({
            "allowed_origins": [server.origin], "allow_http": true, "allow_private_network": true,
            "headers_by_origin": { &server.origin: { "Authorization": download_http::AUTH } }
        }),
    );
    let keys = root.join("private/keys.json");
    private_json(
        &keys,
        &serde_json::json!({"schema_version":1,
            "output":{"kid":KID,"key":KEY,"hls_key_uri":"https://operator.invalid/output"}}),
    );
    let download = root.join("download");
    let output = root.join("converted");
    let url = format!(
        "{}/master.m3u8?token={}",
        server.origin,
        download_http::QUERY
    );
    let mut command = convert(Path::new(&url), &output, &keys);
    command
        .arg("--download-dir")
        .arg(&download)
        .arg("--fetch-policy")
        .arg(&policy);
    if fragmented {
        command.arg("--measure");
    }
    let result = command.output().unwrap();
    let report = json_success(&result);
    assert_verified(&report["verification"]);
    if fragmented {
        assert_eq!(report["measurements"]["schema_version"], 1);
        assert!(
            report["measurements"]["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["calls"].as_u64().unwrap() > 0)
        );
        let preparation = report["measurements"]["phases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|phase| phase["phase"] == "input-preparation")
            .unwrap();
        assert!(preparation["scopes"].as_u64().unwrap() > 0);
        assert!(
            report["measurements"]["workspace"]["checkpoints"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(report["measurements"]["io"]["status"], "unmeasured");
    } else {
        assert!(report.get("measurements").is_none());
    }
    assert_eq!(report["download"]["entrypoint_relative"], "input.m3u8");
    assert_eq!(report["download"]["retained"], true);
    preserves_raw(&download, &responses);
    verify(&output, &source, &keys);
    assert_eq!(snapshot(source.parent().unwrap()), original);
    let counts = server.stop();
    assert_eq!(counts.refused, 0);
    assert!(counts.served >= responses.len());
    assert_packets(&download.join("input.m3u8"));
    let saved = snapshot(&download);
    let offline = root.join("offline");
    let report = json_success(
        &convert(&download.join("input.m3u8"), &offline, &keys)
            .output()
            .unwrap(),
    );
    assert_verified(&report["verification"]);
    verify(&offline, &source, &keys);
    assert_eq!(snapshot(&download), saved);
    assert_eq!(snapshot(source.parent().unwrap()), original);
}

#[test]
fn real_ts_hls_download_converts_verifies_and_reconverts_offline() {
    download_and_reconvert(false);
}

#[test]
fn real_fragmented_hls_download_converts_verifies_and_reconverts_offline() {
    download_and_reconvert(true);
}

#[test]
fn completed_download_survives_a_later_media_tool_failure_without_published_output() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path();
    let source = fixture(&root.join("source"), false);
    let original = snapshot(source.parent().unwrap());
    let responses = remote_files(&original);
    let server = download_http::Server::start(responses.clone());
    let policy = root.join("private/fetch.json");
    private_json(
        &policy,
        &serde_json::json!({"allowed_origins":[server.origin],
        "allow_http":true,"allow_private_network":true,
        "headers_by_origin":{&server.origin:{"Authorization":download_http::AUTH}}}),
    );
    let keys = root.join("private/keys.json");
    private_json(
        &keys,
        &serde_json::json!({"schema_version":1,
        "output":{"kid":KID,"key":KEY,"hls_key_uri":"https://operator.invalid/output"}}),
    );
    let download = root.join("download");
    let output = root.join("must-not-exist");
    let url = format!(
        "{}/master.m3u8?token={}",
        server.origin,
        download_http::QUERY
    );
    let result = convert(Path::new(&url), &output, &keys)
        .arg("--download-dir")
        .arg(&download)
        .arg("--fetch-policy")
        .arg(&policy)
        .arg("--packager-bin")
        .arg(root.join("missing-packager"))
        .output()
        .unwrap();
    assert_private(&result);
    assert_eq!(result.status.code(), Some(1));
    assert!(!output.exists());
    preserves_raw(&download, &responses);
    assert_eq!(snapshot(source.parent().unwrap()), original);
    let counts = server.stop();
    assert_eq!(counts.refused, 0);
    assert!(counts.served >= responses.len());
}

#[test]
fn url_analysis_saved_plan_execution_and_verification_use_the_retained_original_offline() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path();
    let source = fixture(&root.join("source"), false);
    let original = snapshot(source.parent().unwrap());
    let responses = remote_files(&original);
    let server = download_http::Server::start(responses.clone());
    let policy = root.join("private/fetch.json");
    private_json(
        &policy,
        &serde_json::json!({
            "allowed_origins": [server.origin], "allow_http": true, "allow_private_network": true,
            "headers_by_origin": { &server.origin: { "Authorization": download_http::AUTH } }
        }),
    );
    let download = root.join("download");
    let url = format!(
        "{}/master.m3u8?token={}",
        server.origin,
        download_http::QUERY
    );
    let analysis = json_success(
        &Command::new(env!("CARGO_BIN_EXE_drmr"))
            .args(["analyze", "--decode", &url, "--download-dir"])
            .arg(&download)
            .arg("--fetch-policy")
            .arg(&policy)
            .args(["--max-workers", "1", "--json", "--measure"])
            .output()
            .unwrap(),
    );
    assert_eq!(analysis["kind"], "input-analysis");
    assert_eq!(analysis["schema_version"], 2);
    assert_eq!(analysis["input_admitted"], true);
    assert_eq!(analysis["download"]["retained"], true);
    assert_eq!(analysis["measurements"]["schema_version"], 1);
    preserves_raw(&download, &responses);
    let retained = snapshot(&download);
    let counts = server.stop();
    assert_eq!(counts.refused, 0);
    assert!(counts.served >= responses.len());

    let plan = root.join("conversion.plan.json");
    let output = root.join("converted");
    let planned = json_success(
        &Command::new(env!("CARGO_BIN_EXE_drmr"))
            .arg("plan")
            .arg("--decode")
            .arg(download.join("input.m3u8"))
            .args(["--to", "cbcs", "--format", "dash", "--output"])
            .arg(&output)
            .arg("--save")
            .arg(&plan)
            .args(["--max-workers", "1", "--json"])
            .output()
            .unwrap(),
    );
    let saved = std::fs::read(&plan).unwrap();
    let on_disk: serde_json::Value = serde_json::from_slice(&saved).unwrap();
    assert_eq!(planned, on_disk);
    assert_eq!(planned["kind"], "execution-plan");
    assert_eq!(planned["schema_version"], 9);
    assert_eq!(planned["required_depth"], "decoded");
    assert!(planned["plan_id"].as_str().is_some_and(|id| !id.is_empty()));
    let text = std::str::from_utf8(&saved).unwrap();
    for secret in [KEY, download_http::QUERY, download_http::AUTH] {
        assert!(!text.contains(secret), "secret in saved execution plan");
    }
    assert!(!output.exists());

    let missing_keys = saved_plan_command("convert", &plan, None).output().unwrap();
    assert_private(&missing_keys);
    assert_eq!(missing_keys.status.code(), Some(2));
    let failure: serde_json::Value = serde_json::from_slice(&missing_keys.stdout).unwrap();
    assert_eq!(failure["error"]["code"], "output-key-role-mismatch");
    assert!(!output.exists());

    let keys = root.join("private/keys.json");
    private_json(
        &keys,
        &serde_json::json!({
            "schema_version": 1, "output": {"kid": KID, "key": KEY}
        }),
    );
    let converted = json_success(
        &saved_plan_command("convert", &plan, Some(&keys))
            .output()
            .unwrap(),
    );
    assert_eq!(converted["plan_id"], planned["plan_id"]);
    assert_verified(&converted["verification"]);
    let checked = json_success(
        &saved_plan_command("verify", &plan, Some(&keys))
            .output()
            .unwrap(),
    );
    assert_eq!(checked["plan_id"], planned["plan_id"]);
    assert_eq!(checked["source_unchanged"], true);
    assert_eq!(checked["output_matches_plan"], true);
    assert_verified(&checked["verification"]);
    assert_eq!(snapshot(&download), retained);
    assert_eq!(snapshot(source.parent().unwrap()), original);
    assert_eq!(std::fs::read(&plan).unwrap(), saved);
}
