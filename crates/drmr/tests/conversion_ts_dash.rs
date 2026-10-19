//! Original HLS transport streams can become clear or sample-encrypted DASH.
#[path = "support/presentation_cli.rs"]
mod presentation_cli;
use presentation_cli::{configuration, convert_and_verify, fixture};
use serde_json::json;
use std::process::Command;

#[test]
fn real_joint_transport_stream_input_converts_to_each_dash_protection() {
    let owner = tempfile::tempdir().unwrap();
    let root = owner.path().join("private");
    drmr_core::native::create_private_dir_all(&root).unwrap();
    let source = root.join("media.m3u8");
    let generated = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture("clip.mp4"))
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-start_number",
            "7",
            "-hls_segment_filename",
        ])
        .arg(root.join("segment-%02d.ts"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        generated.status.success(),
        "public TS fixture generation failed"
    );
    let original = std::fs::read(&source).unwrap();
    let mut results = Vec::new();
    for target in ["clear", "cenc", "cbcs"] {
        let config = root.join(format!("{target}.json"));
        configuration(&config, false, target, "dash");
        let destination = root.join(format!("dash-{target}"));
        let report = convert_and_verify(&source, &destination, &config, "dash", target);
        assert_eq!(report["output"]["tracks"].as_array().unwrap().len(), 2);
        results.push(json!({"target": target, "verification": report["verification"]}));
    }
    assert_eq!(std::fs::read(&source).unwrap(), original);
    println!(
        "{}",
        json!({"input": "clear-joint-hls-mpeg-ts", "routes": results})
    );
}
