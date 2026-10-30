//! Native CLI plans, direct outputs and retained HTTP input share HLS DRM semantics.
#[path = "support/download_http.rs"]
mod download_http;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    process::{Command, Output},
};
const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const LICENSE: &str = "https://license.example.invalid/hls-private-marker";
const URI: &str = "skd://hls-private-marker";
const WV: &str = "000000347073736801000000edef8ba979d64acea3c827dcd51d21ed00000001000102030405060708090a0b0c0d0e0f00000000";
fn path(value: &Path) -> String {
    drmr_core::manifest_support::path::fspath(value).unwrap()
}
fn write(path: &Path, value: &Value) {
    serde_json::to_writer(drmr_core::native::create_private_new(path).unwrap(), value).unwrap();
}
fn invoke(args: &[String]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(args)
        .args(["--json", "--progress", "off"])
        .output()
        .unwrap()
}
fn success(output: &Output, secrets: &[String]) -> Value {
    for bytes in [&output.stdout, &output.stderr] {
        let text = String::from_utf8_lossy(bytes);
        for secret in [
            KEY,
            LICENSE,
            URI,
            WV,
            download_http::QUERY,
            download_http::AUTH,
        ]
        .into_iter()
        .chain(secrets.iter().map(String::as_str))
        {
            assert!(
                !text.contains(secret),
                "private operator metadata appeared in command output"
            );
        }
    }
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn cli(args: &[String]) -> Value {
    success(&invoke(args), &[])
}
fn checked(report: &Value) {
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(report["required_depth"], "native");
    assert_eq!(
        report["decode_evidence"],
        json!({"status":"not-checked","reason":"not-requested"})
    );
    assert_eq!(report["license_playback_verified"], false);
}
fn settings(fairplay: bool) -> Value {
    let mut output = json!({"kid":KID,"key":KEY,"signaling":{"system":"multi-drm","widevine_pssh":WV,"playready_license_url":LICENSE,"fairplay":fairplay}});
    if fairplay {
        output["hls_key_uri"] = URI.into();
    }
    output
}
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = BTreeMap::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let file = entry.unwrap().path();
            assert!(!file.is_symlink());
            if file.is_dir() {
                pending.push(file);
            } else {
                files.insert(
                    file.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(file).unwrap(),
                );
            }
        }
    }
    files
}
fn bundle(root: &Path, reference: &Path) -> PathBuf {
    let recipe = root.join("outputs.json");
    let keys = root.join("bundle-keys.json");
    let saved = root.join("bundle-plan.json");
    let output = root.join("bundle");
    write(
        &recipe,
        &json!({"schema_version":1,"outputs":[
        {"id":"cbc","format":"hls","target":"cbcs","drms":["widevine","playready","fairplay"]},
        {"id":"ctr","format":"hls","target":"cenc","drms":["widevine","playready"]}]}),
    );
    write(
        &keys,
        &json!({"schema_version":2,"outputs":{"cbc":settings(true),"ctr":settings(false)}}),
    );
    cli(&[
        "plan".into(),
        path(reference),
        "--outputs".into(),
        path(&recipe),
        "--output".into(),
        path(&output),
        "--save".into(),
        path(&saved),
    ]);
    let converted = cli(&[
        "convert".into(),
        "--plan".into(),
        path(&saved),
        "--keys-file".into(),
        path(&keys),
    ]);
    for output in converted["outputs"].as_array().unwrap() {
        checked(&output["conversion"]["verification"]);
        assert_eq!(output["signaling"]["verified"], true);
    }
    let verified = cli(&[
        "verify".into(),
        "--plan".into(),
        path(&saved),
        "--keys-file".into(),
        path(&keys),
    ]);
    assert_eq!(verified["passed"], true);
    for output in verified["outputs"].as_array().unwrap() {
        checked(&output["verification"]);
        assert_eq!(output["signaling"]["verified"], true);
    }
    let direct = cli(&[
        "convert".into(),
        path(reference),
        "--outputs".into(),
        path(&recipe),
        "--keys-file".into(),
        path(&keys),
        "--output".into(),
        path(&root.join("direct")),
    ]);
    for output in direct["outputs"].as_array().unwrap() {
        checked(&output["conversion"]["verification"]);
        assert_eq!(output["signaling"]["verified"], true);
    }
    output.join("cbc")
}
fn remote_files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = snapshot(root);
    let master = files.get(Path::new("master.m3u8")).unwrap();
    let mut master = String::from_utf8(master.clone()).unwrap();
    let media = files
        .values()
        .find_map(|bytes| {
            std::str::from_utf8(bytes)
                .ok()
                .filter(|text| text.contains("#EXTINF:"))
        })
        .unwrap();
    let mut sessions = String::new();
    for value in media
        .lines()
        .filter_map(|line| line.strip_prefix("#EXT-X-KEY:"))
    {
        writeln!(sessions, "#EXT-X-SESSION-KEY:{value}").unwrap();
    }
    master = master
        .lines()
        .filter(|line| !line.starts_with("#EXT-X-VERSION:"))
        .collect::<Vec<_>>()
        .join("\n")
        .replacen("#EXTM3U", "#EXTM3U\n#EXT-X-VERSION:6", 1);
    master.push('\n');
    master.push_str(&sessions);
    files.insert("master.m3u8".into(), master.into_bytes());
    files
        .into_iter()
        .map(|(name, bytes)| {
            let bytes = if name
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("m3u8"))
            {
                std::str::from_utf8(&bytes)
                    .unwrap()
                    .lines()
                    .map(|line| {
                        if !line.is_empty() && !line.starts_with('#') {
                            format!("{line}?token={}", download_http::QUERY)
                        } else if line.starts_with("#EXT-X-MAP:")
                            || line.starts_with("#EXT-X-MEDIA:")
                        {
                            let attrs =
                                drmr_core::m3u8::parse_attributes(line.split_once(':').unwrap().1)
                                    .unwrap();
                            let uri = attrs.get("URI").unwrap();
                            line.replace(
                                &format!("URI=\"{uri}\""),
                                &format!("URI=\"{uri}?token={}\"", download_http::QUERY),
                            )
                        } else {
                            line.into()
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    .into_bytes()
                    .into_iter()
                    .chain(*b"\n")
                    .collect()
            } else {
                bytes
            };
            (
                format!("/{}", name.to_str().unwrap().replace('\\', "/")),
                bytes,
            )
        })
        .collect()
}
fn acquire_encrypted_source(root: &Path, output: &Path) -> (PathBuf, Vec<String>) {
    let responses = remote_files(output);
    let mut private: Vec<String> = responses
        .values()
        .filter_map(|bytes| std::str::from_utf8(bytes).ok())
        .flat_map(|text| {
            text.lines()
                .filter(|line| line.starts_with("#EXT-X-KEY:"))
                .filter_map(|line| {
                    drmr_core::m3u8::parse_attributes(line.split_once(':')?.1)
                        .ok()?
                        .get("URI")
                        .map(str::to_owned)
                })
        })
        .collect();
    let server = download_http::Server::start(responses.clone());
    private.push(server.origin.clone());
    let policy = root.join("fetch.json");
    write(
        &policy,
        &json!({"allowed_origins":[server.origin],"allow_http":true,"allow_private_network":true,"headers_by_origin":{&server.origin:{"Authorization":download_http::AUTH}}}),
    );
    let input_keys = root.join("input-keys.json");
    write(
        &input_keys,
        &json!({"schema_version":1,"input":{"keys":[{"kid":KID,"key":KEY}]}}),
    );
    let download = root.join("download");
    let url = format!(
        "{}/master.m3u8?token={}",
        server.origin,
        download_http::QUERY
    );
    let analyzed = success(
        &invoke(&[
            "analyze".into(),
            url,
            "--download-dir".into(),
            path(&download),
            "--fetch-policy".into(),
            path(&policy),
            "--keys-file".into(),
            path(&input_keys),
        ]),
        &private,
    );
    assert_eq!(analyzed["input_admitted"], true);
    assert_eq!(analyzed["verification"]["required_depth"], "native");
    let counts = server.stop();
    assert_eq!(
        counts.refused, 0,
        "DRM and key addresses are declarations, never network fetches"
    );
    assert_eq!(counts.served, responses.len());
    (download, private)
}
#[test]
fn hls_multi_drm_url_analysis_saved_conversion_and_fresh_verification_stay_offline_after_acquisition()
 {
    let root = drmr_core::native::private_tempdir().unwrap();
    let reference = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let output = bundle(root.path(), &reference);
    let (download, private) = acquire_encrypted_source(root.path(), &output);
    let input = download.join("input.m3u8");
    let before = snapshot(&download);
    let keys = root.path().join("single-keys.json");
    write(
        &keys,
        &json!({"schema_version":1,"input":{"keys":[{"kid":KID,"key":KEY}]},"output":settings(false)}),
    );
    let saved = root.path().join("single-plan.json");
    let result = root.path().join("result");
    success(
        &invoke(&[
            "plan".into(),
            path(&input),
            "--format".into(),
            "hls".into(),
            "--to".into(),
            "cenc".into(),
            "--processing".into(),
            "segments".into(),
            "--keys-file".into(),
            path(&keys),
            "--output".into(),
            path(&result),
            "--save".into(),
            path(&saved),
        ]),
        &private,
    );
    let converted = success(
        &invoke(&[
            "convert".into(),
            "--plan".into(),
            path(&saved),
            "--keys-file".into(),
            path(&keys),
        ]),
        &private,
    );
    checked(&converted["verification"]);
    let verified = success(
        &invoke(&[
            "verify".into(),
            "--plan".into(),
            path(&saved),
            "--keys-file".into(),
            path(&keys),
        ]),
        &private,
    );
    checked(&verified["verification"]);
    let direct = success(
        &invoke(&[
            "verify".into(),
            path(&result.join("master.m3u8")),
            "--source".into(),
            path(&input),
            "--keys-file".into(),
            path(&keys),
        ]),
        &private,
    );
    checked(&direct);
    let reference_keys = root.path().join("reference-keys.json");
    write(
        &reference_keys,
        &json!({"schema_version":1,"output":settings(false)}),
    );
    checked(&success(
        &invoke(&[
            "verify".into(),
            path(&result.join("master.m3u8")),
            "--source".into(),
            path(&reference),
            "--keys-file".into(),
            path(&reference_keys),
        ]),
        &private,
    ));
    assert_eq!(snapshot(&download), before);
}
