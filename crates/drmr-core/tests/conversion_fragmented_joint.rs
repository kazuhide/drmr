//! Finite fragmented video/AAC reimport remains narrower than arbitrary multi-track MP4.
mod common;

use drmr_core::{
    conversion::{
        ContentKey, ConversionRequest, Converter, HlsInputKeys, InputKeys, OutputKey,
        SourceValidationPurpose, VerificationSource,
    },
    events::Collector,
    inspection::{OutputFormat, TargetProtection},
    mp4,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

const VIDEO_KID: &str = "000102030405060708090a0b0c0d0e0f";
const AUDIO_KID: &str = "303132333435363738393a3b3c3d3e3f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";

fn command() -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(runner, "packager", "absent-ffmpeg").with_ffprobe("absent-ffprobe"),
        events,
    )
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}

fn joint(path: &Path, third_track: bool) {
    let mut ffmpeg = std::process::Command::new("ffmpeg");
    ffmpeg
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture("hevc-main.mp4"))
        .arg("-i")
        .arg(fixture("clip.mp4"))
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c",
            "copy",
            "-map_metadata",
            "-1",
        ]);
    if third_track {
        ffmpeg.args([
            "-map",
            "1:a:0",
            "-movflags",
            "empty_moov+frag_keyframe+default_base_moof",
        ]);
    }
    assert!(ffmpeg.arg(path).status().unwrap().success());
}

fn convert(
    command: &Converter,
    source: &Path,
    output: &Path,
    keys: &InputKeys,
    target: TargetProtection,
) -> Result<drmr_core::conversion::ConversionReport, drmr_core::conversion::ConversionError> {
    let key = OutputKey::from_hex(VIDEO_KID, KEY).unwrap();
    command.convert(ConversionRequest {
        input: source,
        output,
        format: OutputFormat::Mp4,
        target,
        input_keys: keys,
        hls_input_keys: &HlsInputKeys::default(),
        output_key: (target != TargetProtection::Clear).then_some(&key),
        hls_key_uri: None,
        hls_output_key: None,
    })
}

fn role_keys() -> InputKeys {
    InputKeys::new(vec![
        ContentKey::from_hex(VIDEO_KID, KEY).unwrap(),
        ContentKey::from_hex(AUDIO_KID, KEY).unwrap(),
    ])
    .unwrap()
}

fn audio_nodes(bytes: &[u8]) -> Vec<mp4::Mp4Box> {
    let nodes = mp4::parse_boxes(bytes).unwrap();
    let track = nodes
        .iter()
        .find(|node| node.kind() == *b"moov")
        .unwrap()
        .children
        .iter()
        .filter(|node| node.kind() == *b"trak")
        .find(|track| {
            mp4::walk(std::slice::from_ref(track)).iter().any(|node| {
                node.kind() == *b"hdlr" && node.header.payload(bytes).get(8..12) == Some(b"soun")
            })
        })
        .unwrap();
    mp4::walk(std::slice::from_ref(track))
        .into_iter()
        .cloned()
        .collect()
}

fn distinct_audio_id(bytes: &mut [u8]) {
    let nodes = audio_nodes(bytes);
    let encryption = nodes.iter().find(|node| node.kind() == *b"tenc").unwrap();
    let at = encryption.header.offset + encryption.header.header_size;
    assert_eq!(bytes[at], 0);
    bytes[at + 8..at + 24].copy_from_slice(&hex::decode(AUDIO_KID).unwrap());
    // Generic source PSSH is optional; remove it while retaining all sample addresses.
    let tree = mp4::parse_boxes(bytes).unwrap();
    for node in mp4::walk(&tree)
        .into_iter()
        .filter(|node| node.kind() == *b"pssh")
    {
        let at = node.header.offset;
        bytes[at + 4..at + 8].copy_from_slice(b"free");
        bytes[at + node.header.header_size..at + node.header.size].fill(0);
    }
}

fn assert_no_commands(events: &Collector) {
    assert!(
        !events
            .records()
            .iter()
            .any(
                |record| record.get("event").and_then(serde_json::Value::as_str) == Some("command")
            )
    );
}

fn refuse_without_tools(source: &Path, output: &Path, keys: &InputKeys, invalid_source: bool) {
    let (denied, events) = command();
    if invalid_source {
        assert!(
            denied
                .validate_source(
                    VerificationSource {
                        input: source,
                        input_keys: keys,
                        hls_keys: &HlsInputKeys::default(),
                    },
                    SourceValidationPurpose::Conversion
                )
                .is_err()
        );
    }
    assert!(convert(&denied, source, output, keys, TargetProtection::Clear).is_err());
    assert!(!output.exists());
    assert_no_commands(&events);
}

#[test]
fn fragmented_joint_reimport_requires_each_role_key_finite_windows_and_exact_track_shape() {
    let owner = common::private_tempdir().unwrap();
    let progressive = owner.path().join("progressive.mp4");
    joint(&progressive, false);
    let protected = owner.path().join("protected.mp4");
    convert(
        &command().0,
        &progressive,
        &protected,
        &InputKeys::default(),
        TargetProtection::Cenc,
    )
    .unwrap();
    let mut bytes = fs::read(&protected).unwrap();
    distinct_audio_id(&mut bytes);
    let source = owner.path().join("two-keys.mp4");
    fs::write(&source, &bytes).unwrap();
    let keys = role_keys();
    let (valid, _) = command();
    let qualification = valid
        .validate_source(
            VerificationSource {
                input: &source,
                input_keys: &keys,
                hls_keys: &HlsInputKeys::default(),
            },
            SourceValidationPurpose::Conversion,
        )
        .unwrap();
    assert!(qualification.eligible_formats.contains(&OutputFormat::Mp4));
    let clear = owner.path().join("clear.mp4");
    assert!(
        convert(&valid, &source, &clear, &keys, TargetProtection::Clear)
            .unwrap()
            .verification
            .passed
    );

    let (denied, events) = command();
    let output = owner.path().join("unused-output-key.mp4");
    let unused_key = OutputKey::from_hex(VIDEO_KID, KEY).unwrap();
    assert_eq!(
        denied
            .convert_mp4(
                &source,
                &output,
                TargetProtection::Clear,
                &keys,
                Some(&unused_key)
            )
            .unwrap_err()
            .0,
        "output-key-not-used"
    );
    assert!(!output.exists());
    assert_no_commands(&events);

    let missing = InputKeys::new(vec![ContentKey::from_hex(VIDEO_KID, KEY).unwrap()]).unwrap();
    refuse_without_tools(
        &source,
        &owner.path().join("missing-key.mp4"),
        &missing,
        true,
    );

    let mut changed = bytes.clone();
    let nodes = audio_nodes(&changed);
    let edit = nodes.iter().find(|node| node.kind() == *b"elst").unwrap();
    let at = edit.header.offset + edit.header.header_size;
    assert_eq!(changed[at], 1);
    changed[at + 8..at + 16].copy_from_slice(&1_000_000_000_u64.to_be_bytes());
    let altered = owner.path().join("outside-window.mp4");
    fs::write(&altered, &changed).unwrap();
    refuse_without_tools(
        &altered,
        &owner.path().join("outside-window-output.mp4"),
        &keys,
        true,
    );

    let third = owner.path().join("three-tracks.mp4");
    joint(&third, true);
    refuse_without_tools(
        &third,
        &owner.path().join("three-track-output.mp4"),
        &InputKeys::default(),
        false,
    );
    assert_eq!(fs::read(source).unwrap(), bytes);
    assert_eq!(fs::read(altered).unwrap(), changed);
}
