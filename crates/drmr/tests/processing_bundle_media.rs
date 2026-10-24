//! Direct conversion applies a default before freezing each output's own processing choice.
#[path = "support/download_http.rs"]
mod download_http;
use serde_json::{Value, json};
use std::{collections::BTreeMap, io::Write as _, path::Path, process::Command};

fn private_json(path: &Path, value: &Value) {
    drmr_core::native::create_private_new(path)
        .unwrap()
        .write_all(value.to_string().as_bytes())
        .unwrap();
}

fn fixture(root: &Path) {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let result = Command::new("packager")
        .current_dir(root)
        .arg(format!(
            "in={},stream=video,segment_template=segment-$Number$.ts,playlist_name=media.m3u8",
            clip.display()
        ))
        .args([
            "--segment_duration",
            "2",
            "--transport_stream_timestamp_offset_ms",
            "0",
            "--hls_master_playlist_output",
            "master.m3u8",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn files(root: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(root)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                format!("/{}", path.file_name().unwrap().to_str().unwrap()),
                std::fs::read(path).unwrap(),
            )
        })
        .collect()
}

fn remote_files(original: &BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    original
        .iter()
        .map(|(name, bytes)| {
            let bytes = if Path::new(name)
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("m3u8"))
            {
                String::from_utf8(bytes.clone())
                    .unwrap()
                    .replace(".m3u8", &format!(".m3u8?token={}", download_http::QUERY))
                    .replace(".ts", &format!(".ts?token={}", download_http::QUERY))
                    .into_bytes()
            } else {
                bytes.clone()
            };
            (name.clone(), bytes)
        })
        .collect()
}

fn direct(root: &Path, input: &str, policy: Option<&Path>, default: &str) {
    drmr_core::native::create_private_dir_all(root).unwrap();
    let explicit = if default == "segments" {
        "stitch"
    } else {
        "segments"
    };
    let recipe = root.join("outputs.json");
    private_json(
        &recipe,
        &json!({"schema_version":2,"outputs":[
            {"id":"default","format":"hls-ts","target":"clear","drms":[]},
            {"id":"explicit","format":"hls-ts","target":"clear","drms":[],"processing":explicit}
        ]}),
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command
        .args(["convert", input, "--outputs"])
        .arg(recipe)
        .arg("--output")
        .arg(root.join("result"))
        .args(["--processing", default, "--json"]);
    if let Some(policy) = policy {
        command
            .arg("--fetch-policy")
            .arg(policy)
            .arg("--download-dir")
            .arg(root.join("retained"));
    }
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    for output in [&result.stdout, &result.stderr] {
        let text = String::from_utf8_lossy(output);
        assert!(!text.contains(download_http::AUTH) && !text.contains(download_http::QUERY));
    }
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["outputs"].as_array().unwrap().len(), 2);
    for (id, mode) in [("default", default), ("explicit", explicit)] {
        let result = report["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|output| output["id"] == id)
            .unwrap();
        assert_eq!(result["conversion"]["verification"]["passed"], true);
        assert_eq!(
            result["conversion"]["verification"]["required_depth"],
            "native"
        );
        assert_eq!(result["conversion"]["processing"]["mode"], mode);
        assert_eq!(
            result["conversion"]["processing"]["boundaries"],
            if mode == "segments" {
                "preserve"
            } else {
                "resegment"
            }
        );
    }
    if policy.is_some() {
        assert!(root.join("retained/input.m3u8").is_file());
    }
}

#[test]
fn local_direct_bundle_applies_defaults_before_executing_explicit_modes() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    fixture(&source);
    let before = files(&source);
    let input = source.join("master.m3u8");
    for mode in ["stitch", "segments"] {
        direct(&root.path().join(mode), input.to_str().unwrap(), None, mode);
    }
    assert_eq!(files(&source), before);
}

#[test]
fn url_direct_bundle_applies_defaults_before_executing_explicit_modes() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    fixture(&source);
    let before = files(&source);
    let server = download_http::Server::start(remote_files(&before));
    let policy = root.path().join("fetch.json");
    private_json(
        &policy,
        &json!({"allowed_origins":[server.origin],"allow_http":true,"allow_private_network":true,
        "headers_by_origin":{server.origin.clone():{"Authorization":download_http::AUTH}}}),
    );
    let input = format!(
        "{}/master.m3u8?token={}",
        server.origin,
        download_http::QUERY
    );
    for mode in ["stitch", "segments"] {
        direct(&root.path().join(mode), &input, Some(&policy), mode);
    }
    assert_eq!(files(&source), before);
    let counts = server.stop();
    assert!(counts.served > 0);
    assert_eq!(counts.refused, 0);
}
