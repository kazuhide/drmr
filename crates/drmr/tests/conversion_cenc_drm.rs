//! Saved native checks cover Clear TS -> CENC DRM -> CBCS DRM -> clear fMP4 HLS.
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};

const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";
const LICENSE: &str = "https://license.example.invalid/cenc-roundtrip";
// A complete version-one Widevine box with the one output KID and no opaque payload.
const WV: &str = "000000347073736801000000edef8ba979d64acea3c827dcd51d21ed00000001000102030405060708090a0b0c0d0e0f00000000";

fn cli(args: &[String], native_segments: bool) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_drmr"));
    command.args(args).args(["--json", "--progress", "off"]);
    if native_segments {
        command.args(["--packager-bin", "must-not-run-packager"]);
    }
    let result = command.output().unwrap();
    for bytes in [&result.stdout, &result.stderr] {
        let text = String::from_utf8_lossy(bytes);
        for secret in [KEY, LICENSE, WV] {
            assert!(!text.contains(secret));
            assert!(!text.contains(&secret.to_ascii_uppercase()));
        }
    }
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}

fn path(value: &Path) -> String {
    drmr_core::manifest_support::path::fspath(value).unwrap()
}

fn checked(report: &Value) {
    assert_eq!(report["passed"], true, "{report}");
    assert_eq!(report["required_depth"], "native");
    assert_eq!(
        report["decode_evidence"],
        json!({"status":"not-checked","reason":"not-requested"})
    );
    assert_eq!(report["license_playback_verified"], false);
    assert_eq!(report["checks"]["packet_payloads_equal"], true);
    assert_eq!(report["checks"]["packet_timing_equal"], true);
}

fn stage(
    root: &Path,
    input: &Path,
    name: &str,
    target: &str,
    format: &str,
    processing: &str,
    config: &Value,
) -> PathBuf {
    let key_file = root.join(format!("{name}.keys.json"));
    serde_json::to_writer(
        drmr_core::native::create_private_new(&key_file).unwrap(),
        config,
    )
    .unwrap();
    let saved = root.join(format!("{name}.plan.json"));
    let destination = root.join(name);
    let native_segments = processing == "segments";
    let planned = cli(
        &[
            "plan".into(),
            path(input),
            "--to".into(),
            target.into(),
            "--format".into(),
            format.into(),
            "--processing".into(),
            processing.into(),
            "--output".into(),
            path(&destination),
            "--save".into(),
            path(&saved),
            "--keys-file".into(),
            path(&key_file),
        ],
        native_segments,
    );
    let saved_bytes = std::fs::read(&saved).unwrap();
    assert_eq!(
        planned,
        serde_json::from_slice::<Value>(&saved_bytes).unwrap()
    );
    let converted = cli(
        &[
            "convert".into(),
            "--plan".into(),
            path(&saved),
            "--keys-file".into(),
            path(&key_file),
        ],
        native_segments,
    );
    checked(&converted["verification"]);
    let verified = cli(
        &[
            "verify".into(),
            "--plan".into(),
            path(&saved),
            "--keys-file".into(),
            path(&key_file),
        ],
        native_segments,
    );
    assert_eq!(verified["source_unchanged"], true);
    assert_eq!(verified["output_matches_plan"], true);
    checked(&verified["verification"]);
    assert_eq!(std::fs::read(saved).unwrap(), saved_bytes);
    let entrypoint = destination.join(if format == "dash" {
        "manifest.mpd"
    } else {
        "master.m3u8"
    });
    let metadata = cli(
        &[
            "analyze".into(),
            path(&entrypoint),
            "--metadata-only".into(),
        ],
        false,
    );
    assert_eq!(metadata["format"], format);
    assert_eq!(metadata["protection"], target);
    assert_eq!(
        metadata["drm_systems"],
        if target == "clear" {
            json!([])
        } else {
            json!(["playready", "widevine"])
        }
    );
    entrypoint
}

#[test]
fn saved_cenc_cbcs_and_clear_outputs_verify_against_the_previous_stage_and_original() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = root.path().join("source");
    drmr_core::native::create_media_dir_all(&source).unwrap();
    let input = source.join("media.m3u8");
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    let generated = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(clip)
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
            "-hls_segment_filename",
        ])
        .arg(source.join("segment-%d.ts"))
        .arg(&input)
        .output()
        .unwrap();
    assert!(generated.status.success(), "fixture preparation failed");
    let before: Vec<_> = std::fs::read_dir(&source)
        .unwrap()
        .map(|entry| {
            let file = entry.unwrap().path();
            let bytes = std::fs::read(&file).unwrap();
            (file, bytes)
        })
        .collect();
    let output = json!({"kid":KID,"key":KEY,"signaling":{"system":"multi-drm","widevine_pssh":WV,"playready_license_url":LICENSE}});
    let first = stage(
        root.path(),
        &input,
        "01-cenc",
        "cenc",
        "dash",
        "stitch",
        &json!({"schema_version":1,"output":output}),
    );
    let input_keys = json!({"keys":[{"kid":KID,"key":KEY}]});
    let second = stage(
        root.path(),
        &first,
        "02-cbcs",
        "cbcs",
        "dash",
        "segments",
        &json!({"schema_version":1,"input":input_keys,"output":output}),
    );
    let third = stage(
        root.path(),
        &second,
        "03-clear",
        "clear",
        "hls",
        "segments",
        &json!({"schema_version":1,"input":input_keys}),
    );
    let final_comparison = cli(
        &[
            "verify".into(),
            path(&third),
            "--source".into(),
            path(&input),
        ],
        true,
    );
    checked(&final_comparison);
    for (file, bytes) in before {
        assert_eq!(std::fs::read(file).unwrap(), bytes);
    }
}
