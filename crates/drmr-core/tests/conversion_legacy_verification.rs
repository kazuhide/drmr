//! Verification preserves its existing scope independently of conversion admission.
mod common;
use drmr_core::{
    conversion::{
        ContentKey, Converter, HlsInputKeys, InputKeys, SourceValidationPurpose,
        VerificationRequest, VerificationSource,
    },
    events::Collector,
    toolrun::{Secrets, ToolRunner},
};
use sha2::Digest as _;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
const ID: &str = "303132333435363738393a3b3c3d3e3f";
const KEY: &str = "202122232425262728292a2b2c2d2e2f";
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}
fn keys() -> InputKeys {
    InputKeys::new(vec![ContentKey::from_hex(ID, KEY).unwrap()]).unwrap()
}
fn converter() -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(runner, "unused-packager", "ffmpeg").with_decode(true),
        events,
    )
}
fn audio(root: &Path) -> PathBuf {
    let output = root.join("audio-clear.mp4");
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-copyts", "-i"])
        .arg(fixture("clip.mp4"))
        .args([
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
            "-use_editlist",
            "1",
        ])
        .arg(&output)
        .output()
        .unwrap();
    assert!(status.status.success());
    output
}
fn cenc(source: &Path, output: &Path) {
    let status = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-copyts", "-i"])
        .arg(source)
        .args([
            "-map",
            "0",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "disabled",
            "-use_editlist",
            "1",
            "-fflags",
            "+bitexact",
            "-encryption_scheme",
            "cenc-aes-ctr",
            "-encryption_kid",
            ID,
            "-encryption_key",
            KEY,
        ])
        .arg(output)
        .output()
        .unwrap();
    assert!(status.status.success());
    // Append one well-formed version-zero PSSH. Sample/ciphertext offsets remain unchanged.
    let mut bytes = std::fs::read(output).unwrap();
    bytes.extend(32u32.to_be_bytes());
    bytes.extend(b"pssh");
    bytes.extend([0; 4]);
    bytes.extend([0x55; 16]);
    bytes.extend(0u32.to_be_bytes());
    std::fs::write(output, bytes).unwrap();
}
fn verification(c: &Converter, source: &Path, result: &Path, result_keys: &InputKeys) {
    let keys = keys();
    let before = std::fs::read(source).unwrap();
    let candidate = std::fs::read(result).unwrap();
    let direct = c.verify_mp4(source, &keys, result, result_keys).unwrap();
    assert!(
        direct.passed
            && (direct.checks.is_pass()
                && direct.decode_evidence.satisfies(
                    drmr_core::conversion::ValidationDepth::Decoded,
                    direct.scope
                ))
    );
    assert!(!direct.license_playback_verified);
    assert!(direct.checks.stream_metadata.is_pass());
    assert!(direct.checks.packet_payloads_equal);
    assert!(direct.checks.packet_timing_equal);
    assert!(direct.decode_evidence.satisfies(
        drmr_core::conversion::ValidationDepth::Decoded,
        direct.scope
    ));
    let generic = c
        .verify(VerificationRequest {
            source: VerificationSource {
                input: source,
                input_keys: &keys,
                hls_keys: &HlsInputKeys::default(),
            },
            output: VerificationSource {
                input: result,
                input_keys: result_keys,
                hls_keys: &HlsInputKeys::default(),
            },
        })
        .unwrap();
    assert!(
        generic.passed
            && (generic.checks.is_pass()
                && generic.decode_evidence.satisfies(
                    drmr_core::conversion::ValidationDepth::Decoded,
                    generic.scope
                ))
    );
    assert!(!generic.license_playback_verified);
    assert!(generic.checks.stream_metadata.is_pass());
    assert!(generic.checks.packet_payloads_equal);
    assert!(generic.checks.packet_timing_equal);
    assert!(generic.decode_evidence.satisfies(
        drmr_core::conversion::ValidationDepth::Decoded,
        generic.scope
    ));
    assert_eq!(std::fs::read(source).unwrap(), before);
    assert_eq!(std::fs::read(result).unwrap(), candidate);
}
#[test]
fn legacy_single_track_cenc_with_pssh_and_cbcs_keep_both_verification_interfaces() {
    let root = common::private_tempdir().unwrap();
    let clear = audio(root.path());
    let encrypted = root.path().join("cenc-pssh.mp4");
    cenc(&clear, &encrypted);
    let (c, _) = converter();
    assert_eq!(
        hex::encode(sha2::Sha256::digest(
            std::fs::read(fixture("cbcs-audio.mp4")).unwrap()
        )),
        "0868265d3c884bbf87a60cea8bb3de12df387393ffb33afa1652a089a95cea1d"
    );
    for source in [&encrypted, &fixture("cbcs-audio.mp4")] {
        verification(&c, source, source, &keys());
        verification(&c, source, &clear, &InputKeys::default());
    }
}
#[test]
fn legacy_joint_cenc_with_one_shared_kid_and_pssh_keeps_generic_verification() {
    let root = common::private_tempdir().unwrap();
    let source = root.path().join("joint-pssh.mp4");
    cenc(&fixture("clip.mp4"), &source);
    let (c, _) = converter();
    let keys = keys();
    let before = std::fs::read(&source).unwrap();
    for result in [source.as_path(), fixture("clip.mp4").as_path()] {
        let empty = InputKeys::default();
        let result_keys = if result == source { &keys } else { &empty };
        let report = c
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &source,
                    input_keys: &keys,
                    hls_keys: &HlsInputKeys::default(),
                },
                output: VerificationSource {
                    input: result,
                    input_keys: result_keys,
                    hls_keys: &HlsInputKeys::default(),
                },
            })
            .unwrap();
        assert!(
            report.passed
                && (report.checks.is_pass()
                    && report.decode_evidence.satisfies(
                        drmr_core::conversion::ValidationDepth::Decoded,
                        report.scope
                    ))
        );
        assert!(!report.license_playback_verified);
    }
    assert_eq!(std::fs::read(source).unwrap(), before);
}

fn pssh(source: &Path, output: &Path) {
    let mut bytes = std::fs::read(source).unwrap();
    bytes.extend(32u32.to_be_bytes());
    bytes.extend(b"pssh");
    bytes.extend([0; 4]);
    bytes.extend([0x55; 16]);
    bytes.extend(0u32.to_be_bytes());
    std::fs::write(output, bytes).unwrap();
}
#[test]
fn legacy_distinct_key_reference_and_clear_candidate_keep_existing_pssh_metadata() {
    let root = common::private_tempdir().unwrap();
    let original = fixture("clip.mp4");
    let encrypted = fixture("cenc-iv8.mp4");
    let protected_pssh = root.path().join("protected-pssh.mp4");
    let clear_pssh = root.path().join("clear-pssh.mp4");
    pssh(&encrypted, &protected_pssh);
    pssh(&original, &clear_pssh);
    let (c, _) = converter();
    let input_keys = InputKeys::new(vec![
        ContentKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap(),
        ContentKey::from_hex(ID, KEY).unwrap(),
    ])
    .unwrap();
    for (source, result) in [
        (&protected_pssh, &original),
        (&encrypted, &clear_pssh),
        (&protected_pssh, &clear_pssh),
    ] {
        let before = std::fs::read(source).unwrap();
        let candidate = std::fs::read(result).unwrap();
        let report = c
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: source,
                    input_keys: &input_keys,
                    hls_keys: &HlsInputKeys::default(),
                },
                output: VerificationSource {
                    input: result,
                    input_keys: &InputKeys::default(),
                    hls_keys: &HlsInputKeys::default(),
                },
            })
            .unwrap();
        assert!(
            report.passed
                && (report.checks.is_pass()
                    && report.decode_evidence.satisfies(
                        drmr_core::conversion::ValidationDepth::Decoded,
                        report.scope
                    ))
        );
        assert!(!report.license_playback_verified);
        assert_eq!(std::fs::read(source).unwrap(), before);
        assert_eq!(std::fs::read(result).unwrap(), candidate);
    }
}
#[test]
fn conversion_admission_still_refuses_legacy_verifiable_protection_before_tools() {
    use drmr_core::{
        conversion::ConversionRequest,
        inspection::{OutputFormat, TargetProtection},
    };
    let root = common::private_tempdir().unwrap();
    let audio = audio(root.path());
    let source = root.path().join("cenc-pssh.mp4");
    cenc(&audio, &source);
    let joint = root.path().join("joint-pssh.mp4");
    cenc(&fixture("clip.mp4"), &joint);
    let cbcs = root.path().join("cbcs-pssh.mp4");
    pssh(&fixture("cbcs-audio.mp4"), &cbcs);
    for (index, (source, code)) in [
        (&source, "progressive-cenc-profile-not-supported"),
        (&joint, "progressive-cenc-profile-not-supported"),
        (&cbcs, "progressive-cbcs-profile-not-supported"),
    ]
    .into_iter()
    .enumerate()
    {
        let (c, events) = converter();
        let output = root.path().join(format!("refused-{index}.mp4"));
        let before = std::fs::read(source).unwrap();
        let keys = keys();
        assert_eq!(
            c.convert_mp4(source, &output, TargetProtection::Clear, &keys, None)
                .unwrap_err()
                .0,
            code
        );
        assert_eq!(
            c.convert(ConversionRequest {
                input: source,
                output: &output,
                format: OutputFormat::Mp4,
                target: TargetProtection::Clear,
                input_keys: &keys,
                hls_input_keys: &HlsInputKeys::default(),
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None
            })
            .unwrap_err()
            .0,
            code
        );
        assert!(!output.exists());
        assert_eq!(std::fs::read(source).unwrap(), before);
        assert_eq!(events.records().len(), 0);
    }
}

#[test]
fn joint_mp4_verification_uses_keys_without_conversion_admission() {
    let root = common::private_tempdir().unwrap();
    let source = root.path().join("source-pssh.mp4");
    let output = root.path().join("clear-pssh.mp4");
    pssh(&fixture("cenc-iv8.mp4"), &source);
    pssh(&fixture("clip.mp4"), &output);
    let input_keys = InputKeys::new(vec![
        ContentKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap(),
        ContentKey::from_hex(ID, KEY).unwrap(),
    ])
    .unwrap();
    let before = std::fs::read(&source).unwrap();
    let result = std::fs::read(&output).unwrap();
    let (c, _) = converter();
    let report = c
        .verify_mp4(&source, &input_keys, &output, &InputKeys::default())
        .unwrap();
    assert!(
        report.passed
            && (report.checks.is_pass()
                && report.decode_evidence.satisfies(
                    drmr_core::conversion::ValidationDepth::Decoded,
                    report.scope
                ))
    );
    assert!(!report.license_playback_verified);
    assert_eq!(std::fs::read(source).unwrap(), before);
    assert_eq!(std::fs::read(output).unwrap(), result);
}

#[test]
fn qualified_cbcs_audio_converts_but_still_requires_its_original_key() {
    use drmr_core::{
        conversion::ConversionRequest,
        inspection::{OutputFormat, TargetProtection},
    };
    let root = common::private_tempdir().unwrap();
    let source = fixture("cbcs-audio.mp4");
    let before = std::fs::read(&source).unwrap();
    let keys = keys();
    for generic in [false, true] {
        let (c, events) = converter();
        let output = root.path().join(format!("clear-{generic}.mp4"));
        let report = if generic {
            c.convert(ConversionRequest {
                input: &source,
                output: &output,
                format: OutputFormat::Mp4,
                target: TargetProtection::Clear,
                input_keys: &keys,
                hls_input_keys: &HlsInputKeys::default(),
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
        } else {
            c.convert_mp4(&source, &output, TargetProtection::Clear, &keys, None)
        }
        .unwrap();
        let proof = report.verification;
        assert!(
            proof.passed
                && (proof.checks.is_pass()
                    && proof
                        .decode_evidence
                        .satisfies(drmr_core::conversion::ValidationDepth::Decoded, proof.scope))
        );
        assert!(proof.checks.stream_metadata.is_pass());
        assert!(proof.checks.packet_payloads_equal);
        assert!(proof.checks.packet_timing_equal);
        assert!(
            proof
                .decode_evidence
                .satisfies(drmr_core::conversion::ValidationDepth::Decoded, proof.scope)
        );
        assert!(!proof.license_playback_verified);
        verification(&c, &source, &output, &InputKeys::default());
        assert!(events.records().iter().any(|record| {
            record.get("stage").and_then(serde_json::Value::as_str) == Some("tool")
                && record.get("event").and_then(serde_json::Value::as_str) == Some("command")
        }));
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }
    let (c, events) = converter();
    let output = root.path().join("missing-key.mp4");
    assert_eq!(
        c.convert_mp4(
            &source,
            &output,
            TargetProtection::Clear,
            &InputKeys::default(),
            None
        )
        .unwrap_err()
        .0,
        "input-key-required"
    );
    assert_eq!(
        c.convert(ConversionRequest {
            input: &source,
            output: &output,
            format: OutputFormat::Mp4,
            target: TargetProtection::Clear,
            input_keys: &InputKeys::default(),
            hls_input_keys: &HlsInputKeys::default(),
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap_err()
        .0,
        "input-key-required"
    );
    assert!(!output.exists());
    assert_eq!(events.records().len(), 0);
    assert_eq!(std::fs::read(source).unwrap(), before);
}

#[test]
fn deep_admission_keeps_conversion_and_verification_pssh_policies_distinct() {
    let root = tempfile::tempdir().unwrap();
    let clear = audio(root.path());
    let protected = root.path().join("signaled.mp4");
    cenc(&clear, &protected);
    let original = std::fs::read(&protected).unwrap();
    let keys = keys();
    let hls = HlsInputKeys::default();
    let source = VerificationSource {
        input: &protected,
        input_keys: &keys,
        hls_keys: &hls,
    };
    let (c, _) = converter();
    let result = c
        .validate_source(source, SourceValidationPurpose::Verification)
        .unwrap();
    assert!(result.verification.passed);
    assert!(
        c.validate_source(source, SourceValidationPurpose::Conversion)
            .is_err()
    );
    assert_eq!(std::fs::read(&protected).unwrap(), original);
}
