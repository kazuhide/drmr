//! Decoder input keys follow the actual container while native metadata keeps every KID.
mod common;
use drmr_core::{
    conversion::{
        Aes128Key, ContentKey, ConversionRequest, Converter, DecodeEvidence, HlsInputKeys,
        HlsOutputKey, InputKeys, SourceValidationPurpose, VerificationReport, VerificationRequest,
        VerificationSource,
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

const URI: &str = "https://operator.invalid/decoder-fixture";
const AES_KEY: &str = "42424242424242424242424242424242";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}
fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(NoSink),
        )),
        "packager",
        "ffmpeg",
    )
    .with_decode(true)
}
fn keys(include_audio: bool) -> InputKeys {
    let mut keys = vec![
        ContentKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap(),
    ];
    if include_audio {
        keys.push(
            ContentKey::from_hex(
                "303132333435363738393a3b3c3d3e3f",
                "202122232425262728292a2b2c2d2e2f",
            )
            .unwrap(),
        );
    }
    InputKeys::new(keys).unwrap()
}
fn proof(report: &VerificationReport) {
    assert!(report.passed);
    assert!(report.checks.is_pass());
    assert_eq!(
        report.decode_evidence,
        DecodeEvidence::ContentCompared { equal: true }
    );
}

#[test]
fn aes_hls_decoding_ignores_unrelated_mp4_keys_in_source_admission_and_both_directions() {
    let root = common::private_tempdir().unwrap();
    let source = root.path().join("video.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture("clip.mp4"))
        .args(["-t", "2", "-map", "0:v:0", "-an", "-c:v", "copy"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(made.status.success());
    let empty = InputKeys::default();
    let unrelated = keys(true);
    let no_hls = HlsInputKeys::default();
    let hls = HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(AES_KEY).unwrap())]).unwrap();
    let output = root.path().join("encrypted");
    let key = HlsOutputKey::new(URI, Aes128Key::from_hex(AES_KEY).unwrap()).unwrap();
    let converter = converter();
    proof(
        &converter
            .convert(ConversionRequest {
                input: &source,
                output: &output,
                format: OutputFormat::Hls,
                target: TargetProtection::HlsAes128,
                input_keys: &empty,
                hls_input_keys: &no_hls,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: Some(&key),
            })
            .unwrap()
            .verification,
    );
    let manifest = output.join("master.m3u8");
    let before = common::snapshot_tree(root.path());
    let clear = VerificationSource {
        input: &source,
        input_keys: &empty,
        hls_keys: &no_hls,
    };
    let protected = VerificationSource {
        input: &manifest,
        input_keys: &unrelated,
        hls_keys: &hls,
    };
    let admitted = converter
        .validate_source(protected, SourceValidationPurpose::Verification)
        .unwrap();
    assert!(admitted.verification.passed);
    assert_eq!(
        admitted.verification.decode_evidence,
        DecodeEvidence::SourceDecoded
    );
    for (source, output) in [(clear, protected), (protected, clear)] {
        proof(
            &converter
                .verify(VerificationRequest { source, output })
                .unwrap(),
        );
    }
    assert_eq!(common::snapshot_tree(root.path()), before);
}

#[test]
fn raw_muxed_mp4_decoding_retains_both_kids_and_finite_audio_in_both_directions() {
    let converter = converter();
    let all = keys(true);
    let missing = keys(false);
    let empty = InputKeys::default();
    let no_hls = HlsInputKeys::default();
    let clear_file = fixture("clip.mp4");
    for name in ["cenc-iv8.mp4", "cenc-iv16.mp4"] {
        let protected_file = fixture(name);
        let before = std::fs::read(&protected_file).unwrap();
        let clear = VerificationSource {
            input: &clear_file,
            input_keys: &empty,
            hls_keys: &no_hls,
        };
        let protected = VerificationSource {
            input: &protected_file,
            input_keys: &all,
            hls_keys: &no_hls,
        };
        for (source, output) in [(clear, protected), (protected, clear)] {
            proof(
                &converter
                    .verify(VerificationRequest { source, output })
                    .unwrap(),
            );
        }
        let incomplete = VerificationSource {
            input_keys: &missing,
            ..protected
        };
        for (source, output) in [(clear, incomplete), (incomplete, clear)] {
            assert_eq!(
                converter
                    .verify(VerificationRequest { source, output })
                    .unwrap_err()
                    .0,
                "input-key-required"
            );
        }
        assert_eq!(std::fs::read(protected_file).unwrap(), before);
    }
}
