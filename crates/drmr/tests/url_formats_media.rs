//! Native URL formats retain complete originals for offline, independently verified plans.
#[path = "support/download_http.rs"]
mod download_http;

use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Output},
};

// Public synthetic fixture material, independent of operator settings.
const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const IV: &str = "303132333435363738393a3b3c3d3e3f";

fn private_json(path: &Path, value: &Value) {
    drmr_core::native::create_private_new(path)
        .unwrap()
        .write_all(value.to_string().as_bytes())
        .unwrap();
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut result = BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        assert!(!path.is_symlink());
        if path.is_dir() {
            for (name, bytes) in snapshot(&path) {
                result.insert(Path::new(&entry.file_name()).join(name), bytes);
            }
        } else {
            result.insert(entry.file_name().into(), std::fs::read(&path).unwrap());
        }
    }
    result
}

fn fixture(root: &Path, format: &str, scheme: &str) -> PathBuf {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let mut command = Command::new("packager");
    command.arg("--single_threaded");
    command.current_dir(root);
    if format == "mp4" {
        command.arg(format!(
            "in={},stream=video,output=media.mp4",
            clip.display()
        ));
    } else {
        for role in ["video", "audio"] {
            command.arg(format!(
                "in={},stream={role},init_segment={role}-init.mp4,segment_template={role}-$Number$.m4s",
                clip.display()
            ));
        }
        command.args(["--generate_static_live_mpd", "--mpd_output", "manifest.mpd"]);
    }
    command.args([
        "--segment_duration",
        "2",
        "--fragment_duration",
        "2",
        "--mp4_reset_initial_composition_offset_to_zero=false",
    ]);
    if scheme != "clear" {
        command.args([
            "--enable_raw_key_encryption",
            "--keys",
            &format!("label=:key_id={KID}:key={KEY}"),
            "--iv",
            IV,
            "--protection_scheme",
            scheme,
            "--clear_lead",
            "0",
            "--protection_systems",
            "Widevine,PlayReady",
        ]);
    }
    assert!(
        command.output().unwrap().status.success(),
        "fixture packaging failed"
    );
    root.join(if format == "mp4" {
        "media.mp4"
    } else {
        "manifest.mpd"
    })
}

fn success(output: &Output) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    for secret in [KEY, IV, download_http::QUERY, download_http::AUTH] {
        assert!(
            !stdout.contains(secret) && !stderr.contains(secret),
            "secret in command output"
        );
    }
    assert!(output.status.success(), "command failed: {stdout} {stderr}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn command(operation: &str, keys: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command
        .arg(operation)
        .arg("--decode")
        .arg("--keys-file")
        .arg(keys)
        .args(["--max-workers", "1", "--json"]);
    command
}

fn verified(value: &Value) {
    assert_eq!(value["passed"], true);
    assert_eq!(
        value["decode_evidence"],
        serde_json::json!({"status":"content-compared","equal":true})
    );
    assert_eq!(value["license_playback_verified"], false);
    for name in ["packet_payloads_equal", "packet_timing_equal"] {
        assert_eq!(value["checks"][name], true);
    }
}

fn bundle_progress(output: &Output) {
    let text = std::str::from_utf8(&output.stderr).unwrap();
    let records: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    for activity in [
        "acquisition",
        "input-preparation",
        "verification",
        "publication",
    ] {
        assert!(
            records.iter().any(|v| v["activity"] == activity),
            "missing {activity}: {text}"
        );
    }
    assert_eq!(records.last().unwrap()["event"], "finished");
    assert_eq!(records.last().unwrap()["activity"], "publication");
}

fn remote_files(original: &BTreeMap<PathBuf, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    original
        .iter()
        .map(|(name, bytes)| {
            let bytes = if name.extension().is_some_and(|ext| ext == "mpd") {
                // Every referenced URL must carry its own query. Manifest query
                // parameters do not implicitly propagate to its relative children.
                String::from_utf8(bytes.clone())
                    .unwrap()
                    .replace(".mp4\"", &format!(".mp4?token={}\"", download_http::QUERY))
                    .replace(".m4s\"", &format!(".m4s?token={}\"", download_http::QUERY))
                    .into_bytes()
            } else {
                bytes.clone()
            };
            (
                format!("/{}", name.to_str().unwrap().replace('\\', "/")),
                bytes,
            )
        })
        .collect()
}

fn refuse_missing_and_wrong_keys(root: &Path, input: &Path) {
    let missing = Command::new(env!("CARGO_BIN_EXE_drmr"))
        .arg("analyze")
        .arg("--decode")
        .arg(input)
        .arg("--json")
        .output()
        .unwrap();
    assert!(
        !missing.status.success(),
        "encrypted originals require separately supplied keys"
    );
    let wrong = root.join("wrong-keys.json");
    private_json(
        &wrong,
        &json!({"schema_version":1,"input":{"keys":[{
            "kid":KID,"key":"404142434445464748494a4b4c4d4e4f"
        }]}}),
    );
    let rejected = command("analyze", &wrong).arg(input).output().unwrap();
    assert!(
        !rejected.status.success(),
        "wrong keys must fail media admission"
    );
    for result in [&missing, &rejected] {
        for output in [&result.stdout, &result.stderr] {
            let text = String::from_utf8_lossy(output);
            for secret in [
                KEY,
                IV,
                "404142434445464748494a4b4c4d4e4f",
                download_http::AUTH,
                download_http::QUERY,
            ] {
                assert!(!text.contains(secret));
            }
        }
    }
}

fn saved_plan_workflow(root: &Path, input: &Path, keys: &Path, target: &str) {
    let plan = root.join("conversion.plan.json");
    let output = root.join("converted");
    let planned = success(
        &command("plan", keys)
            .arg(input)
            .args(["--to", target, "--format", "dash", "--output"])
            .arg(&output)
            .arg("--save")
            .arg(&plan)
            .output()
            .unwrap(),
    );
    let saved = std::fs::read(&plan).unwrap();
    let plan_text = String::from_utf8(saved.clone()).unwrap();
    for secret in [KEY, IV, download_http::QUERY, download_http::AUTH] {
        assert!(
            !plan_text.contains(secret),
            "saved plan contains confidential input"
        );
    }
    let converted = success(
        &command("convert", keys)
            .arg("--plan")
            .arg(&plan)
            .output()
            .unwrap(),
    );
    assert_eq!(converted["plan_id"], planned["plan_id"]);
    verified(&converted["verification"]);
    let checked = success(
        &command("verify", keys)
            .arg("--plan")
            .arg(&plan)
            .output()
            .unwrap(),
    );
    assert_eq!(checked["plan_id"], planned["plan_id"]);
    assert_eq!(checked["source_unchanged"], true);
    assert_eq!(checked["output_matches_plan"], true);
    verified(&checked["verification"]);
    assert_eq!(std::fs::read(&plan).unwrap(), saved);
}

fn authenticated_policy(root: &Path, origin: &str) -> PathBuf {
    let path = root.join("fetch.json");
    private_json(
        &path,
        &json!({
            "allowed_origins":[origin], "allow_http":true, "allow_private_network":true,
            "headers_by_origin":{origin:{"Authorization":download_http::AUTH}}
        }),
    );
    path
}

fn offline_workflow(format: &str, scheme: &str, prefix: &[u8]) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = fixture(&root.path().join("source"), format, scheme);
    if !prefix.is_empty() {
        let mut body = std::fs::read(&source).unwrap();
        if format == "dash" && body.starts_with(b"<?xml") {
            // An XML declaration must precede comments when present. Removing
            // the optional declaration lets the fixture begin with its comment.
            let end = body.windows(2).position(|pair| pair == b"?>").unwrap() + 2;
            body.drain(..end);
        }
        let mut bytes = prefix.to_vec();
        bytes.extend(body);
        std::fs::write(&source, bytes).unwrap();
    }
    let original = snapshot(source.parent().unwrap());
    let responses = remote_files(&original);
    let server = download_http::Server::start(responses.clone());
    let keys = root.path().join("keys.json");
    let input_keys = if scheme == "clear" {
        json!([])
    } else {
        json!([{ "kid": KID, "key": KEY }])
    };
    let mut key_config = json!({"schema_version":1,"input":{"keys":input_keys}});
    let target = if scheme == "clear" {
        key_config["output"] = json!({"kid":KID,"key":KEY});
        "cbcs"
    } else {
        "clear"
    };
    private_json(&keys, &key_config);
    let policy = authenticated_policy(root.path(), &server.origin);
    let download = root.path().join("download");
    let url = format!(
        "{}/{}?token={}",
        server.origin,
        source.file_name().unwrap().to_str().unwrap(),
        download_http::QUERY
    );
    let analysis = success(
        &command("analyze", &keys)
            .arg(&url)
            .arg("--download-dir")
            .arg(&download)
            .arg("--fetch-policy")
            .arg(&policy)
            .output()
            .unwrap(),
    );
    assert_eq!(analysis["input_admitted"], true);
    assert_eq!(analysis["actual_format"], format);
    let entrypoint = if format == "mp4" {
        "input.mp4"
    } else {
        "input.mpd"
    };
    assert_eq!(analysis["download"]["entrypoint_relative"], entrypoint);
    assert_eq!(analysis["download"]["retained"], true);
    let retained = snapshot(&download);
    if format == "mp4" {
        assert_eq!(
            retained[Path::new(entrypoint)],
            original[Path::new("media.mp4")]
        );
    } else {
        assert_eq!(
            retained[Path::new("raw/r000000.mpd")],
            responses["/manifest.mpd"]
        );
        assert_eq!(analysis["input"]["tracks"].as_array().unwrap().len(), 2);
    }
    let counts = server.stop();
    assert_eq!(counts.refused, 0);
    assert_eq!(counts.served, responses.len());
    if scheme != "clear" {
        refuse_missing_and_wrong_keys(root.path(), &download.join(entrypoint));
    }

    saved_plan_workflow(root.path(), &download.join(entrypoint), &keys, target);
    assert_eq!(snapshot(&download), retained);
    assert_eq!(snapshot(source.parent().unwrap()), original);
    eprintln!(
        "native URL format={format} input={scheme}: offline plan, conversion, packet/timing/decoded verification passed"
    );
}

#[test]
fn mp4_urls_clear_cenc_and_cbcs_support_offline_plans_and_independent_verification() {
    for scheme in ["clear", "cenc", "cbcs"] {
        offline_workflow("mp4", scheme, &[]);
    }
}

#[test]
fn dash_urls_clear_cenc_and_cbcs_support_offline_plans_and_independent_verification() {
    for scheme in ["clear", "cenc", "cbcs"] {
        offline_workflow("dash", scheme, &[]);
    }
}

#[test]
fn dash_urls_with_a_leading_xml_comment_keep_the_complete_offline_workflow() {
    offline_workflow("dash", "clear", b"<!--widevine information-->\n");
}

#[test]
fn mp4_urls_with_supported_leading_boxes_keep_the_complete_offline_workflow() {
    for kind in [b"wide", b"mdat", b"sidx"] {
        let size: u32 = if kind == b"sidx" { 32 } else { 8 };
        let mut prefix = size.to_be_bytes().to_vec();
        prefix.extend(kind);
        if kind == b"sidx" {
            // A version-zero index with no references; the following fragments
            // retain their own relative offsets and complete media payload.
            for value in [0_u32, 1, 90_000, 0, 0, 0] {
                prefix.extend(value.to_be_bytes());
            }
        }
        offline_workflow("mp4", "clear", &prefix);
    }
}

#[test]
fn mp4_and_dash_urls_share_direct_single_and_multiple_output_conversion() {
    for format in ["mp4", "dash"] {
        let root = drmr_core::native::private_tempdir().unwrap();
        let source = fixture(&root.path().join("source"), format, "clear");
        let original = snapshot(source.parent().unwrap());
        let responses = remote_files(&original);
        let server = download_http::Server::start(responses.clone());
        let keys = root.path().join("keys.json");
        private_json(&keys, &json!({"schema_version":1}));
        let bundle_keys = root.path().join("bundle-keys.json");
        private_json(&bundle_keys, &json!({"schema_version":2,"outputs":{}}));
        let policy = authenticated_policy(root.path(), &server.origin);
        let url = format!(
            "{}/{}?token={}",
            server.origin,
            source.file_name().unwrap().to_str().unwrap(),
            download_http::QUERY
        );
        let single_download = root.path().join("single-original");
        let single_output = root.path().join("single-output");
        let single = success(
            &command("convert", &keys)
                .arg(&url)
                .args(["--to", "clear", "--format", "dash", "--output"])
                .arg(&single_output)
                .arg("--download-dir")
                .arg(&single_download)
                .arg("--fetch-policy")
                .arg(&policy)
                .output()
                .unwrap(),
        );
        verified(&single["verification"]);
        let entrypoint = if format == "mp4" {
            "input.mp4"
        } else {
            "input.mpd"
        };
        assert_eq!(single["download"]["entrypoint_relative"], entrypoint);
        let retained_single = snapshot(&single_download);
        let outputs = root.path().join("outputs.json");
        private_json(
            &outputs,
            &json!({"schema_version":1,"outputs":[
                {"id":"dash","format":"dash","target":"clear","drms":[]},
                {"id":"hls","format":"hls","target":"clear","drms":[]}
            ]}),
        );
        let bundle_download = root.path().join("bundle-original");
        let bundle_output = root.path().join("bundle-output");
        let bundle_result = command("convert", &bundle_keys)
            .arg(&url)
            .arg("--outputs")
            .arg(&outputs)
            .arg("--output")
            .arg(&bundle_output)
            .arg("--download-dir")
            .arg(&bundle_download)
            .arg("--fetch-policy")
            .arg(&policy)
            .args(["--progress", "json"])
            .output()
            .unwrap();
        let bundle = success(&bundle_result);
        bundle_progress(&bundle_result);
        assert_eq!(bundle["kind"], "bundle-conversion");
        assert_eq!(bundle["outputs"].as_array().unwrap().len(), 2);
        for child in bundle["outputs"].as_array().unwrap() {
            verified(&child["conversion"]["verification"]);
        }
        let retained_bundle = snapshot(&bundle_download);
        let counts = server.stop();
        assert_eq!(counts.refused, 0);
        assert_eq!(counts.served, responses.len() * 2);
        for (output, source) in [
            (
                single_output.join("manifest.mpd"),
                single_download.join(entrypoint),
            ),
            (
                bundle_output.join("dash/manifest.mpd"),
                bundle_download.join(entrypoint),
            ),
            (
                bundle_output.join("hls/master.m3u8"),
                bundle_download.join(entrypoint),
            ),
        ] {
            verified(&success(
                &command("verify", &keys)
                    .arg(output)
                    .arg("--source")
                    .arg(source)
                    .output()
                    .unwrap(),
            ));
        }
        assert_eq!(snapshot(&single_download), retained_single);
        assert_eq!(snapshot(&bundle_download), retained_bundle);
        assert_eq!(snapshot(source.parent().unwrap()), original);
    }
}
