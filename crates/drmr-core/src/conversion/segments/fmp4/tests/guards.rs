use super::*;
use std::io::Write as _;

#[test]
fn wrong_keys_and_foreign_destinations_do_not_publish_or_overwrite() {
    let fixture = fixture(48_000);
    let input = manifest_fixture(
        &fixture,
        TargetProtection::Cenc,
        OutputFormat::Dash,
        "encrypted",
    );
    let hls = HlsInputKeys::default();
    let (converter, events) = converter();
    let output = fixture.directory.path().join("must-not-exist");
    for keys in [
        InputKeys::default(),
        InputKeys::new(vec![
            crate::conversion::ContentKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap(),
        ])
        .unwrap(),
    ] {
        let request = ConversionRequest {
            input: &input,
            output: &output,
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &keys,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        };
        assert!(convert_dash(&converter, request).is_err());
        assert!(!output.exists());
    }
    let keys = InputKeys::new(vec![
        crate::conversion::ContentKey::from_hex(SOURCE_ID, SOURCE_KEY).unwrap(),
    ])
    .unwrap();
    std::fs::write(&output, b"foreign output").unwrap();
    let request = ConversionRequest {
        input: &input,
        output: &output,
        format: OutputFormat::Dash,
        target: TargetProtection::Clear,
        input_keys: &keys,
        hls_input_keys: &hls,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    };
    assert!(convert_dash(&converter, request).is_err());
    assert_eq!(std::fs::read(output).unwrap(), b"foreign output");
    assert_no_tools(&events);
}

#[test]
fn unauthenticated_cipher_keys_need_an_independent_reference_or_optional_decode_evidence() {
    let mut fixture = fixture(48_000);
    // AAC configuration and sample framing do not authenticate encrypted entropy data.
    // Wrong AVC plaintext can instead trip syntax checks, depending on the source IV.
    fixture.parts.retain(|(init, _, _)| {
        crate::inspection::inspect(init).unwrap().tracks[0].kind == TrackKind::Audio
    });
    assert_eq!(fixture.parts.len(), 1);
    let input = manifest_fixture(
        &fixture,
        TargetProtection::Cenc,
        OutputFormat::Dash,
        "source",
    );
    let keys = InputKeys::new(vec![
        crate::conversion::ContentKey::from_hex(SOURCE_ID, OUTPUT_KEY).unwrap(),
    ])
    .unwrap();
    let hls = HlsInputKeys::default();
    let output = fixture.directory.path().join("wrong-key-native");
    let request = ConversionRequest {
        input: &input,
        output: &output,
        format: OutputFormat::Dash,
        target: TargetProtection::Clear,
        input_keys: &keys,
        hls_input_keys: &hls,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    };
    let (converter, events) = converter();
    let native = convert_dash(&converter, request).unwrap();
    assert!(native.verification.passed);
    assert_eq!(
        native.verification.decode_evidence,
        crate::conversion::DecodeEvidence::not_requested()
    );
    assert_no_tools(&events);
    let correct_keys = InputKeys::new(vec![
        crate::conversion::ContentKey::from_hex(SOURCE_ID, SOURCE_KEY).unwrap(),
    ])
    .unwrap();
    assert!(
        verify(
            &converter,
            VerificationRequest {
                source: VerificationSource {
                    input: &input,
                    input_keys: &correct_keys,
                    hls_keys: &hls
                },
                output: VerificationSource {
                    input: &output.join("manifest.mpd"),
                    input_keys: &InputKeys::default(),
                    hls_keys: &hls
                },
            }
        )
        .is_err(),
        "an independently keyed reference must reject the wrong plaintext"
    );
    let decoded_output = fixture.directory.path().join("wrong-key-decoded");
    // Complete decoding rejects this fixture; it is not a cryptographic key-authentication claim.
    let converter = Converter::new(
        converter.backend.runner.clone(),
        "must-not-run-packager",
        "ffmpeg",
    )
    .with_decode(true);
    assert!(
        convert_dash(
            &converter,
            ConversionRequest {
                output: &decoded_output,
                ..request
            }
        )
        .is_err()
    );
    assert!(!decoded_output.exists());
}

#[test]
fn optional_decode_adds_complete_evidence_before_publication() {
    let fixture = fixture(48_000);
    let input = manifest_fixture(
        &fixture,
        TargetProtection::Clear,
        OutputFormat::Hls,
        "source",
    );
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let (converter, events) = converter();
    let converter = Converter::new(
        converter.backend.runner.clone(),
        "must-not-run-packager",
        "ffmpeg",
    )
    .with_decode(true);
    let output = fixture.directory.path().join("decoded-output");
    let key = OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap();
    let source = super::super::super::source::HlsSource::load(
        &converter,
        VerificationSource {
            input: &input,
            input_keys: &empty,
            hls_keys: &hls,
        },
    )
    .unwrap();
    let qualified = qualify_hls(
        &converter,
        &source,
        VerificationSource {
            input: &input,
            input_keys: &empty,
            hls_keys: &hls,
        },
        SourceValidationPurpose::Conversion,
    )
    .unwrap();
    assert_eq!(
        qualified.verification.decode_evidence,
        crate::conversion::DecodeEvidence::SourceDecoded
    );
    let report = convert_hls(
        &converter,
        ConversionRequest {
            input: &input,
            output: &output,
            format: OutputFormat::Dash,
            target: TargetProtection::Cbcs,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: Some(&key),
            hls_key_uri: None,
            hls_output_key: None,
        },
        source,
    )
    .unwrap();
    assert!(report.verification.passed);
    assert_eq!(
        report.verification.required_depth,
        crate::conversion::ValidationDepth::Decoded
    );
    assert_eq!(
        report.verification.decode_evidence,
        crate::conversion::DecodeEvidence::ContentCompared { equal: true }
    );
    assert_eq!(
        report.processing.unwrap().required_tools,
        vec![
            crate::inspection::RequiredTool::Ffmpeg,
            crate::inspection::RequiredTool::Ffprobe
        ]
    );
    assert!(
        events
            .records()
            .iter()
            .any(|record| record.get("stage").and_then(serde_json::Value::as_str) == Some("tool"))
    );
}

#[test]
fn regrouping_the_same_samples_is_not_boundary_preservation() {
    let mut fixture = fixture(48_000);
    let input = manifest_fixture(
        &fixture,
        TargetProtection::Clear,
        OutputFormat::Dash,
        "original",
    );
    let first = fixture.directory.path().join("merged.m4s");
    std::fs::write(
        &first,
        [
            std::fs::read(&fixture.parts[0].1[0]).unwrap(),
            std::fs::read(&fixture.parts[0].1[1]).unwrap(),
        ]
        .concat(),
    )
    .unwrap();
    fixture.parts[0].1 = vec![first, fixture.parts[0].1[2].clone()];
    let output = manifest_fixture(
        &fixture,
        TargetProtection::Clear,
        OutputFormat::Dash,
        "regrouped",
    );
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let (converter, events) = converter();
    let report = verify(
        &converter,
        VerificationRequest {
            source: VerificationSource {
                input: &input,
                input_keys: &empty,
                hls_keys: &hls,
            },
            output: VerificationSource {
                input: &output,
                input_keys: &empty,
                hls_keys: &hls,
            },
        },
    )
    .unwrap();
    assert!(!report.passed);
    assert!(report.checks.stream_metadata.is_pass());
    assert!(report.checks.packet_payloads_equal);
    assert!(!report.checks.packet_timing_equal);
    assert_no_tools(&events);
}

#[test]
fn a_changed_original_is_rejected_before_final_publication() {
    let fixture = fixture(48_000);
    let input = manifest_fixture(
        &fixture,
        TargetProtection::Clear,
        OutputFormat::Hls,
        "source",
    );
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let (converter, events) = converter();
    let source = super::super::super::source::HlsSource::load(
        &converter,
        VerificationSource {
            input: &input,
            input_keys: &empty,
            hls_keys: &hls,
        },
    )
    .unwrap();
    let path = input.parent().unwrap().join("track-0000/seg_1.m4s");
    std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .unwrap()
        .write_all(&[0, 0, 0, 8, b'f', b'r', b'e', b'e'])
        .unwrap();
    let output = fixture.directory.path().join("changed-output");
    let request = ConversionRequest {
        input: &input,
        output: &output,
        format: OutputFormat::Dash,
        target: TargetProtection::Clear,
        input_keys: &empty,
        hls_input_keys: &hls,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    };
    assert_eq!(
        convert_hls(&converter, request, source).err(),
        Some(crate::conversion::ConversionError("planned-source-changed"))
    );
    assert!(!output.exists());
    assert_no_tools(&events);
}

#[test]
fn cancellation_during_resource_rewriting_leaves_no_partial_output() {
    struct CancelAfterResource {
        stop: crate::concurrency::Stop,
        calls: std::sync::atomic::AtomicUsize,
    }
    impl crate::events::Sink for CancelAfterResource {
        fn emit(&self, record: &crate::events::Record) {
            if record.get("event").and_then(serde_json::Value::as_str)
                == Some("work-unit-completed")
                && self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 2
            {
                self.stop.set();
            }
        }
    }
    let fixture = fixture(48_000);
    let input = manifest_fixture(
        &fixture,
        TargetProtection::Clear,
        OutputFormat::Dash,
        "source",
    );
    let (converter, events) = converter();
    let stop = crate::concurrency::Stop::new();
    let converter = converter
        .with_stop(stop.clone())
        .with_max_workers(Some(1))
        .unwrap()
        .with_activity_sink(Arc::new(CancelAfterResource {
            stop,
            calls: std::sync::atomic::AtomicUsize::new(0),
        }));
    let output = fixture.directory.path().join("cancelled-output");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let request = ConversionRequest {
        input: &input,
        output: &output,
        format: OutputFormat::Dash,
        target: TargetProtection::Clear,
        input_keys: &empty,
        hls_input_keys: &hls,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    };
    assert_eq!(
        convert_dash(&converter, request).err(),
        Some(crate::conversion::ConversionError("cancelled"))
    );
    assert!(!output.exists());
    assert!(
        !std::fs::read_dir(fixture.directory.path())
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".drmr-convert-"))
    );
    assert_no_tools(&events);
}
