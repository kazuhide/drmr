//! Native progressive copies retain independent source evidence and reject corrupt metadata.
mod common;

use drmr_core::{
    conversion::{
        ContentKey, ConversionError, ConversionReport, ConversionRequest, Converter, HlsInputKeys,
        InputKeys, VerificationRequest, VerificationSource,
    },
    events::Collector,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{path::Path, sync::Arc};

const VIDEO_ID: &str = "000102030405060708090a0b0c0d0e0f";
const AUDIO_ID: &str = "303132333435363738393a3b3c3d3e3f";
const VIDEO_KEY: &str = "101112131415161718191a1b1c1d1e1f";
const AUDIO_KEY: &str = "202122232425262728292a2b2c2d2e2f";

fn fixture(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}

fn keys(entries: &[(&str, &str)]) -> InputKeys {
    InputKeys::new(
        entries
            .iter()
            .map(|(id, key)| ContentKey::from_hex(id, key).unwrap())
            .collect(),
    )
    .unwrap()
}

fn converter(root: &Path) -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let c = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
        root.join("absent-packager").to_string_lossy(),
        root.join("absent-ffmpeg").to_string_lossy(),
    )
    .with_ffprobe(root.join("absent-ffprobe").to_string_lossy());
    (c, events)
}

fn copy(
    c: &Converter,
    input: &Path,
    input_keys: &InputKeys,
    output: &Path,
) -> Result<ConversionReport, ConversionError> {
    c.convert(ConversionRequest {
        input,
        output,
        format: OutputFormat::Mp4,
        target: TargetProtection::Clear,
        input_keys,
        hls_input_keys: &HlsInputKeys::default(),
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    })
}

fn assert_no_tools(events: &Collector) {
    assert!(
        events.records().iter().all(|record| {
            record.get("stage").and_then(serde_json::Value::as_str) != Some("tool")
        })
    );
}

fn assert_differs_from_clear_reference(c: &Converter, output: &Path) {
    let reference = fixture("clip.mp4");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let checked = c.verify(VerificationRequest {
        source: VerificationSource {
            input: &reference,
            input_keys: &empty,
            hls_keys: &hls,
        },
        output: VerificationSource {
            input: output,
            input_keys: &empty,
            hls_keys: &hls,
        },
    });
    assert!(checked.is_err() || checked.is_ok_and(|report| !report.passed));
}

#[test]
fn missing_keys_and_invalid_auxiliary_never_publish_a_native_copy() {
    let root = common::private_tempdir().unwrap();
    let (c, events) = converter(root.path());
    let complete = keys(&[(VIDEO_ID, VIDEO_KEY), (AUDIO_ID, AUDIO_KEY)]);
    for name in ["cenc-iv8.mp4", "cbcs-joint.mp4"] {
        let input = fixture(name);
        let original = std::fs::read(&input).unwrap();
        for (label, input_keys) in [
            ("empty", InputKeys::default()),
            ("video-only", keys(&[(VIDEO_ID, VIDEO_KEY)])),
            ("audio-only", keys(&[(AUDIO_ID, AUDIO_KEY)])),
        ] {
            let output = root.path().join(format!("{name}-{label}.mp4"));
            assert_eq!(
                copy(&c, &input, &input_keys, &output).unwrap_err().0,
                "input-key-required"
            );
            assert!(!output.exists());
        }
        let boxes = drmr_core::mp4::parse_boxes(&original).unwrap();
        let mut checked = 0;
        for (index, node) in drmr_core::mp4::walk(&boxes)
            .into_iter()
            .filter(|node| matches!(&node.kind(), b"senc" | b"saiz" | b"saio"))
            .enumerate()
        {
            let mut broken = original.clone();
            let offset = match &node.kind() {
                b"senc" => 4,
                b"saiz" => 5,
                b"saio" => 8,
                _ => unreachable!(),
            };
            let at = node.header.offset + node.header.header_size + offset;
            broken[at + 3] ^= 1;
            let source = root.path().join(format!("{name}-invalid-{index}.mp4"));
            let output = root.path().join(format!("{name}-result-{index}.mp4"));
            std::fs::write(&source, &broken).unwrap();
            assert!(copy(&c, &source, &complete, &output).is_err());
            assert!(!output.exists());
            assert_eq!(std::fs::read(&source).unwrap(), broken);
            checked += 1;
        }
        assert!(checked >= 3);
        assert_eq!(std::fs::read(&input).unwrap(), original);
    }
    assert_no_tools(&events);
}

#[test]
fn changed_windows_offsets_and_payloads_fail_against_the_clear_original() {
    let root = common::private_tempdir().unwrap();
    let (c, events) = converter(root.path());
    let input = fixture("cbcs-joint.mp4");
    let complete = keys(&[(VIDEO_ID, VIDEO_KEY), (AUDIO_ID, AUDIO_KEY)]);
    let output = root.path().join("clear.mp4");
    let report = copy(&c, &input, &complete, &output).unwrap();
    assert!(report.verification.passed && report.verification.checks.is_pass());
    let original = std::fs::read(&output).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&original).unwrap();
    let mut changed_offsets = Vec::new();
    for node in drmr_core::mp4::walk(&boxes) {
        let at = node.header.offset + node.header.header_size;
        match &node.kind() {
            b"elst" => {
                assert_eq!(original[at], 0);
                changed_offsets.extend([at + 11, at + 15]);
            }
            b"stco" => changed_offsets.push(at + 11),
            _ => {}
        }
    }
    let media = boxes.iter().find(|node| node.kind() == *b"mdat").unwrap();
    changed_offsets.push(media.header.offset + media.header.header_size + 4);
    assert!(changed_offsets.len() >= 5);
    for at in changed_offsets {
        let mut changed = original.clone();
        changed[at] ^= 1;
        std::fs::write(&output, &changed).unwrap();
        assert_differs_from_clear_reference(&c, &output);
    }
    assert_no_tools(&events);
}

#[test]
fn unauthenticated_wrong_keys_do_not_establish_equality_to_a_known_clear_original() {
    let root = common::private_tempdir().unwrap();
    let (c, events) = converter(root.path());
    for name in ["cenc-iv8.mp4", "cbcs-joint.mp4"] {
        let input = fixture(name);
        let original = std::fs::read(&input).unwrap();
        let output = root.path().join(name);
        let wrong_audio = keys(&[
            (VIDEO_ID, VIDEO_KEY),
            (AUDIO_ID, "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"),
        ]);
        // These modes do not authenticate a key. A structural refusal is possible,
        // but a completed native copy still needs the independent clear reference.
        match copy(&c, &input, &wrong_audio, &output) {
            Ok(_) => assert_differs_from_clear_reference(&c, &output),
            Err(_) => assert!(!output.exists()),
        }
        assert_eq!(std::fs::read(&input).unwrap(), original);
    }
    assert_no_tools(&events);
}
