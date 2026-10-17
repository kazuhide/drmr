use drmr_core::conversion::{ContentKey, ConversionError, Converter, InputKeys, OutputKey};
use drmr_core::events::{Collector, NoSink};
use drmr_core::inspection::{Protection, TargetProtection, inspect};
use drmr_core::toolrun::{Secrets, ToolRunner};
use std::{path::Path, sync::Arc};

const KID_IN: &str = "000102030405060708090a0b0c0d0e0f";
const KEY_IN: &str = "101112131415161718191a1b1c1d1e1f";
const KID_OUT: &str = "202122232425262728292a2b2c2d2e2f";
const KEY_OUT: &str = "303132333435363738393a3b3c3d3e3f";
fn runner() -> Arc<ToolRunner> {
    Arc::new(ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink)))
}
fn converter() -> Converter {
    Converter::new(runner(), "packager", "ffmpeg")
}
fn input_keys(kid: &str, key: &str) -> InputKeys {
    InputKeys::new(vec![ContentKey::from_hex(kid, key).unwrap()]).unwrap()
}
fn video(path: &Path, source: &str) {
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error", "-f", "lavfi", "-i", source, "-t", "1", "-c:v", "libx264", "-pix_fmt",
            "yuv420p", "-g", "5", "-an",
        ])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
fn keys_validate_without_exposing_values_in_debug_or_errors() {
    let key = ContentKey::from_hex(KID_IN, KEY_IN).unwrap();
    assert!(!format!("{key:?}").contains(KEY_IN));
    for value in ["", "SECRET", "0", "0g00000000000000000000000000000000"] {
        let error = ContentKey::from_hex(KID_IN, value).unwrap_err();
        assert_eq!(error, ConversionError("invalid-content-key"));
        assert!(!format!("{error:?}").contains(value) || value.is_empty());
    }
    assert!(
        InputKeys::new(vec![
            ContentKey::from_hex(KID_IN, KEY_IN).unwrap(),
            ContentKey::from_hex(KID_IN, KEY_OUT).unwrap()
        ])
        .is_err()
    );
}
#[test]
fn real_mp4_clear_cenc_cbcs_nine_routes_use_independent_output_keys() {
    let directory = tempfile::tempdir().unwrap();
    let clear = directory.path().join("clear.mp4");
    video(&clear, "testsrc2=size=64x64:rate=5");
    let converter = converter();
    let source_key = OutputKey::from_hex(KID_IN, KEY_IN).unwrap();
    let output_key = OutputKey::from_hex(KID_OUT, KEY_OUT).unwrap();
    let empty = InputKeys::default();
    let supplied = input_keys(KID_IN, KEY_IN);
    for source in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        let input = directory
            .path()
            .join(format!("source-{}.mp4", source.as_str()));
        converter
            .convert_mp4(
                &clear,
                &input,
                source,
                &empty,
                (source != TargetProtection::Clear).then_some(&source_key),
            )
            .unwrap();
        for target in [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ] {
            let output =
                directory
                    .path()
                    .join(format!("{}-{}.mp4", source.as_str(), target.as_str()));
            let report = converter
                .convert_mp4(
                    &input,
                    &output,
                    target,
                    if source == TargetProtection::Clear {
                        &empty
                    } else {
                        &supplied
                    },
                    (target != TargetProtection::Clear).then_some(&output_key),
                )
                .unwrap();
            assert!(report.verification.passed);
            assert!(report.verification.checks.decoded_content_equal);
            assert!(report.verification.checks.packet_payloads_equal);
            assert!(!report.verification.license_playback_verified);
            assert_eq!(
                report.output.protection,
                match target {
                    TargetProtection::Clear => Protection::Clear,
                    TargetProtection::Cenc => Protection::Cenc,
                    TargetProtection::Cbcs => Protection::Cbcs,
                    TargetProtection::HlsAes128 => unreachable!(),
                }
            );
            if target != TargetProtection::Clear {
                assert_eq!(inspect(&output).unwrap().tracks[0].key_ids, [KID_OUT]);
            }
        }
    }
}
#[test]
fn wrong_input_key_and_overwrite_fail_without_publishing_output() {
    let directory = tempfile::tempdir().unwrap();
    let clear = directory.path().join("clear.mp4");
    video(&clear, "testsrc2=size=64x64:rate=5");
    let converter = converter();
    let encrypted = directory.path().join("encrypted.mp4");
    converter
        .convert_mp4(
            &clear,
            &encrypted,
            TargetProtection::Cenc,
            &InputKeys::default(),
            Some(&OutputKey::from_hex(KID_IN, KEY_IN).unwrap()),
        )
        .unwrap();
    let output = directory.path().join("out.mp4");
    assert!(
        converter
            .convert_mp4(
                &encrypted,
                &output,
                TargetProtection::Clear,
                &input_keys(KID_IN, KEY_OUT),
                None
            )
            .is_err()
    );
    assert!(!output.exists());
    std::fs::write(&output, b"keep").unwrap();
    assert!(
        converter
            .convert_mp4(
                &clear,
                &output,
                TargetProtection::Clear,
                &InputKeys::default(),
                None
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&output).unwrap(), b"keep");
}
#[test]
fn mismatched_media_does_not_pass_verification() {
    let directory = tempfile::tempdir().unwrap();
    let a = directory.path().join("a.mp4");
    let b = directory.path().join("b.mp4");
    video(&a, "testsrc2=size=64x64:rate=5");
    video(&b, "color=blue:size=64x64:rate=5");
    let report = converter()
        .verify_mp4(&a, &InputKeys::default(), &b, &InputKeys::default())
        .unwrap();
    assert!(!report.passed);
    assert!(!report.checks.decoded_content_equal);
}
#[test]
fn keys_never_reach_retained_tool_records_or_reports() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    video(&input, "testsrc2=size=64x64:rate=5");
    let sink = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), sink.clone()));
    let converter = Converter::new(runner, "packager", "ffmpeg");
    let report = converter
        .convert_mp4(
            &input,
            &directory.path().join("output.mp4"),
            TargetProtection::Cbcs,
            &InputKeys::default(),
            Some(&OutputKey::from_hex(KID_OUT, KEY_OUT).unwrap()),
        )
        .unwrap();
    assert!(
        !serde_json::to_string(&sink.records())
            .unwrap()
            .contains(KEY_OUT)
    );
    assert!(!serde_json::to_string(&report).unwrap().contains(KEY_OUT));
}
#[cfg(unix)]
#[test]
fn output_symlinks_are_rejected_without_modifying_their_target() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("input.mp4");
    video(&source, "testsrc2=size=64x64:rate=5");
    let victim = directory.path().join("victim");
    std::fs::write(&victim, b"keep").unwrap();
    let output = directory.path().join("output.mp4");
    std::os::unix::fs::symlink(&victim, &output).unwrap();
    assert!(
        converter()
            .convert_mp4(
                &source,
                &output,
                TargetProtection::Clear,
                &InputKeys::default(),
                None
            )
            .is_err()
    );
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep");
}

#[test]
fn missing_keys_unsupported_profiles_and_multi_track_inputs_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("input.mp4");
    video(&source, "testsrc2=size=64x64:rate=5");
    let output = directory.path().join("output.mp4");
    let converter = converter();
    assert_eq!(
        converter
            .convert_mp4(
                &source,
                &output,
                TargetProtection::Cenc,
                &InputKeys::default(),
                None
            )
            .unwrap_err(),
        ConversionError("output-key-required")
    );
    assert_eq!(
        converter
            .convert_mp4(
                &source,
                &output,
                TargetProtection::HlsAes128,
                &InputKeys::default(),
                None
            )
            .unwrap_err(),
        ConversionError("target-requires-hls-output")
    );
    assert!(!output.exists());
    let av = directory.path().join("av.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=64x64:rate=5",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "1",
            "-c:v",
            "libx264",
            "-c:a",
            "aac",
        ])
        .arg(&av)
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(
        converter
            .convert_mp4(
                &av,
                &output,
                TargetProtection::Clear,
                &InputKeys::default(),
                None
            )
            .unwrap_err(),
        ConversionError("single-track-mp4-required")
    );
    assert!(!output.exists());
}
#[test]
fn cbcs_output_uses_fresh_random_initialization_vectors_and_private_permissions() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("input.mp4");
    video(&source, "testsrc2=size=64x64:rate=5");
    let converter = converter();
    let key = OutputKey::from_hex(KID_OUT, KEY_OUT).unwrap();
    let mut vectors = Vec::new();
    for name in ["a.mp4", "b.mp4"] {
        let output = directory.path().join(name);
        converter
            .convert_mp4(
                &source,
                &output,
                TargetProtection::Cbcs,
                &InputKeys::default(),
                Some(&key),
            )
            .unwrap();
        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
        #[cfg(windows)]
        private_fs::verify(&drmr_core::native::open_private_read(&output).unwrap()).unwrap();
        let bytes = std::fs::read(&output).unwrap();
        let entries = drmr_core::mp4::sample_entries(&bytes).unwrap();
        let tenc = entries[0].tenc.as_ref().unwrap();
        vectors.push(tenc.default_constant_iv.as_ref().unwrap().to_vec());
    }
    assert_ne!(vectors[0], vectors[1]);
}

#[test]
fn generic_conversion_preserves_original_aac_priming_and_presentation() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "1",
            "-c:a",
            "aac",
            "-movflags",
            "+frag_keyframe+empty_moov+default_base_moof",
            "-output_ts_offset",
            "2",
        ])
        .arg(&input)
        .status()
        .unwrap();
    assert!(status.success());
    add_open_ended_audio_trim(&input, 1024);
    let output = directory.path().join("output.mp4");
    let report = converter()
        .convert_mp4(
            &input,
            &output,
            TargetProtection::Cbcs,
            &InputKeys::default(),
            Some(&OutputKey::from_hex(KID_OUT, KEY_OUT).unwrap()),
        )
        .unwrap();
    assert!(report.verification.passed);
    let source = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&input)
        .args(["-f", "hash", "-hash", "sha256", "-"])
        .output()
        .unwrap();
    assert!(source.status.success());
    let result = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-decryption_key", KEY_OUT, "-i"])
        .arg(&output)
        .args(["-f", "hash", "-hash", "sha256", "-"])
        .output()
        .unwrap();
    assert!(result.status.success());
    assert_eq!(source.stdout, result.stdout);
    let timing = |path: &Path| {
        let bytes = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "packet=pts,dts,duration",
                "-of",
                "json",
            ])
            .arg(path)
            .output()
            .unwrap()
            .stdout;
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|packet| {
                (
                    packet["pts"].as_i64(),
                    packet["dts"].as_i64(),
                    packet["duration"].as_u64(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(timing(&input), timing(&output));
}

fn add_open_ended_audio_trim(path: &Path, ticks: u64) {
    fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut bytes = u32::try_from(payload.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        bytes.extend(kind);
        bytes.extend(payload);
        bytes
    }
    fn rewrite(data: &[u8], nodes: &[drmr_core::mp4::Mp4Box], edit: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        for node in nodes {
            if node.kind() == *b"edts" {
                continue;
            }
            if matches!(&node.kind(), b"moov" | b"trak") {
                let mut children = rewrite(data, &node.children, edit);
                if node.kind() == *b"trak" {
                    children.extend(edit);
                }
                output.extend(boxed(node.kind(), &children));
            } else {
                output.extend(node.header.bytes(data));
            }
        }
        output
    }
    let mut payload = vec![1, 0, 0, 0];
    payload.extend(1u32.to_be_bytes());
    payload.extend(0u64.to_be_bytes());
    payload.extend(ticks.to_be_bytes());
    payload.extend([0, 1, 0, 0]);
    let edit = boxed(*b"edts", &boxed(*b"elst", &payload));
    let bytes = std::fs::read(path).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&bytes).unwrap();
    std::fs::write(path, rewrite(&bytes, &boxes, &edit)).unwrap();
}

#[test]
fn middle_packet_timestamp_changes_fail_even_when_payloads_and_duration_match() {
    fn boxed(kind: [u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = u32::try_from(payload.len() + 8)
            .unwrap()
            .to_be_bytes()
            .to_vec();
        out.extend(kind);
        out.extend(payload);
        out
    }
    fn rewrite(data: &[u8], nodes: &[drmr_core::mp4::Mp4Box]) -> Vec<u8> {
        let mut out = Vec::new();
        for node in nodes {
            if node.kind() == *b"stts" {
                let payload = node.header.payload(data);
                assert_eq!(u32::from_be_bytes(payload[4..8].try_into().unwrap()), 1);
                let count = u32::from_be_bytes(payload[8..12].try_into().unwrap());
                let delta = u32::from_be_bytes(payload[12..16].try_into().unwrap());
                assert!(count >= 3);
                let mut changed = vec![0, 0, 0, 0];
                changed.extend(3u32.to_be_bytes());
                for (count, delta) in [(1, delta + 1), (1, delta - 1), (count - 2, delta)] {
                    changed.extend(count.to_be_bytes());
                    changed.extend(delta.to_be_bytes());
                }
                out.extend(boxed(*b"stts", &changed));
            } else if matches!(
                &node.kind(),
                b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl"
            ) {
                out.extend(boxed(node.kind(), &rewrite(data, &node.children)));
            } else {
                out.extend(node.header.bytes(data));
            }
        }
        out
    }
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    video(&source, "testsrc2=size=64x64:rate=5");
    let result = directory.path().join("changed.mp4");
    let bytes = std::fs::read(&source).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&bytes).unwrap();
    std::fs::write(&result, rewrite(&bytes, &boxes)).unwrap();
    let report = converter()
        .verify_mp4(
            &source,
            &InputKeys::default(),
            &result,
            &InputKeys::default(),
        )
        .unwrap();
    assert!(report.checks.packet_payloads_equal);
    assert!(report.checks.decoded_content_equal);
    assert!(!report.checks.packet_timing_equal);
    assert!(!report.passed);
}

#[test]
fn fragmented_b_frame_input_keeps_its_original_presentation_timeline() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=s=64x64:r=24:d=1",
            "-an",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+frag_keyframe+empty_moov",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    let output = directory.path().join("output.mp4");
    let report = converter()
        .convert_mp4(
            &source,
            &output,
            TargetProtection::Cbcs,
            &InputKeys::default(),
            Some(&OutputKey::from_hex(KID_OUT, KEY_OUT).unwrap()),
        )
        .unwrap();
    assert!(report.verification.passed);
}

#[test]
fn progressive_aac_with_finite_tail_padding_fails_before_publication() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "sine=frequency=440:sample_rate=48000",
            "-t",
            "1",
            "-c:a",
            "aac",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    let output = directory.path().join("output.mp4");
    let failure = converter()
        .convert_mp4(
            &source,
            &output,
            TargetProtection::Cbcs,
            &InputKeys::default(),
            Some(&OutputKey::from_hex(KID_OUT, KEY_OUT).unwrap()),
        )
        .unwrap_err();
    assert_eq!(failure, ConversionError("finite-audio-tail-not-supported"));
    assert!(!output.exists());
}

#[test]
fn a_changed_audio_sample_clock_cannot_pass_with_identical_silent_samples() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    let result = directory.path().join("result.mp4");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=stereo:sample_rate=48000",
            "-t",
            "0.2",
            "-c:a",
            "aac",
            "-use_editlist",
            "0",
            "-movflags",
            "+frag_keyframe+empty_moov+default_base_moof",
        ])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    let mut data = std::fs::read(&source).unwrap();
    let esds = data.windows(4).position(|bytes| bytes == b"esds").unwrap();
    let asc = data[esds..esds + 100]
        .windows(2)
        .position(|bytes| bytes == [0x11, 0x90])
        .unwrap()
        + esds;
    // Only the AAC decoder configuration changes, from 48 kHz to 44.1 kHz.
    // The encoded packets, silent sample bytes, and MP4 clock remain identical.
    data[asc..asc + 2].copy_from_slice(&[0x12, 0x10]);
    std::fs::write(&result, data).unwrap();
    let report = converter()
        .verify_mp4(
            &source,
            &InputKeys::default(),
            &result,
            &InputKeys::default(),
        )
        .unwrap();
    assert!(
        !report.passed,
        "a different decoded sample clock changes presentation semantics"
    );
    assert!(!report.checks.stream_metadata.is_pass());
    assert!(
        report.checks.packet_payloads_equal
            && report.checks.packet_timing_equal
            && report.checks.decoded_content_equal
    );
}

#[test]
fn in_band_events_after_media_are_refused_before_publication() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source.mp4");
    let output = directory.path().join("output.mp4");
    video(&source, "testsrc2=size=64x64:rate=5");
    let mut event = vec![0, 0, 0, 0];
    event.extend(b"urn:example:event\0\0");
    for value in [1000_u32, 100, 100, 1] {
        event.extend(value.to_be_bytes());
    }
    event.extend(b"public fixture event");
    let mut bytes = std::fs::read(&source).unwrap();
    bytes.extend(u32::try_from(event.len() + 8).unwrap().to_be_bytes());
    bytes.extend(b"emsg");
    bytes.extend(event);
    std::fs::write(&source, bytes).unwrap();
    assert_eq!(
        converter()
            .convert_mp4(
                &source,
                &output,
                TargetProtection::Clear,
                &InputKeys::default(),
                None
            )
            .unwrap_err()
            .0,
        "inband-events-not-supported"
    );
    assert!(!output.exists());
}
