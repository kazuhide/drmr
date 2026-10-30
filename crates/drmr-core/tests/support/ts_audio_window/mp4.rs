//! Standalone MP4 keeps exact terminal audio selections, clocks and language.
use super::*;

#[test]
fn ts_mp4_partial_aac_keeps_every_coded_frame_and_exact_presented_end() {
    use drmr_core::conversion::{
        ContentKey, OutputKey, SourceValidationPurpose, VerificationRequest, VerificationSource,
    };
    const KID: &str = "00112233445566778899aabbccddeeff";
    for profile in [AAC_44100, AAC_48000] {
        let root = drmr_core::native::private_tempdir().unwrap();
        let source = audio_sources_at_rate(root.path(), Some(profile.rate)).0;
        let packets = shorten_final_declaration(&source, profile);
        let original = root.path().join("original.ts");
        join_resources(&source, &original, false);
        let reference = raw_audio(&original, false, profile);
        let empty = InputKeys::default();
        let hls = HlsInputKeys::default();
        let key = OutputKey::from_hex(KID, OUTPUT_KEY).unwrap();
        let output_keys =
            InputKeys::new(vec![ContentKey::from_hex(KID, OUTPUT_KEY).unwrap()]).unwrap();
        for target in [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ] {
            let output = root.path().join(format!("result-{}.mp4", target.as_str()));
            let native = native_window_converter("packager");
            let report = native
                .convert(ConversionRequest {
                    input: &source,
                    output: &output,
                    format: OutputFormat::Mp4,
                    target,
                    input_keys: &empty,
                    hls_input_keys: &hls,
                    output_key: (target != TargetProtection::Clear).then_some(&key),
                    hls_key_uri: None,
                    hls_output_key: None,
                })
                .unwrap_or_else(|e| {
                    panic!("{} {} convert: {}", profile.rate, target.as_str(), e.0)
                });
            assert_native_window_proof(&report.verification);
            let verify = || {
                native_window_converter("must-not-run-packager").verify(VerificationRequest {
                    source: VerificationSource {
                        input: &source,
                        input_keys: &empty,
                        hls_keys: &hls,
                    },
                    output: VerificationSource {
                        input: &output,
                        input_keys: if target == TargetProtection::Clear {
                            &empty
                        } else {
                            &output_keys
                        },
                        hls_keys: &hls,
                    },
                })
            };
            assert_native_window_proof(&verify().unwrap());
            native
                .validate_source(
                    VerificationSource {
                        input: &output,
                        input_keys: if target == TargetProtection::Clear {
                            &empty
                        } else {
                            &output_keys
                        },
                        hls_keys: &hls,
                    },
                    SourceValidationPurpose::Conversion,
                )
                .unwrap();
            if target == TargetProtection::Clear {
                assert_eq!(raw_audio(&output, true, profile), reference);
                assert_two_standard_edits(&output, profile, packets[0][0]);
                let bytes = std::fs::read(&output).unwrap();
                for (name, mutation) in edit_mutations(&bytes, profile) {
                    std::fs::write(&output, mutation).unwrap();
                    let proof = verify();
                    assert!(
                        proof.is_err() || !proof.unwrap().passed,
                        "accepted changed MP4 edit {name}"
                    );
                    std::fs::write(&output, &bytes).unwrap();
                }
                assert!(verify().unwrap().passed);
            }
        }
    }
}

#[test]
fn ts_mp4_joint_44100_keeps_audio_language_and_partial_end_with_video() {
    use drmr_core::conversion::{
        ContentKey, OutputKey, SourceValidationPurpose, VerificationRequest, VerificationSource,
    };
    const KID: &str = "00112233445566778899aabbccddeeff";
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("input");
    drmr_core::native::create_media_dir_all(&input).unwrap();
    shaka_source(&input.join("video"), "video", false);
    let audio = audio_sources_at_rate(&input.join("audio"), Some(44_100)).0;
    shorten_final_declaration(&audio, AAC_44100);
    let source = input.join("master.m3u8");
    std::fs::write(&source,"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"Main\",LANGUAGE=\"en\",URI=\"audio/clear/media.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=320x180,AUDIO=\"audio\"\nvideo/media.m3u8\n").unwrap();
    let before = common::snapshot_tree(&input);
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let key = OutputKey::from_hex(KID, OUTPUT_KEY).unwrap();
    let output_keys = InputKeys::new(vec![ContentKey::from_hex(KID, OUTPUT_KEY).unwrap()]).unwrap();
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        let output = root.path().join(format!("result-{}.mp4", target.as_str()));
        let native = native_window_converter("packager");
        let report = native
            .convert(ConversionRequest {
                input: &source,
                output: &output,
                format: OutputFormat::Mp4,
                target,
                input_keys: &empty,
                hls_input_keys: &hls,
                output_key: (target != TargetProtection::Clear).then_some(&key),
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap_or_else(|e| panic!("joint {} convert: {}", target.as_str(), e.0));
        assert_native_window_proof(&report.verification);
        let verify = || {
            native_window_converter("must-not-run-packager").verify(VerificationRequest {
                source: VerificationSource {
                    input: &source,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: &output,
                    input_keys: if target == TargetProtection::Clear {
                        &empty
                    } else {
                        &output_keys
                    },
                    hls_keys: &hls,
                },
            })
        };
        assert_native_window_proof(&verify().unwrap());
        native
            .validate_source(
                VerificationSource {
                    input: &output,
                    input_keys: if target == TargetProtection::Clear {
                        &empty
                    } else {
                        &output_keys
                    },
                    hls_keys: &hls,
                },
                SourceValidationPurpose::Conversion,
            )
            .unwrap();
        if target == TargetProtection::Clear {
            assert_mp4_language_mutation_rejected(&output, verify);
        }
        let decoded = converter()
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &source,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: &output,
                    input_keys: if target == TargetProtection::Clear {
                        &empty
                    } else {
                        &output_keys
                    },
                    hls_keys: &hls,
                },
            })
            .unwrap();
        assert!(
            decoded.passed,
            "{}",
            serde_json::to_string(&decoded).unwrap()
        );
        all_checks(&decoded);
    }
    assert_eq!(common::snapshot_tree(&input), before);
}

fn assert_mp4_language_mutation_rejected(
    output: &Path,
    verify: impl Fn() -> Result<
        drmr_core::conversion::VerificationReport,
        drmr_core::conversion::ConversionError,
    >,
) {
    let bytes = std::fs::read(output).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&bytes).unwrap();
    let moov = boxes.iter().find(|node| node.kind() == *b"moov").unwrap();
    let tracks: Vec<_> = moov
        .children
        .iter()
        .filter(|node| node.kind() == *b"trak")
        .collect();
    assert_eq!(tracks.len(), 2);
    let audio = tracks[1];
    let nodes = drmr_core::mp4::walk(std::slice::from_ref(audio));
    let mdhd = nodes.iter().find(|node| node.kind() == *b"mdhd").unwrap();
    let at = mdhd.header.offset
        + mdhd.header.header_size
        + if mdhd.header.payload(&bytes)[0] == 0 {
            20
        } else {
            32
        };
    let mut changed = bytes.clone();
    changed[at..at + 2].copy_from_slice(&0x55c4u16.to_be_bytes());
    std::fs::write(output, changed).unwrap();
    let proof = verify();
    assert!(
        proof.is_err() || !proof.unwrap().passed,
        "accepted changed audio language"
    );
    std::fs::write(output, &bytes).unwrap();
    assert!(verify().unwrap().passed);
}
