//! Complete finite presentations retain their declared playback windows in every output.
mod common;

use drmr_core::{
    conversion::{
        Aes128Key, ContentKey, ConversionRequest, Converter, HlsInputKeys, HlsOutputKey, InputKeys,
        OutputKey, ValidationDepth, VerificationRequest, VerificationSource,
    },
    events::{Collector, NoSink},
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{path::Path, sync::Arc};
const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "42424242424242424242424242424242";
fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))),
        "packager",
        "ffmpeg",
    )
    .with_decode(true)
}

#[test]
fn native_finite_progressive_copy_needs_no_media_tools() {
    let root = common::private_tempdir().unwrap();
    let events = Arc::new(Collector::default());
    let c = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
        root.path().join("absent-packager").to_string_lossy(),
        root.path().join("absent-ffmpeg").to_string_lossy(),
    )
    .with_ffprobe(root.path().join("absent-ffprobe").to_string_lossy());
    let input_keys = InputKeys::new(vec![
        ContentKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap(),
        ContentKey::from_hex(
            "303132333435363738393a3b3c3d3e3f",
            "202122232425262728292a2b2c2d2e2f",
        )
        .unwrap(),
    ])
    .unwrap();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let reference = fixture_path("clip.mp4");
    for name in ["cenc-iv8.mp4", "cenc-iv16.mp4", "cbcs-joint.mp4"] {
        let input = fixture_path(name);
        let original = std::fs::read(&input).unwrap();
        let output = root.path().join(format!("clear-{name}"));
        let converted = c
            .convert(ConversionRequest {
                input: &input,
                output: &output,
                format: OutputFormat::Mp4,
                target: TargetProtection::Clear,
                input_keys: &input_keys,
                hls_input_keys: &hls,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_native_verified(&converted.verification);
        assert_eq!(
            serde_json::to_value(&converted).unwrap()["processing"]["required_tools"],
            serde_json::json!([])
        );
        // The independent clear original also catches a consistently wrong decrypt/write pair.
        for (source, keys) in [(&input, &input_keys), (&reference, &empty)] {
            let verified = c
                .verify(VerificationRequest {
                    source: VerificationSource {
                        input: source,
                        input_keys: keys,
                        hls_keys: &hls,
                    },
                    output: VerificationSource {
                        input: &output,
                        input_keys: &empty,
                        hls_keys: &hls,
                    },
                })
                .unwrap_or_else(|error| panic!("{name}: independent verification: {error}"));
            assert_native_verified(&verified);
        }
        let clear = std::fs::read(&output).unwrap();
        assert_progressive_layout_unchanged(&original, &clear);
        for node in drmr_core::mp4::walk(&drmr_core::mp4::parse_boxes(&clear).unwrap()) {
            assert!(
                !matches!(
                    &node.kind(),
                    b"encv" | b"enca" | b"sinf" | b"tenc" | b"senc" | b"saiz" | b"saio" | b"pssh"
                ),
                "{name}: retained encryption metadata: {:?}",
                node.kind()
            );
        }
        assert_eq!(std::fs::read(&input).unwrap(), original);
    }
    assert!(
        events.records().iter().all(|record| {
            record.get("stage").and_then(serde_json::Value::as_str) != Some("tool")
        })
    );
}

fn assert_native_verified(report: &drmr_core::conversion::VerificationReport) {
    assert!(report.passed && report.checks.is_pass(), "{report:?}");
    assert_eq!(
        report.decode_evidence,
        drmr_core::conversion::DecodeEvidence::not_requested()
    );
    assert!(!report.license_playback_verified);
}

fn assert_progressive_layout_unchanged(original: &[u8], clear: &[u8]) {
    assert_eq!(original.len(), clear.len());
    let roots = drmr_core::mp4::parse_boxes(original).unwrap();
    let copied = drmr_core::mp4::parse_boxes(clear).unwrap();
    assert_eq!(
        roots.iter().map(|node| node.header).collect::<Vec<_>>(),
        copied.iter().map(|node| node.header).collect::<Vec<_>>()
    );
    for node in drmr_core::mp4::walk(&roots) {
        match &node.kind() {
            b"mvhd" | b"tkhd" | b"mdhd" | b"elst" | b"stts" | b"ctts" | b"stsc" | b"stsz"
            | b"stco" | b"co64" | b"stss" | b"sgpd" | b"sbgp" | b"avcC" | b"esds" => {
                assert_eq!(node.header.bytes(original), node.header.bytes(clear));
            }
            b"sinf" | b"senc" | b"saiz" | b"saio" => {
                assert_eq!(
                    &clear[node.header.offset + 4..node.header.offset + 8],
                    b"free"
                );
                assert!(node.header.payload(clear).iter().all(|byte| *byte == 0));
            }
            _ => {}
        }
    }
}

#[test]
fn complete_finite_audio_survives_all_nine_output_profiles() {
    finite_output_matrix(&converter(), ValidationDepth::Decoded);
}

#[test]
fn native_finite_audio_survives_all_nine_output_profiles_without_decoder_tools() {
    let root = common::private_tempdir().unwrap();
    let events = Arc::new(Collector::default());
    let c = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
        "packager",
        root.path().join("absent-ffmpeg").to_string_lossy(),
    )
    .with_ffprobe(root.path().join("absent-ffprobe").to_string_lossy());
    finite_output_matrix(&c, ValidationDepth::Native);
    let records = events.records();
    assert!(records.iter().any(|record| {
        record.get("stage").and_then(serde_json::Value::as_str) == Some("tool")
            && record.get("tool").and_then(serde_json::Value::as_str) == Some("packager")
    }));
    for record in records {
        assert!(
            !matches!(
                record.get("tool").and_then(serde_json::Value::as_str),
                Some("ffmpeg" | "ffprobe")
            ),
            "unexpected decoder tool: {record:?}"
        );
    }
}

fn finite_output_matrix(c: &Converter, depth: ValidationDepth) {
    let d = tempfile::tempdir().unwrap();
    let input = d.path().join("audio.mp4");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let result = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(fixture)
        .args(["-map", "0:a:0", "-c", "copy"])
        .arg(&input)
        .output()
        .unwrap();
    assert!(result.status.success());
    let protected_audio = d.path().join("audio-cenc.mp4");
    let result = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&input)
        .args([
            "-map",
            "0:0",
            "-c",
            "copy",
            "-encryption_scheme",
            "cenc-aes-ctr",
            "-encryption_kid",
            "303132333435363738393a3b3c3d3e3f",
            "-encryption_key",
            "202122232425262728292a2b2c2d2e2f",
        ])
        .arg(&protected_audio)
        .output()
        .unwrap();
    assert!(result.status.success());
    for (label, input, input_keys) in sources(input, protected_audio) {
        for (format, target) in [
            (OutputFormat::Mp4, TargetProtection::Clear),
            (OutputFormat::Mp4, TargetProtection::Cenc),
            (OutputFormat::Mp4, TargetProtection::Cbcs),
            (OutputFormat::Dash, TargetProtection::Clear),
            (OutputFormat::Dash, TargetProtection::Cenc),
            (OutputFormat::Dash, TargetProtection::Cbcs),
            (OutputFormat::Hls, TargetProtection::Clear),
            (OutputFormat::Hls, TargetProtection::Cbcs),
            (OutputFormat::Hls, TargetProtection::HlsAes128),
        ] {
            let output = d
                .path()
                .join(format!("{label}-{}-{}", format.as_str(), target.as_str()));
            let original = std::fs::read(&input).unwrap();
            run_profile_at(c, &input, &input_keys, &output, format, target, depth);
            assert_eq!(std::fs::read(&input).unwrap(), original);
            println!(
                "finite-output-cell {}",
                serde_json::json!({"input":label,"format":format.as_str(),"target":target.as_str(),"depth":depth,"native_checks":true,"all_four_checks":depth == ValidationDepth::Decoded,"fresh_verification":true,"source_immutable":true,"license_playback_verified":false})
            );
        }
    }
}

fn sources(
    input: std::path::PathBuf,
    protected_audio: std::path::PathBuf,
) -> Vec<(&'static str, std::path::PathBuf, InputKeys)> {
    vec![
        ("audio-clear", input, InputKeys::default()),
        (
            "audio-cbcs",
            fixture_path("cbcs-audio.mp4"),
            InputKeys::new(vec![
                ContentKey::from_hex(
                    "303132333435363738393a3b3c3d3e3f",
                    "202122232425262728292a2b2c2d2e2f",
                )
                .unwrap(),
            ])
            .unwrap(),
        ),
        (
            "audio-cenc",
            protected_audio,
            InputKeys::new(vec![
                ContentKey::from_hex(
                    "303132333435363738393a3b3c3d3e3f",
                    "202122232425262728292a2b2c2d2e2f",
                )
                .unwrap(),
            ])
            .unwrap(),
        ),
        (
            "joint-clear",
            fixture_path("clip.mp4"),
            InputKeys::default(),
        ),
        (
            "joint-cbcs",
            fixture_path("cbcs-joint.mp4"),
            InputKeys::new(vec![
                ContentKey::from_hex(
                    "000102030405060708090a0b0c0d0e0f",
                    "101112131415161718191a1b1c1d1e1f",
                )
                .unwrap(),
                ContentKey::from_hex(
                    "303132333435363738393a3b3c3d3e3f",
                    "202122232425262728292a2b2c2d2e2f",
                )
                .unwrap(),
            ])
            .unwrap(),
        ),
        (
            "joint-cenc",
            fixture_path("cenc-iv8.mp4"),
            InputKeys::new(vec![
                ContentKey::from_hex(
                    "000102030405060708090a0b0c0d0e0f",
                    "101112131415161718191a1b1c1d1e1f",
                )
                .unwrap(),
                ContentKey::from_hex(
                    "303132333435363738393a3b3c3d3e3f",
                    "202122232425262728292a2b2c2d2e2f",
                )
                .unwrap(),
            ])
            .unwrap(),
        ),
    ]
}

fn fixture_path(name: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}

fn assert_verified(r: &drmr_core::conversion::VerificationReport) {
    assert!(
        r.passed
            && (r.checks.is_pass()
                && r.decode_evidence
                    .satisfies(drmr_core::conversion::ValidationDepth::Decoded, r.scope))
    );
    assert!(!r.license_playback_verified);
    assert_eq!(
        serde_json::to_value(r).unwrap()["checks"]["stream_metadata"],
        "pass"
    );
    assert!(
        r.checks.packet_payloads_equal
            && r.checks.packet_timing_equal
            && r.decode_evidence
                .satisfies(drmr_core::conversion::ValidationDepth::Decoded, r.scope)
    );
}

fn run_profile(
    c: &Converter,
    input: &Path,
    input_keys: &InputKeys,
    output: &Path,
    format: OutputFormat,
    target: TargetProtection,
) {
    run_profile_at(
        c,
        input,
        input_keys,
        output,
        format,
        target,
        ValidationDepth::Decoded,
    );
}

fn run_profile_at(
    c: &Converter,
    input: &Path,
    input_keys: &InputKeys,
    output: &Path,
    format: OutputFormat,
    target: TargetProtection,
    depth: ValidationDepth,
) {
    let hls = HlsInputKeys::default();
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    let aes = HlsOutputKey::new(
        "https://keys.invalid/finite",
        Aes128Key::from_hex(KEY).unwrap(),
    )
    .unwrap();
    let report = c
        .convert(ConversionRequest {
            input,
            output,
            format,
            target,
            input_keys,
            hls_input_keys: &hls,
            output_key: matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs)
                .then_some(&key),
            hls_key_uri: (format == OutputFormat::Hls && target == TargetProtection::Cbcs)
                .then_some("https://keys.invalid/finite"),
            hls_output_key: (target == TargetProtection::HlsAes128).then_some(&aes),
        })
        .unwrap_or_else(|error| panic!("{} {}: {}", format.as_str(), target.as_str(), error));
    match depth {
        ValidationDepth::Native => assert_native_verified(&report.verification),
        ValidationDepth::Decoded => assert_verified(&report.verification),
    }

    let result = match format {
        OutputFormat::Mp4 => output.to_path_buf(),
        OutputFormat::Dash => output.join("manifest.mpd"),
        _ => output.join("master.m3u8"),
    };
    let resultkeys = if matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs) {
        InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap()
    } else {
        InputKeys::default()
    };
    let hlskeys = if target == TargetProtection::HlsAes128 {
        HlsInputKeys::new(vec![(
            "https://keys.invalid/finite".into(),
            Aes128Key::from_hex(KEY).unwrap(),
        )])
        .unwrap()
    } else {
        HlsInputKeys::default()
    };
    let verified = c
        .verify(VerificationRequest {
            source: VerificationSource {
                input,
                input_keys,
                hls_keys: &hls,
            },
            output: VerificationSource {
                input: &result,
                input_keys: &resultkeys,
                hls_keys: &hlskeys,
            },
        })
        .unwrap_or_else(|error| {
            panic!("verify {} {}: {}", format.as_str(), target.as_str(), error)
        });
    match depth {
        ValidationDepth::Native => assert_native_verified(&verified),
        ValidationDepth::Decoded => assert_verified(&verified),
    }
}

#[test]
fn a_finite_end_inside_the_last_coded_frame_is_preserved_by_every_output() {
    let d = tempfile::tempdir().unwrap();
    let input = d.path().join("short.mp4");
    assert!(
        std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(fixture_path("clip.mp4"))
            .args(["-map", "0:a:0", "-c", "copy"])
            .arg(&input)
            .status()
            .unwrap()
            .success()
    );
    let mut data = std::fs::read(&input).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&data).unwrap();
    let fields: Vec<_> = drmr_core::mp4::walk(&boxes)
        .into_iter()
        .filter_map(|n| match n.kind() {
            k if k == *b"elst" => Some(n.header.offset + n.header.header_size + 8),
            k if k == *b"tkhd" => Some(n.header.offset + n.header.header_size + 20),
            k if k == *b"mvhd" => Some(n.header.offset + n.header.header_size + 16),
            _ => None,
        })
        .collect();
    assert_eq!(fields.len(), 3);
    for at in fields {
        data[at..at + 4].copy_from_slice(&479_521u32.to_be_bytes());
    }
    std::fs::write(&input, data).unwrap();
    let c = converter();
    let keys = InputKeys::default();
    for (format, target) in [
        (OutputFormat::Mp4, TargetProtection::Clear),
        (OutputFormat::Mp4, TargetProtection::Cenc),
        (OutputFormat::Mp4, TargetProtection::Cbcs),
        (OutputFormat::Dash, TargetProtection::Clear),
        (OutputFormat::Dash, TargetProtection::Cenc),
        (OutputFormat::Dash, TargetProtection::Cbcs),
        (OutputFormat::Hls, TargetProtection::Clear),
        (OutputFormat::Hls, TargetProtection::Cbcs),
        (OutputFormat::Hls, TargetProtection::HlsAes128),
    ] {
        run_profile(
            &c,
            &input,
            &keys,
            &d.path()
                .join(format!("{}-{}", format.as_str(), target.as_str())),
            format,
            target,
        );
    }
}

#[test]
fn reordered_video_keeps_negative_decode_times_and_finite_audio_in_encrypted_outputs() {
    let d = tempfile::tempdir().unwrap();
    let input = d.path().join("reordered.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(fixture_path("clip.mp4"))
        .args([
            "-map", "0:v:0", "-map", "0:a:0", "-c:v", "libx264", "-bf", "2", "-g", "48", "-c:a",
            "copy",
        ])
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());
    let c = converter();
    let keys = InputKeys::default();
    for (format, target) in [
        (OutputFormat::Mp4, TargetProtection::Cbcs),
        (OutputFormat::Dash, TargetProtection::Cenc),
        (OutputFormat::Hls, TargetProtection::Cbcs),
    ] {
        run_profile(
            &c,
            &input,
            &keys,
            &d.path()
                .join(format!("{}-{}", format.as_str(), target.as_str())),
            format,
            target,
        );
    }
}
#[test]
fn candidate_window_changes_do_not_pass_with_unchanged_encrypted_packets() {
    let d = tempfile::tempdir().unwrap();
    let input = fixture_path("cbcs-audio.mp4");
    let output = d.path().join("encrypted.mp4");
    let keys = InputKeys::new(vec![
        ContentKey::from_hex(
            "303132333435363738393a3b3c3d3e3f",
            "202122232425262728292a2b2c2d2e2f",
        )
        .unwrap(),
    ])
    .unwrap();
    let c = converter();
    run_profile(
        &c,
        &input,
        &keys,
        &output,
        OutputFormat::Mp4,
        TargetProtection::Cbcs,
    );
    let original = std::fs::read(&output).unwrap();
    let nodes = drmr_core::mp4::parse_boxes(&original).unwrap();
    let elst = drmr_core::mp4::walk(&nodes)
        .into_iter()
        .find(|n| n.kind() == *b"elst")
        .unwrap();
    let at = elst.header.offset + elst.header.header_size;
    assert_eq!(original[at], 1);
    let result_keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
    let empty = HlsInputKeys::default();
    for (offset, value) in [(8, 479_999u64), (16, 1025u64), (8, 0u64)] {
        let mut changed = original.clone();
        changed[at + offset..at + offset + 8].copy_from_slice(&value.to_be_bytes());
        std::fs::write(&output, changed).unwrap();
        let result = c.verify(VerificationRequest {
            source: VerificationSource {
                input: &input,
                input_keys: &keys,
                hls_keys: &empty,
            },
            output: VerificationSource {
                input: &output,
                input_keys: &result_keys,
                hls_keys: &empty,
            },
        });
        assert!(result.is_err() || result.is_ok_and(|r| !r.passed));
    }
}
#[test]
fn unsupported_cbcs_pattern_and_vector_size_are_refused_before_tools() {
    use drmr_core::events::Collector;
    let d = tempfile::tempdir().unwrap();
    let original = std::fs::read(fixture_path("cbcs-audio.mp4")).unwrap();
    let nodes = drmr_core::mp4::parse_boxes(&original).unwrap();
    let tenc = drmr_core::mp4::walk(&nodes)
        .into_iter()
        .find(|n| n.kind() == *b"tenc")
        .unwrap();
    let at = tenc.header.offset + tenc.header.header_size;
    let keys = InputKeys::new(vec![
        ContentKey::from_hex(
            "303132333435363738393a3b3c3d3e3f",
            "202122232425262728292a2b2c2d2e2f",
        )
        .unwrap(),
    ])
    .unwrap();
    let empty = HlsInputKeys::default();
    for (offset, value) in [(5, 0x19), (24, 8)] {
        let events = Arc::new(Collector::default());
        let c = Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
            "packager",
            "ffmpeg",
        )
        .with_decode(true);
        let source = d.path().join(format!("source-{offset}.mp4"));
        let output = d.path().join(format!("result-{offset}"));
        let mut bytes = original.clone();
        bytes[at + offset] = value;
        std::fs::write(&source, &bytes).unwrap();
        let result = c.convert(ConversionRequest {
            input: &source,
            output: &output,
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &keys,
            hls_input_keys: &empty,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        });
        assert!(result.is_err());
        assert!(!output.exists());
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert!(
            !events
                .records()
                .iter()
                .any(|r| r.get("event").and_then(|v| v.as_str()) == Some("command"))
        );
    }
}

#[test]
fn fragmented_playback_controls_remain_bound_to_the_original_presentation() {
    let d = tempfile::tempdir().unwrap();
    let input = fixture_path("clip.mp4");
    let output = d.path().join("encrypted.mp4");
    let c = converter();
    let source_keys = InputKeys::default();
    run_profile(
        &c,
        &input,
        &source_keys,
        &output,
        OutputFormat::Mp4,
        TargetProtection::Cbcs,
    );
    let original = std::fs::read(&output).unwrap();
    let nodes = drmr_core::mp4::parse_boxes(&original).unwrap();
    let mut mutations = Vec::new();
    for n in drmr_core::mp4::walk(&nodes) {
        let at = n.header.offset + n.header.header_size;
        if n.kind() == *b"mvhd" {
            assert_eq!(original[at], 0);
            mutations.push(("movie-rate", at + 20, (2u32 << 16).to_be_bytes().to_vec()));
            mutations.push(("movie-volume", at + 24, vec![0, 0]));
        } else if n.kind() == *b"tkhd" {
            assert_eq!(original[at], 0);
            mutations.push(("track-enabled", at + 3, vec![0]));
            mutations.push(("track-group", at + 34, vec![0, 2]));
            if original[at + 36..at + 38] == [1, 0] {
                mutations.push(("audio-volume", at + 36, vec![0, 0]));
            } else {
                let width = u32::from_be_bytes(original[at + 76..at + 80].try_into().unwrap());
                mutations.push((
                    "display-extent",
                    at + 76,
                    (width * 2).to_be_bytes().to_vec(),
                ));
            }
        }
    }
    assert_eq!(mutations.len(), 8);
    let result_keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
    let hls = HlsInputKeys::default();
    for (label, at, bytes) in mutations {
        let mut changed = original.clone();
        changed[at..at + bytes.len()].copy_from_slice(&bytes);
        std::fs::write(&output, changed).unwrap();
        let result = c.verify(VerificationRequest {
            source: VerificationSource {
                input: &input,
                input_keys: &source_keys,
                hls_keys: &hls,
            },
            output: VerificationSource {
                input: &output,
                input_keys: &result_keys,
                hls_keys: &hls,
            },
        });
        assert!(
            result.is_err() || result.is_ok_and(|r| !r.passed),
            "{label}"
        );
        let mp4 = c.verify_mp4(&input, &source_keys, &output, &result_keys);
        assert!(mp4.is_err() || mp4.is_ok_and(|r| !r.passed), "mp4 {label}");
    }
}

#[test]
fn a_single_sample_manifest_change_cannot_redefine_finite_media_presentation() {
    let d = tempfile::tempdir().unwrap();
    let input = fixture_path("cbcs-audio.mp4");
    let keys = InputKeys::new(vec![
        ContentKey::from_hex(
            "303132333435363738393a3b3c3d3e3f",
            "202122232425262728292a2b2c2d2e2f",
        )
        .unwrap(),
    ])
    .unwrap();
    let c = converter();
    let empty_keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    for format in [OutputFormat::Hls, OutputFormat::Dash] {
        let output = d.path().join(format.as_str());
        run_profile(&c, &input, &keys, &output, format, TargetProtection::Clear);
        let entry = output.join(if format == OutputFormat::Hls {
            "master.m3u8"
        } else {
            "manifest.mpd"
        });
        let manifest = if format == OutputFormat::Hls {
            output.join("track-0000/media.m3u8")
        } else {
            entry.clone()
        };
        let original = std::fs::read_to_string(&manifest).unwrap();
        let changed = if format == OutputFormat::Hls {
            let line = original
                .lines()
                .rfind(|line| line.starts_with("#EXTINF:"))
                .unwrap();
            let seconds: f64 = line
                .strip_prefix("#EXTINF:")
                .unwrap()
                .split(',')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            original.replacen(line, &format!("#EXTINF:{:.9},", seconds - 1.0 / 48000.0), 1)
        } else {
            let doc = roxmltree::Document::parse(&original).unwrap();
            let last = doc
                .descendants()
                .rfind(|n| n.is_element() && n.tag_name().name() == "S")
                .unwrap();
            let duration = last.attribute_node("d").unwrap();
            let value: u64 = duration.value().parse().unwrap();
            let mut changed = original.clone();
            changed.replace_range(duration.range_value(), &(value - 1).to_string());
            changed
        };
        std::fs::write(&manifest, changed).unwrap();
        let verified = c.verify(VerificationRequest {
            source: VerificationSource {
                input: &input,
                input_keys: &keys,
                hls_keys: &hls,
            },
            output: VerificationSource {
                input: &entry,
                input_keys: &empty_keys,
                hls_keys: &hls,
            },
        });
        assert!(
            verified.is_err() || verified.is_ok_and(|r| !r.passed),
            "{}",
            format.as_str()
        );
    }
}

#[test]
fn cbcs_joint_auxiliary_corruption_and_swapped_role_keys_never_publish() {
    let d = tempfile::tempdir().unwrap();
    let input = fixture_path("cbcs-joint.mp4");
    let original = std::fs::read(&input).unwrap();
    let mut cases = Vec::new();
    for n in drmr_core::mp4::walk(&drmr_core::mp4::parse_boxes(&original).unwrap()) {
        let at = n.header.offset + n.header.header_size;
        let field = match &n.kind() {
            b"senc" => Some(at + 4),
            b"saiz" => Some(at + 5),
            _ => None,
        };
        if let Some(at) = field {
            let mut bytes = original.clone();
            bytes[at..at + 4].copy_from_slice(&239u32.to_be_bytes());
            cases.push(bytes);
        }
    }
    assert_eq!(cases.len(), 2);
    let valid = sources(input.clone(), input.clone())
        .into_iter()
        .find(|(label, _, _)| *label == "joint-cbcs")
        .unwrap()
        .2;
    let hls = HlsInputKeys::default();
    for (index, bytes) in cases.into_iter().enumerate() {
        let source = d.path().join(format!("invalid-{index}.mp4"));
        std::fs::write(&source, &bytes).unwrap();
        let events = Arc::new(drmr_core::events::Collector::default());
        let converter = Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
            "packager",
            "ffmpeg",
        )
        .with_decode(true);
        let output = d.path().join(format!("output-{index}"));
        assert!(
            converter
                .convert(ConversionRequest {
                    input: &source,
                    output: &output,
                    format: OutputFormat::Dash,
                    target: TargetProtection::Clear,
                    input_keys: &valid,
                    hls_input_keys: &hls,
                    output_key: None,
                    hls_key_uri: None,
                    hls_output_key: None
                })
                .is_err()
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read(&source).unwrap(), bytes);
        assert!(!events.records().iter().any(|r| r["event"] == "command"));
    }
    let swapped = InputKeys::new(vec![
        ContentKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "202122232425262728292a2b2c2d2e2f",
        )
        .unwrap(),
        ContentKey::from_hex(
            "303132333435363738393a3b3c3d3e3f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap(),
    ])
    .unwrap();
    let converter = converter();
    let output = d.path().join("wrong-keys");
    assert!(
        converter
            .convert(ConversionRequest {
                input: &input,
                output: &output,
                format: OutputFormat::Dash,
                target: TargetProtection::Clear,
                input_keys: &swapped,
                hls_input_keys: &hls,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None
            })
            .is_err()
    );
    assert!(!output.exists());
    assert_eq!(std::fs::read(&input).unwrap(), original);
    run_profile(
        &converter,
        &input,
        &valid,
        &d.path().join("recovered"),
        OutputFormat::Dash,
        TargetProtection::Clear,
    );
}

#[test]
fn finite_joint_mp4_conversion_and_mp4_verification_share_the_same_key_domains() {
    let d = tempfile::tempdir().unwrap();
    let c = converter();
    let output_key = OutputKey::from_hex(KID, KEY).unwrap();
    let output_keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
    let wrong_keys = InputKeys::new(vec![
        ContentKey::from_hex(KID, "00000000000000000000000000000000").unwrap(),
    ])
    .unwrap();
    let wrong_input = InputKeys::new(vec![
        ContentKey::from_hex(KID, "00000000000000000000000000000000").unwrap(),
        ContentKey::from_hex(
            "303132333435363738393a3b3c3d3e3f",
            "11111111111111111111111111111111",
        )
        .unwrap(),
    ])
    .unwrap();
    let empty = InputKeys::default();
    for (label, input, input_keys) in
        sources(fixture_path("clip.mp4"), fixture_path("cenc-iv8.mp4"))
            .into_iter()
            .filter(|(label, _, _)| label.starts_with("joint-"))
    {
        let original = std::fs::read(&input).unwrap();
        for target in [TargetProtection::Cenc, TargetProtection::Cbcs] {
            let output = d.path().join(format!("{label}-{}.mp4", target.as_str()));
            let report = c
                .convert_mp4(&input, &output, target, &input_keys, Some(&output_key))
                .unwrap();
            assert_verified(&report.verification);
            let encoded = std::fs::read(&output).unwrap();
            assert_verified(
                &c.verify_mp4(&input, &input_keys, &output, &output_keys)
                    .unwrap(),
            );
            for keys in [&empty, &wrong_keys] {
                let denied = c.verify_mp4(&input, &input_keys, &output, keys);
                assert!(denied.is_err() || denied.is_ok_and(|v| !v.passed));
            }
            if label != "joint-clear" {
                for (a, b) in [
                    (&empty, &output_keys),
                    (&wrong_input, &output_keys),
                    (&output_keys, &input_keys),
                ] {
                    let denied = c.verify_mp4(&input, a, &output, b);
                    assert!(denied.is_err() || denied.is_ok_and(|v| !v.passed));
                }
            }
            assert_eq!(std::fs::read(&output).unwrap(), encoded);
            assert_eq!(std::fs::read(&input).unwrap(), original);
        }
    }
}

#[test]
fn the_mp4_specific_verifier_keeps_nonfinite_joint_inputs_outside_the_profile() {
    let d = tempfile::tempdir().unwrap();
    let input = d.path().join("open-ended.mp4");
    let output = d.path().join("protected.mp4");
    let c = converter();
    c.convert_mp4(
        &fixture_path("clip.mp4"),
        &output,
        TargetProtection::Cenc,
        &InputKeys::default(),
        Some(&OutputKey::from_hex(KID, KEY).unwrap()),
    )
    .unwrap();
    let result_keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
    let mut bytes = std::fs::read(fixture_path("clip.mp4")).unwrap();
    let nodes = drmr_core::mp4::parse_boxes(&bytes).unwrap();
    let mut changed = 0;
    for n in drmr_core::mp4::walk(&nodes) {
        if n.kind() == *b"elst" {
            let at = n.header.offset + n.header.header_size;
            assert_eq!(bytes[at], 0);
            bytes[at + 8..at + 12].copy_from_slice(&0u32.to_be_bytes());
            changed += 1;
        }
    }
    assert!(changed > 0);
    std::fs::write(&input, &bytes).unwrap();
    let error = c
        .verify_mp4(&input, &InputKeys::default(), &output, &result_keys)
        .unwrap_err();
    assert_eq!(error.0, "single-track-mp4-required");
    assert_eq!(std::fs::read(&input).unwrap(), bytes);
}
