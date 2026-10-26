//! Final addressing forms remain ordinary independent native inputs, including finite AAC.
use super::*;
use crate::conversion::{
    ContentKey, OutputSignaling, dash_addressing::DashAddressing, key_file::OperatorKeys,
};

fn provider_signaling() -> OutputSignaling {
    let mut payload = vec![0x12, 0x10];
    payload.extend(hex::decode(OUTPUT_ID).unwrap());
    let pssh =
        crate::playready::build_pssh_box(&crate::playready::WIDEVINE_SYSTEM_ID, &payload).unwrap();
    OutputSignaling::multi_drm(
        Some(&hex::encode(pssh)),
        Some("https://example.invalid/license"),
        false,
    )
    .unwrap()
}

fn operator_keys(target: TargetProtection) -> OperatorKeys {
    let mut keys = OperatorKeys::default();
    keys.output = (target != TargetProtection::Clear)
        .then(|| OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap());
    keys.signaling = (target == TargetProtection::Cbcs).then(provider_signaling);
    keys
}

fn regular_fixture(rate: u32) -> Fixture {
    let fixture = fixture(rate);
    for (index, (_, resources, complete)) in fixture.parts.iter().enumerate() {
        let (_, fragments) =
            crate::mp4::split_fragmented(&std::fs::read(complete).unwrap()).unwrap();
        // Existing video keyframes at 3.5 and 7 seconds form a regular cadence.
        // AAC cuts stay on complete frames near those same two boundaries.
        let cuts = if index == 0 {
            [0, 84, 168, fragments.len()]
        } else {
            [
                0,
                rate as usize * 7 / 2048,
                rate as usize * 7 / 1024,
                fragments.len(),
            ]
        };
        assert_eq!(resources.len(), cuts.len() - 1);
        for (resource, bounds) in resources.iter().zip(cuts.windows(2)) {
            std::fs::write(resource, fragments[bounds[0]..bounds[1]].concat()).unwrap();
        }
    }
    fixture
}

fn source_manifest(fixture: &Fixture) -> PathBuf {
    manifest_fixture(
        fixture,
        TargetProtection::Clear,
        OutputFormat::Hls,
        "source",
    )
}

fn decoded_compare(
    command: &Converter,
    source: VerificationSource<'_>,
    output: VerificationSource<'_>,
) {
    let decoded = Converter::new(
        command.backend.runner.clone(),
        "must-not-run-packager",
        "ffmpeg",
    )
    .with_processing(ProcessingMode::Segments)
    .with_decode(true);
    let report = decoded
        .verify(VerificationRequest { source, output })
        .unwrap();
    assert!(report.passed);
    assert_eq!(
        report.decode_evidence,
        crate::conversion::DecodeEvidence::ContentCompared { equal: true }
    );
}

fn convert_with_addressing(
    command: &Converter,
    source: VerificationSource<'_>,
    output: &Path,
    target: TargetProtection,
    mode: DashAddressing,
    keys: &OperatorKeys,
) -> Result<(), ConversionError> {
    let selected = command
        .with_processing_clone(ProcessingMode::Segments)
        .with_dash_addressing(mode);
    let report = selected.convert(ConversionRequest {
        input: source.input,
        output,
        format: OutputFormat::Dash,
        target,
        input_keys: source.input_keys,
        hls_input_keys: source.hls_keys,
        output_key: keys.output.as_ref(),
        hls_key_uri: None,
        hls_output_key: None,
    })?;
    assert!(report.verification.passed);
    crate::conversion::dash::verify_output_addressing(output, mode, &command.backend.stop)
}

#[test]
fn selected_addressing_keeps_finite_audio_and_drm_in_fresh_native_verification() {
    let mut incomplete = Vec::new();
    for rate in [44_100, 48_000] {
        let unequal = fixture(rate);
        let unequal_input = source_manifest(&unequal);
        let regular = regular_fixture(rate);
        let regular_input = source_manifest(&regular);
        let empty = InputKeys::default();
        let hls = HlsInputKeys::default();
        let output_keys =
            InputKeys::new(vec![ContentKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap()]).unwrap();
        for mode in [
            DashAddressing::TemplateDuration,
            DashAddressing::List,
            DashAddressing::Base,
        ] {
            let (fixture, input) = if mode == DashAddressing::TemplateDuration {
                (&regular, &regular_input)
            } else {
                (&unequal, &unequal_input)
            };
            for target in [
                TargetProtection::Clear,
                TargetProtection::Cenc,
                TargetProtection::Cbcs,
            ] {
                let (command, events) = converter();
                let command = command.with_processing(ProcessingMode::Segments);
                let keys = operator_keys(target);
                let command = command.with_output_signaling(keys.signaling.clone());
                let output = fixture
                    .directory
                    .path()
                    .join(format!("{mode:?}-{target:?}"));
                convert_with_addressing(
                    &command,
                    VerificationSource {
                        input,
                        input_keys: &empty,
                        hls_keys: &hls,
                    },
                    &output,
                    target,
                    mode,
                    &keys,
                )
                .unwrap();
                let manifest = output.join("manifest.mpd");
                command
                    .verify_output_signaling(&manifest, OutputFormat::Dash, target, &keys)
                    .unwrap();
                let report = command
                    .verify(VerificationRequest {
                        source: VerificationSource {
                            input,
                            input_keys: &empty,
                            hls_keys: &hls,
                        },
                        output: VerificationSource {
                            input: &manifest,
                            input_keys: if target == TargetProtection::Clear {
                                &empty
                            } else {
                                &output_keys
                            },
                            hls_keys: &hls,
                        },
                    })
                    .unwrap();
                assert!(report.checks.packet_payloads_equal && report.checks.packet_timing_equal);
                if !report.passed {
                    incomplete.push(format!("{rate}/{mode:?}/{target:?}"));
                }
                assert_no_tools(&events);
                if rate == 44_100 && target == TargetProtection::Cbcs {
                    decoded_compare(
                        &command,
                        VerificationSource {
                            input,
                            input_keys: &empty,
                            hls_keys: &hls,
                        },
                        VerificationSource {
                            input: &manifest,
                            input_keys: &output_keys,
                            hls_keys: &hls,
                        },
                    );
                }
            }
        }
    }
    assert!(
        incomplete.is_empty(),
        "native manifest checks are incomplete: {incomplete:?}"
    );
}

#[test]
fn unequal_boundaries_refuse_nominal_addressing_before_publication() {
    for rate in [44_100, 48_000] {
        let fixture = fixture(rate);
        let input = source_manifest(&fixture);
        let empty = InputKeys::default();
        let hls = HlsInputKeys::default();
        for target in [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ] {
            let (command, events) = converter();
            let keys = operator_keys(target);
            let command = command.with_output_signaling(keys.signaling.clone());
            let output = fixture.directory.path().join(format!("refused-{target:?}"));
            let error = convert_with_addressing(
                &command,
                VerificationSource {
                    input: &input,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                &output,
                target,
                DashAddressing::TemplateDuration,
                &keys,
            )
            .unwrap_err();
            assert_eq!(error.0, "dash-nominal-duration-not-representable");
            assert!(!output.exists());
            assert_no_tools(&events);
        }
    }
}
