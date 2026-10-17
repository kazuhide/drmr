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
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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
#[cfg(unix)]
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

#[cfg(unix)]
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
#[cfg(unix)]
#[test]
fn cbcs_output_uses_fresh_random_initialization_vectors_and_private_permissions() {
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
        assert_eq!(
            std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let bytes = std::fs::read(&output).unwrap();
        let entries = drmr_core::mp4::sample_entries(&bytes).unwrap();
        let tenc = entries[0].tenc.as_ref().unwrap();
        vectors.push(tenc.default_constant_iv.as_ref().unwrap().to_vec());
    }
    assert_ne!(vectors[0], vectors[1]);
}
