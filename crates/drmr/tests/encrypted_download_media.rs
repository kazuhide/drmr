//! Encrypted URL acquisition retains ciphertext for offline admission and saved plans.
#[path = "support/download_http.rs"]
mod download_http;

use drmr_core::conversion::{Aes128Key, SegmentIv, encrypt_segment};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

// Public synthetic fixture material; no operator configuration is reused.
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const ROTATED: &str = "202122232425262728292a2b2c2d2e2f";
const WRONG: &str = "404142434445464748494a4b4c4d4e4f";
const KID: &str = "000102030405060708090a0b0c0d0e0f";
const IV: &str = "303132333435363738393a3b3c3d3e3f";
const SKD: &str = "skd://operator.invalid/encrypted-url-fixture";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    AesTs,
    AesFmp4,
    SampleTs,
    CbcsFmp4,
    CencFmp4,
}

fn private_json(path: &Path, value: &Value) {
    drmr_core::native::create_private_new(path)
        .unwrap()
        .write_all(value.to_string().as_bytes())
        .unwrap();
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut values = BTreeMap::new();
    for entry in std::fs::read_dir(root).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            for (name, bytes) in snapshot(&path) {
                values.insert(Path::new(path.file_name().unwrap()).join(name), bytes);
            }
        } else {
            assert!(!path.is_symlink());
            values.insert(
                path.file_name().unwrap().into(),
                std::fs::read(&path).unwrap(),
            );
        }
    }
    values
}

fn package_fixture(root: &Path, profile: Profile) {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let ts = matches!(profile, Profile::AesTs | Profile::SampleTs);
    let descriptor = format!(
        "input={},stream=video,{}segment_template={},playlist_name=media.m3u8",
        input.display(),
        if ts {
            String::new()
        } else {
            format!("init_segment={},", root.join("init.mp4").display())
        },
        root.join(if ts {
            "segment-$Number$.ts"
        } else {
            "segment-$Number$.m4s"
        })
        .display()
    );
    let mut command = Command::new("packager");
    command
        .arg(descriptor)
        .args(["--hls_master_playlist_output"])
        .arg(root.join("master.m3u8"))
        .args([
            "--segment_duration",
            "2",
            "--fragment_duration",
            "2",
            "--transport_stream_timestamp_offset_ms",
            "0",
            "--mp4_reset_initial_composition_offset_to_zero=false",
        ]);
    if matches!(
        profile,
        Profile::SampleTs | Profile::CbcsFmp4 | Profile::CencFmp4
    ) {
        command.args([
            "--enable_raw_key_encryption",
            "--keys",
            &format!("label=:key_id={KID}:key={KEY}"),
            "--iv",
            IV,
            "--protection_scheme",
            if profile == Profile::CencFmp4 {
                "cenc"
            } else {
                "cbcs"
            },
            "--clear_lead",
            "0",
            "--hls_key_uri",
            SKD,
        ]);
    }
    if profile == Profile::AesTs {
        let result = Command::new("ffmpeg")
            .current_dir(root)
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(&input)
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
                "37",
                "-hls_segment_filename",
                "segment-%d.ts",
                "media.m3u8",
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "required transport fixture generation failed"
        );
        std::fs::write(
            root.join("master.m3u8"),
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nmedia.m3u8\n",
        )
        .unwrap();
    } else {
        assert!(
            command.output().unwrap().status.success(),
            "required encrypted fixture generation failed"
        );
    }
}

fn fixture(root: &Path, profile: Profile) -> PathBuf {
    package_fixture(root, profile);
    let ts = matches!(profile, Profile::AesTs | Profile::SampleTs);
    let media = root.join("media.m3u8");
    let original = std::fs::read_to_string(&media).unwrap();
    let mut rewritten = String::new();
    let mut ordinal = 0;
    for line in original.lines() {
        if profile == Profile::AesTs && line.starts_with("#EXT-X-MEDIA-SEQUENCE:") {
            rewritten.push_str("#EXT-X-MEDIA-SEQUENCE:37\n");
        } else if profile == Profile::AesFmp4 && line.starts_with("#EXT-X-MAP:") {
            let clear = root.join("init.mp4");
            let encrypted = root.join("encrypted-init.mp4");
            encrypt_segment(
                &clear,
                &encrypted,
                &Aes128Key::from_hex(KEY).unwrap(),
                &SegmentIv::for_sequence(1),
            )
            .unwrap();
            writeln!(rewritten, "#EXT-X-KEY:METHOD=AES-128,URI=\"key.bin\",IV=0x1\n#EXT-X-MAP:URI=\"encrypted-init.mp4\"").unwrap();
            std::fs::remove_file(clear).unwrap();
        } else if !line.starts_with('#') && !line.is_empty() {
            ordinal += 1;
            if matches!(profile, Profile::AesTs | Profile::AesFmp4) {
                let (uri, key) = if profile == Profile::AesTs && ordinal > 2 {
                    ("rotated.bin", ROTATED)
                } else {
                    ("key.bin", KEY)
                };
                let iv = if profile == Profile::AesTs {
                    SegmentIv::for_sequence(36 + ordinal)
                } else {
                    SegmentIv::for_sequence(1)
                };
                let name = format!("encrypted-{ordinal}.{}", if ts { "ts" } else { "m4s" });
                encrypt_segment(
                    &root.join(line),
                    &root.join(&name),
                    &Aes128Key::from_hex(key).unwrap(),
                    &iv,
                )
                .unwrap();
                let suffix = if profile == Profile::AesFmp4 {
                    ",IV=0x1"
                } else {
                    ""
                };
                writeln!(
                    rewritten,
                    "#EXT-X-KEY:METHOD=AES-128,URI=\"{uri}\"{suffix}\n{name}"
                )
                .unwrap();
                std::fs::remove_file(root.join(line)).unwrap();
            } else {
                writeln!(rewritten, "{line}").unwrap();
            }
        } else {
            let line = if profile == Profile::SampleTs {
                line.replace(
                    "KEYFORMAT=\"identity\"",
                    "KEYFORMAT=\"com.apple.streamingkeydelivery\"",
                )
            } else {
                line.to_owned()
            };
            writeln!(rewritten, "{line}").unwrap();
        }
    }
    assert_eq!(
        ordinal, 5,
        "BBB fixture must contain five two-second segments"
    );
    std::fs::write(media, rewritten).unwrap();
    root.join("master.m3u8")
}

fn input_keys(profile: Profile, origin: Option<&str>, wrong: bool) -> Value {
    let key = if wrong { WRONG } else { KEY };
    if matches!(profile, Profile::CbcsFmp4 | Profile::CencFmp4) {
        return json!({"keys":[{"kid":KID,"key":key}]});
    }
    if profile == Profile::SampleTs {
        return json!({"hls_keys":[{"uri":SKD,"key":key}]});
    }
    let uri =
        |name: &str| origin.map_or_else(|| name.to_owned(), |origin| format!("{origin}/{name}"));
    let mut values = vec![json!({"uri":uri("key.bin"),"key":key})];
    if profile == Profile::AesTs {
        values.push(json!({"uri":uri("rotated.bin"),"key":if wrong { WRONG } else { ROTATED }}));
    }
    json!({"hls_keys":values})
}

fn invoke(operation: &str, input: &Path, keys: Option<&Path>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command
        .arg(operation)
        .arg(input)
        .args(["--max-workers", "1", "--json"]);
    if let Some(keys) = keys {
        command.arg("--keys-file").arg(keys);
    }
    command
}
fn checked(output: &Output) -> Value {
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        for private in [
            KEY,
            ROTATED,
            WRONG,
            IV,
            SKD,
            download_http::QUERY,
            download_http::AUTH,
        ] {
            assert!(
                !text.contains(private),
                "operator material leaked into CLI output"
            );
        }
    }
    serde_json::from_slice(&output.stdout).unwrap()
}
#[track_caller]
fn success(output: &Output) -> Value {
    let report = checked(output);
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        if line.starts_with("DRMR_PROBE ") {
            eprintln!("{line}");
        }
    }
    assert!(
        output.status.success(),
        "command failed with classification {}",
        report["error"]["code"]
    );
    report
}
fn verified(report: &Value) {
    assert_eq!(report["passed"], true);
    assert_eq!(report["media_verified"], true);
    assert_eq!(report["license_playback_verified"], false);
    for name in [
        "packet_payloads_equal",
        "packet_timing_equal",
        "decoded_content_equal",
    ] {
        assert_eq!(report["checks"][name], true);
    }
}

fn native_verified(report: &Value) {
    assert_eq!(report["native_manifest"]["scope"], "manifest-only");
    assert_eq!(report["native_manifest"]["errors"], 0);
    assert_eq!(report["native_manifest"]["incomplete"], false);
    assert_eq!(report["native_manifest"]["truncated"], false);
}

fn remote_files(original: &BTreeMap<PathBuf, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    original
        .iter()
        .map(|(name, bytes)| {
            let bytes = if name.extension().is_some_and(|ext| ext == "m3u8") {
                std::str::from_utf8(bytes)
                    .unwrap()
                    .lines()
                    .map(|line| {
                        if !line.starts_with('#') && !line.is_empty() {
                            format!("{line}?token={}", download_http::QUERY)
                        } else if let Some(uri) = line
                            .strip_prefix("#EXT-X-MAP:URI=\"")
                            .and_then(|value| value.strip_suffix('"'))
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
            (
                format!("/{}", name.to_str().unwrap().replace('\\', "/")),
                bytes,
            )
        })
        .collect()
}

fn encrypted_url_roundtrip(profile: Profile) {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = fixture(&root.path().join("source"), profile);
    let original = snapshot(source.parent().unwrap());
    let local_keys = root.path().join("local-keys.json");
    private_json(
        &local_keys,
        &json!({"schema_version":1,"input":input_keys(profile,None,false)}),
    );
    let local = success(
        &invoke("analyze", &source, Some(&local_keys))
            .output()
            .unwrap(),
    );
    assert_eq!(local["input_admitted"], true);
    let responses = remote_files(&original);
    let server = download_http::Server::start(responses.clone());
    let origin = server.origin.clone();
    let keys = root.path().join("keys.json");
    private_json(
        &keys,
        &json!({"schema_version":2,"input":input_keys(profile,Some(&server.origin),false),"outputs":{}}),
    );
    let policy = root.path().join("fetch.json");
    private_json(
        &policy,
        &json!({"allowed_origins":[server.origin],"allow_http":true,"allow_private_network":true,
        "headers_by_origin":{&server.origin:{"Authorization":download_http::AUTH}}}),
    );
    let download = root.path().join("download");
    let url = format!(
        "{}/master.m3u8?token={}",
        server.origin,
        download_http::QUERY
    );
    let report = success(
        &invoke("analyze", Path::new(&url), Some(&keys))
            .arg("--download-dir")
            .arg(&download)
            .arg("--fetch-policy")
            .arg(&policy)
            .output()
            .unwrap(),
    );
    assert_eq!(report["input_admitted"], true);
    assert_eq!(report["download"]["retained"], true);
    let counts = server.stop();
    assert_eq!(
        counts.refused, 0,
        "key or license URI must never be requested"
    );
    assert_eq!(counts.served, responses.len());
    let retained = snapshot(&download);
    let raw: Vec<_> = retained
        .iter()
        .filter(|(name, _)| {
            name.starts_with("raw") && name.extension().is_some_and(|ext| ext == "m3u8")
        })
        .map(|(_, bytes)| bytes)
        .collect();
    for (name, bytes) in &responses {
        let suffix = Path::new(name).extension().unwrap();
        if suffix == "m3u8" {
            assert!(raw.contains(&bytes));
        } else {
            assert!(
                retained
                    .iter()
                    .any(|(name, actual)| name.starts_with("media") && actual == bytes)
            );
        }
    }
    let input = download.join("input.m3u8");
    rejected_keys(root.path(), profile, &input, &origin);
    if profile == Profile::SampleTs {
        refuse_ts_counter_mode(&download, &input, &keys);
    }
    if profile == Profile::AesTs {
        refuse_single_output_bundle_keys(root.path(), &input, &keys);
    }
    offline_plan(root.path(), &input, &keys);
    if profile == Profile::CencFmp4 {
        cenc_all_qualified_outputs(root.path(), &input);
    }
    assert_eq!(snapshot(&download), retained);
    assert_eq!(snapshot(source.parent().unwrap()), original);
}

fn refuse_ts_counter_mode(download: &Path, input: &Path, keys: &Path) {
    let playlist = download.join("p000001.m3u8");
    let original = std::fs::read_to_string(&playlist).unwrap();
    assert!(original.contains("METHOD=SAMPLE-AES,"));
    std::fs::write(
        &playlist,
        original.replace("METHOD=SAMPLE-AES,", "METHOD=SAMPLE-AES-CTR,"),
    )
    .unwrap();
    let refused = invoke("analyze", input, Some(keys)).output().unwrap();
    assert!(!refused.status.success());
    assert_eq!(
        checked(&refused)["error"]["code"],
        "ts-sample-key-declaration-not-supported"
    );
    std::fs::write(playlist, original).unwrap();
}

fn refuse_single_output_bundle_keys(root: &Path, input: &Path, keys: &Path) {
    let single = root.join("single-must-not-exist");
    let refused = invoke("convert", input, Some(keys))
        .args(["--to", "clear", "--format", "dash", "--output"])
        .arg(&single)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(checked(&refused)["error"]["code"], "key-file-invalid");
    assert!(!single.exists());
}

fn rejected_keys(root: &Path, profile: Profile, input: &Path, origin: &str) {
    for (name, supplied) in [("missing", None), ("wrong", Some(root.join("wrong.json")))] {
        if let Some(path) = &supplied {
            private_json(
                path,
                &json!({"schema_version":2,"input":input_keys(profile,Some(origin),true),"outputs":{}}),
            );
        }
        let result = invoke("analyze", input, supplied.as_deref())
            .output()
            .unwrap();
        checked(&result);
        assert!(
            !result.status.success(),
            "{name} keys must fail input admission"
        );
    }
    if matches!(profile, Profile::AesTs | Profile::AesFmp4) {
        let path = root.join("relative-keys.json");
        private_json(
            &path,
            &json!({"schema_version":2,"input":input_keys(profile,None,false),"outputs":{}}),
        );
        let result = invoke("analyze", input, Some(&path)).output().unwrap();
        assert!(
            !result.status.success(),
            "relative bindings must not match a resolved remote identifier"
        );
        assert_eq!(checked(&result)["error"]["code"], "hls-input-key-required");
    }
}

fn offline_plan(root: &Path, input: &Path, keys: &Path) {
    let plan = root.join("conversion.plan.json");
    let output = root.join("converted");
    let outputs = root.join("outputs.json");
    private_json(
        &outputs,
        &json!({"schema_version":1,"outputs":[{"id":"dash","format":"dash","target":"clear","drms":[]}]}),
    );
    let planned = success(
        &invoke("plan", input, Some(keys))
            .arg("--outputs")
            .arg(&outputs)
            .arg("--output")
            .arg(&output)
            .arg("--save")
            .arg(&plan)
            .output()
            .unwrap(),
    );
    assert!(!output.exists());
    let saved = std::fs::read(&plan).unwrap();
    for secret in [KEY, ROTATED, WRONG, IV, SKD, download_http::QUERY] {
        assert!(!std::str::from_utf8(&saved).unwrap().contains(secret));
    }
    for operation in ["convert", "verify"] {
        let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
            .args([operation, "--plan"])
            .arg(&plan)
            .arg("--keys-file")
            .arg(keys)
            .args(["--max-workers", "1", "--json"])
            .output()
            .unwrap();
        let result = success(&result);
        assert_eq!(result["plan_id"], planned["plan_id"]);
        if operation == "verify" {
            verified(&result["outputs"][0]["verification"]);
            native_verified(&result["outputs"][0]["verification"]);
            assert_eq!(result["source_unchanged"], true);
            assert_eq!(result["outputs"][0]["output_matches_plan"], true);
        } else {
            verified(&result["outputs"][0]["conversion"]["verification"]);
            native_verified(&result["outputs"][0]["conversion"]["verification"]);
        }
    }
    assert_eq!(std::fs::read(plan).unwrap(), saved);
}

// Nine qualified combinations use two saved plans; the eight-output admission cap is unchanged.
fn cenc_all_qualified_outputs(root: &Path, input: &Path) {
    let routes = [
        ("mp4", "clear"),
        ("mp4", "cenc"),
        ("mp4", "cbcs"),
        ("dash", "clear"),
        ("dash", "cenc"),
        ("dash", "cbcs"),
        ("hls", "clear"),
        ("hls", "cbcs"),
        ("hls", "hls-aes128"),
    ];
    for (batch, routes) in routes.chunks(8).enumerate() {
        let plan = root.join(format!("cenc-matrix-{batch}.plan.json"));
        let output = root.join(format!("cenc-matrix-{batch}"));
        let spec = root.join(format!("cenc-matrix-{batch}.outputs.json"));
        let keys = root.join(format!("cenc-matrix-{batch}.keys.json"));
        let mut output_keys = serde_json::Map::new();
        let mut intents = Vec::new();
        for &(format, target) in routes {
            let id = format!("{format}-{target}");
            intents.push(json!({"id":id,"format":format,"target":target,"drms":[]}));
            if target != "clear" {
                let value = if target == "hls-aes128" {
                    json!({"key":ROTATED,"hls_key_uri":"https://operator.invalid/output.key"})
                } else {
                    let mut value = json!({"kid":"a0a1a2a3a4a5a6a7a8a9aaabacadaeaf","key":ROTATED});
                    if format == "hls" {
                        value["hls_key_uri"] = SKD.into();
                    }
                    value
                };
                output_keys.insert(id, value);
            }
        }
        private_json(&spec, &json!({"schema_version":1,"outputs":intents}));
        private_json(
            &keys,
            &json!({"schema_version":2,"input":input_keys(Profile::CencFmp4,None,false),"outputs":output_keys}),
        );
        let planned = success(
            &invoke("plan", input, Some(&keys))
                .arg("--outputs")
                .arg(&spec)
                .arg("--output")
                .arg(&output)
                .arg("--save")
                .arg(&plan)
                .output()
                .unwrap(),
        );
        assert_eq!(planned["outputs"].as_array().unwrap().len(), routes.len());
        assert!(!output.exists());
        let saved = std::fs::read(&plan).unwrap();
        for secret in [KEY, ROTATED, IV, SKD] {
            assert!(!std::str::from_utf8(&saved).unwrap().contains(secret));
        }
        for operation in ["convert", "verify"] {
            let result = Command::new(env!("CARGO_BIN_EXE_drmr"))
                .args([operation, "--plan"])
                .arg(&plan)
                .arg("--keys-file")
                .arg(&keys)
                .args(["--max-workers", "1", "--json"])
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "CENC matrix batch {batch} {operation} failed: {}",
                checked(&result)["error"]["code"]
            );
            let result = success(&result);
            assert_eq!(result["plan_id"], planned["plan_id"]);
            let outputs = result["outputs"].as_array().unwrap();
            assert_eq!(outputs.len(), routes.len());
            for (report, &(format, target)) in outputs.iter().zip(routes) {
                assert_eq!(report["id"], format!("{format}-{target}"));
                let verification = if operation == "verify" {
                    &report["verification"]
                } else {
                    &report["conversion"]["verification"]
                };
                verified(verification);
                if format == "mp4" {
                    assert!(verification["native_manifest"].is_null());
                } else {
                    native_verified(verification);
                }
                if operation == "verify" {
                    assert_eq!(report["output_matches_plan"], true);
                }
            }
            if operation == "verify" {
                assert_eq!(result["source_unchanged"], true);
            }
        }
        assert_eq!(std::fs::read(&plan).unwrap(), saved);
    }
}

#[test]
fn aes128_ts_url_retains_rotation_and_implicit_sequence_vectors_for_offline_plans() {
    encrypted_url_roundtrip(Profile::AesTs);
}
#[test]
fn aes128_fragmented_url_retains_short_explicit_vectors_for_offline_plans() {
    encrypted_url_roundtrip(Profile::AesFmp4);
}
#[test]
fn sample_aes_ts_url_retains_fairplay_identifiers_for_offline_plans() {
    encrypted_url_roundtrip(Profile::SampleTs);
}
#[test]
fn cbcs_fragmented_url_uses_mp4_input_key_ids_for_offline_plans() {
    encrypted_url_roundtrip(Profile::CbcsFmp4);
}
#[test]
fn cenc_fragmented_url_matches_existing_local_input_admission_for_offline_plans() {
    encrypted_url_roundtrip(Profile::CencFmp4);
}
