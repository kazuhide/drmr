//! Local metadata inspection, conversion intent, compatibility, and secret-safe output.

use std::path::Path;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_drmr"))
        .args(args)
        .output()
        .unwrap()
}

fn fixture(dir: &Path, name: &str, contents: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, contents).unwrap();
    path.to_string_lossy().into_owned()
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(&output.stderr)))
}

#[test]
fn metadata_analysis_is_read_only_and_never_fetches_or_displays_hls_key_material() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture(
        dir.path(),
        "input.m3u8",
        "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-KEY:METHOD=AES-128,URI=\"https://example.test/key?credential=do-not-print\",IV=0x0123456789abcdef0123456789abcdef\n#EXTINF:2,\nsegment.ts\n#EXT-X-ENDLIST\n",
    );
    let output = run(&["analyze", "--metadata-only", &input, "--json"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json(&output);
    assert_eq!(report["format"], "hls");
    assert_eq!(report["protection"], "hls-aes128");
    assert_eq!(report["media_verified"], false);
    let shown = String::from_utf8_lossy(&output.stdout);
    assert!(
        !shown.contains("do-not-print")
            && !shown.contains("0123456789abcdef")
            && !shown.contains("https://")
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn human_metadata_analysis_names_the_sample_method_without_claiming_a_container_scheme() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture(
        dir.path(),
        "input.m3u8",
        "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://private-sentinel\",KEYFORMAT=\"com.apple.streamingkeydelivery\",IV=0x0123456789abcdef0123456789abcdef\n#EXTINF:2,\nnot-read.ts\n#EXT-X-ENDLIST\n",
    );
    let output = run(&["analyze", "--metadata-only", &input]);
    assert!(output.status.success());
    let shown = String::from_utf8(output.stdout).unwrap();
    assert!(shown.contains("HLS method: SAMPLE-AES"));
    assert!(shown.contains("Protection: unknown"));
    assert!(shown.contains("AES-128 in cipher block chaining mode"));
    assert!(!shown.contains("private-sentinel") && !shown.contains("0123456789abcdef"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn plan_separates_input_and_output_key_roles_and_explains_incompatible_formats() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture(
        dir.path(),
        "input.mpd",
        r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011"><Period><AdaptationSet contentType="video"><ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc"/><Representation id="v0" codecs="avc1.64001f"/></AdaptationSet></Period></MPD>"#,
    );
    let output = run(&[
        "plan", &input, "--to", "cbcs", "--format", "hls,dash", "--json",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json(&output);
    assert_eq!(report["target"], "cbcs");
    assert_eq!(report["keys"][0]["role"], "input");
    assert_eq!(report["keys"][1]["role"], "output");
    assert_eq!(report["execution_available"], false);
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);

    let incompatible = run(&[
        "plan",
        &input,
        "--to",
        "hls-aes128",
        "--format",
        "mp4",
        "--json",
    ]);
    assert_eq!(incompatible.status.code(), Some(1));
    assert_eq!(json(&incompatible)["compatible"], false);
}

#[test]
fn errors_do_not_echo_secret_source_urls_and_new_plan_requires_a_target() {
    let output = run(&[
        "analyze",
        "--metadata-only",
        "ftp://example.test/source.mpd?credential=do-not-print",
        "--json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "analyze: download-url-scheme-not-supported\n"
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("do-not-print"));
    let output = run(&["plan", "source.mpd"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--to"));
}

#[test]
fn human_plan_explains_unknown_sample_coverage_and_conditional_tools_without_running_them() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture(
        dir.path(),
        "input.mpd",
        r#"<MPD><Period><AdaptationSet contentType="video"><ContentProtection schemeIdUri="urn:mpeg:dash:mp4protection:2011" value="cenc"/><Representation codecs="avc1.64001f"/></AdaptationSet></Period></MPD>"#,
    );
    let output = run(&["plan", &input, "--to", "cbcs", "--format", "hls"]);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("AES-128 in counter mode"));
    assert!(text.contains("Full or partial sample encryption: unknown"));
    assert!(text.contains("Conditional route: fmp4-hls-presentation"));
    assert!(text.contains("Native minimum tools: shaka-packager"));
    assert!(text.contains("Additional complete decoding requires FFmpeg and ffprobe"));
    assert!(text.contains("encrypted-finite-progressive-source-copy-requires-ffmpeg"));
    assert!(text.contains("Source revalidation is required"));
    assert!(text.contains("manifest declaration"));
    assert!(
        !text.contains("Some(")
            && !text.contains("ManifestDeclaration")
            && !text.contains("NotApplicable")
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn known_text_declarations_have_no_candidate_in_human_or_json_plan() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture(
        dir.path(),
        "text.mpd",
        "<MPD><Period><AdaptationSet contentType=\"text\"><Representation codecs=\"wvtt\"/></AdaptationSet></Period></MPD>",
    );
    let output = run(&[
        "plan", &input, "--to", "clear", "--format", "dash", "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let report = json(&output);
    assert_eq!(report["compatible"], false);
    assert_eq!(report["execution_available"], false);
    assert_eq!(report["processing"]["candidates"], serde_json::json!([]));
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| { finding["code"] == "no-supported-processing-route" })
    );
    let output = run(&["plan", &input, "--to", "clear", "--format", "dash"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Plan status: unsupported"));
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("No candidate route is available from the inspected declarations.")
    );
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn several_supported_output_intents_remain_conditional_separate_commands() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture(
        dir.path(),
        "video.mpd",
        "<MPD><Period><AdaptationSet contentType=\"video\"><Representation codecs=\"avc1.64001f\"/></AdaptationSet></Period></MPD>",
    );
    let output = run(&[
        "plan", &input, "--to", "clear", "--format", "dash,hls", "--json",
    ]);
    assert!(output.status.success());
    let report = json(&output);
    assert_eq!(report["compatible"], true);
    assert_eq!(report["execution_available"], false);
    assert_eq!(
        report["processing"]["candidates"].as_array().unwrap().len(),
        2
    );
    let output = run(&["plan", &input, "--to", "clear", "--format", "dash,hls"]);
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("Plan status: conditional"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn each_requested_output_format_needs_its_own_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture(
        dir.path(),
        "two-videos.mpd",
        "<MPD><Period><AdaptationSet contentType=\"video\" codecs=\"avc1.64001f\"><Representation id=\"v1\"/><Representation id=\"v2\"/></AdaptationSet></Period></MPD>",
    );
    let output = run(&[
        "plan", &input, "--to", "clear", "--format", "mp4,dash", "--json",
    ]);
    assert_eq!(output.status.code(), Some(1));
    let report = json(&output);
    assert_eq!(report["compatible"], false);
    assert_eq!(report["execution_available"], false);
    assert_eq!(
        report["processing"]["candidates"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        report["processing"]["candidates"][0]["output_format"],
        "dash"
    );
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| { finding["code"] == "output-format-has-no-processing-route" })
    );
}

#[test]
fn ts_output_plan_is_explicit_conditional_and_keeps_key_references_private() {
    let dir = tempfile::tempdir().unwrap();
    let input = fixture(
        dir.path(),
        "input.m3u8",
        "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://private-sentinel\",IV=0x0123456789abcdef0123456789abcdef\n#EXTINF:2,\nnot-read.ts\n#EXT-X-ENDLIST\n",
    );
    let output = run(&[
        "plan",
        &input,
        "--to",
        "hls-sample-aes",
        "--format",
        "hls-ts",
        "--json",
    ]);
    assert!(output.status.success());
    let report = json(&output);
    assert_eq!(report["target"], "hls-sample-aes");
    assert_eq!(report["formats"], serde_json::json!(["hls-ts"]));
    assert_eq!(report["input"]["protection"], "unknown");
    assert_eq!(
        report["processing"]["candidates"][0]["route"],
        "mpeg-ts-hls-ts"
    );
    assert_eq!(
        report["processing"]["candidates"][0]["revalidation_required"],
        true
    );
    assert_eq!(report["keys"][0]["role"], "input");
    assert_eq!(report["keys"][1]["role"], "output");
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("private-sentinel") && !text.contains("0123456789abcdef"));
    let human = run(&[
        "plan",
        &input,
        "--to",
        "hls-sample-aes",
        "--format",
        "hls-ts",
    ]);
    assert!(human.status.success());
    assert!(
        String::from_utf8(human.stdout)
            .unwrap()
            .contains("Conditional route: mpeg-ts-hls-ts")
    );
    for target in ["cbcs", "cenc"] {
        let output = run(&[
            "plan", &input, "--to", target, "--format", "hls-ts", "--json",
        ]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(json(&output)["compatible"], false);
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn ts_conversion_passes_the_hls_key_uri_in_its_own_key_role() {
    use std::io::Write as _;
    let dir = drmr_core::native::private_tempdir().unwrap();
    let config_path = dir.path().join("keys.json");
    let output_path = dir.path().join("result");
    for target in ["hls-aes128", "hls-sample-aes"] {
        let mut config = serde_json::json!({"schema_version":1,"output":{
            "key":"22222222222222222222222222222222", "hls_key_uri":"skd://private-sentinel"
        }});
        if target == "hls-sample-aes" {
            config["output"]["iv"] = "33333333333333333333333333333333".into();
        }
        drmr_core::native::create_private_or_truncate(&config_path)
            .unwrap()
            .write_all(serde_json::to_string(&config).unwrap().as_bytes())
            .unwrap();
        let output = run(&[
            "convert",
            "missing-source.m3u8",
            "--to",
            target,
            "--format",
            "hls-ts",
            "--output",
            output_path.to_str().unwrap(),
            "--keys-file",
            config_path.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(output.status.code(), Some(1));
        assert_eq!(
            String::from_utf8_lossy(&output.stderr),
            "convert: input-inspection-failed\n"
        );
        assert_eq!(
            json(&output),
            serde_json::json!({
                "schema_version": 1,
                "command": "convert",
                "status": "failed",
                "error": {
                    "code": "input-inspection-failed",
                    "hint": "Review the error code and source profile before retrying with a new output destination.",
                    "last_activity": "inspection"
                }
            })
        );
        for stream in [&output.stdout, &output.stderr] {
            let shown = String::from_utf8_lossy(stream);
            for secret in [
                "private-sentinel",
                "22222222222222222222222222222222",
                "33333333333333333333333333333333",
            ] {
                assert!(!shown.contains(secret));
            }
        }
        assert!(!output_path.exists());
        assert_eq!(
            std::fs::read(&config_path).unwrap(),
            serde_json::to_vec(&config).unwrap()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
