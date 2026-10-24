//! Full CLI conversion and verification of a generated, long presentation without large fixtures.
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output};

fn success(command: &mut Command) -> Output {
    let output = command
        .output()
        .expect("the required media tool must be installed");
    assert!(output.status.success(), "required media operation failed");
    output
}

fn proof(value: &serde_json::Value) {
    assert_eq!(value["passed"], true);
    assert_eq!(
        value["decode_evidence"],
        serde_json::json!({"status":"content-compared","equal":true})
    );
    assert_eq!(value["license_playback_verified"], false);
    assert_eq!(value["checks"]["stream_metadata"], "pass");
    for name in ["packet_payloads_equal", "packet_timing_equal"] {
        assert_eq!(value["checks"][name], true);
    }
}

#[test]
fn a_long_bbb_presentation_converts_and_independently_verifies_every_packet() {
    let root = tempfile::tempdir().unwrap();
    let private = root.path().join("private");
    drmr_core::native::create_private_dir_all(&private).unwrap();
    let small = private.join("small.mp4");
    let source = private.join("long.mp4");
    let target = private.join("protected.mp4");
    let keys = private.join("keys.json");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/bbb/clip.mp4");
    // Repeat whole attributed clips, retaining the complete 4,170-second presentation.
    success(
        Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(fixture)
            .args([
                "-map",
                "0:v:0",
                "-an",
                "-vf",
                "scale=32:18",
                "-c:v",
                "libx264",
                "-threads",
                "1",
                "-preset",
                "ultrafast",
                "-g",
                "48",
                "-keyint_min",
                "48",
                "-sc_threshold",
                "0",
            ])
            .arg(&small),
    );
    success(
        Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-stream_loop", "416", "-i"])
            .arg(&small)
            .args(["-map", "0:v:0", "-c", "copy"])
            .arg(&source),
    );
    let counted = success(
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_packets",
                "-select_streams",
                "0",
                "-show_entries",
                "stream=nb_read_packets",
                "-of",
                "json",
            ])
            .arg(&source),
    );
    let counted: serde_json::Value = serde_json::from_slice(&counted.stdout).unwrap();
    assert_eq!(counted["streams"][0]["nb_read_packets"], "100080");
    drmr_core::native::create_private_new(&keys).unwrap().write_all(
        br#"{"schema_version":1,"output":{"kid":"00000000000000000000000000000002","key":"22222222222222222222222222222222"}}"#,
    ).unwrap();
    let converted = success(
        Command::new(env!("CARGO_BIN_EXE_drmr"))
            .arg("convert")
            .arg("--decode")
            .arg(&source)
            .args(["--to", "cbcs", "--format", "mp4", "--output"])
            .arg(&target)
            .arg("--keys-file")
            .arg(&keys)
            .arg("--json"),
    );
    let converted: serde_json::Value = serde_json::from_slice(&converted.stdout).unwrap();
    proof(&converted["verification"]);
    let verified = success(
        Command::new(env!("CARGO_BIN_EXE_drmr"))
            .arg("verify")
            .arg("--decode")
            .arg(&target)
            .arg("--source")
            .arg(&source)
            .arg("--keys-file")
            .arg(&keys)
            .arg("--json"),
    );
    proof(&serde_json::from_slice::<serde_json::Value>(&verified.stdout).unwrap());
}
