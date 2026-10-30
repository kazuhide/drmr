//! Canonical MP4 output can enter another conversion without weakening edit admission.
use super::*;
use drmr_core::conversion::{
    ContentKey, OutputKey, VerificationRequest, VerificationSource, plan::ExecutionPlanRequest,
};

const KID: &str = "00112233445566778899aabbccddeeff";
const PROTECTIONS: [TargetProtection; 3] = [
    TargetProtection::Clear,
    TargetProtection::Cenc,
    TargetProtection::Cbcs,
];

fn keys(target: TargetProtection) -> InputKeys {
    if target == TargetProtection::Clear {
        InputKeys::default()
    } else {
        InputKeys::new(vec![ContentKey::from_hex(KID, OUTPUT_KEY).unwrap()]).unwrap()
    }
}

fn convert(
    command: &Converter,
    input: &Path,
    output: &Path,
    input_keys: &InputKeys,
    format: OutputFormat,
    target: TargetProtection,
) {
    let hls = HlsInputKeys::default();
    let output_key = OutputKey::from_hex(KID, OUTPUT_KEY).unwrap();
    let result = command
        .convert(ConversionRequest {
            input,
            output,
            format,
            target,
            input_keys,
            hls_input_keys: &hls,
            output_key: (target != TargetProtection::Clear).then_some(&output_key),
            hls_key_uri: (format == OutputFormat::Hls && target != TargetProtection::Clear)
                .then_some("https://keys.example.invalid/roundtrip"),
            hls_output_key: None,
        })
        .unwrap_or_else(|e| panic!("{} {}: {}", format.as_str(), target.as_str(), e.0));
    assert_native_window_proof(&result.verification);
}

fn verify_original(command: &Converter, source: &Path, output: &Path, target: TargetProtection) {
    let empty = InputKeys::default();
    let result_keys = keys(target);
    let hls = HlsInputKeys::default();
    let result = command
        .verify(VerificationRequest {
            source: VerificationSource {
                input: source,
                input_keys: &empty,
                hls_keys: &hls,
            },
            output: VerificationSource {
                input: output,
                input_keys: &result_keys,
                hls_keys: &hls,
            },
        })
        .unwrap();
    assert_native_window_proof(&result);
}

fn manifest_cycle(
    command: &Converter,
    source: &Path,
    mp4: &Path,
    root: &Path,
    target: TargetProtection,
) {
    let empty = InputKeys::default();
    let input_keys = keys(target);
    let hls = HlsInputKeys::default();
    for format in [OutputFormat::Dash, OutputFormat::Hls] {
        let output = root.join(format!("{}-{}", target.as_str(), format.as_str()));
        command
            .create_execution_plan(ExecutionPlanRequest {
                input: VerificationSource {
                    input: mp4,
                    input_keys: &input_keys,
                    hls_keys: &hls,
                },
                output: &output,
                format: Some(format),
                target: TargetProtection::Clear,
            })
            .unwrap();
        convert(
            command,
            mp4,
            &output,
            &input_keys,
            format,
            TargetProtection::Clear,
        );
        let input = output.join(if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        verify_original(command, source, &input, TargetProtection::Clear);
        let returned = root.join(format!(
            "returned-{}-{}.mp4",
            target.as_str(),
            format.as_str()
        ));
        convert(
            command,
            &input,
            &returned,
            &empty,
            OutputFormat::Mp4,
            target,
        );
        verify_original(command, source, &returned, target);
    }
}

#[test]
fn canonical_terminal_mp4_reinput_preserves_audio_in_dash_and_hls_round_trips() {
    for profile in [AAC_44100, AAC_48000] {
        let root = drmr_core::native::private_tempdir().unwrap();
        let source = audio_sources_at_rate(root.path(), Some(profile.rate)).0;
        shorten_final_declaration(&source, profile);
        let original = common::snapshot_tree(source.parent().unwrap());
        let empty = InputKeys::default();
        let command = native_window_converter("packager");
        for target in PROTECTIONS {
            let mp4 = root
                .path()
                .join(format!("intermediate-{}.mp4", target.as_str()));
            convert(&command, &source, &mp4, &empty, OutputFormat::Mp4, target);
            manifest_cycle(&command, &source, &mp4, root.path(), target);
            for next in PROTECTIONS {
                let output = root
                    .path()
                    .join(format!("{}-{}.mp4", target.as_str(), next.as_str()));
                convert(
                    &command,
                    &mp4,
                    &output,
                    &keys(target),
                    OutputFormat::Mp4,
                    next,
                );
                verify_original(&command, &source, &output, next);
            }
        }
        assert_eq!(common::snapshot_tree(source.parent().unwrap()), original);
    }
}

#[test]
fn canonical_reinput_refuses_noncanonical_edit_rates_and_unbound_clocks() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let source = audio_sources_at_rate(root.path(), Some(44_100)).0;
    shorten_final_declaration(&source, AAC_44100);
    let command = native_window_converter("packager");
    let mp4 = root.path().join("source.mp4");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    convert(
        &command,
        &source,
        &mp4,
        &empty,
        OutputFormat::Mp4,
        TargetProtection::Clear,
    );
    let original = std::fs::read(&mp4).unwrap();
    for (name, bytes) in edit_mutations(&original, AAC_44100) {
        if !matches!(
            name,
            "unknown-edit-rate" | "one-tick-empty-edit" | "one-tick-media-time"
        ) {
            continue;
        }
        std::fs::write(&mp4, bytes).unwrap();
        let output = root.path().join(format!("must-not-exist-{name}"));
        let result = command.convert(ConversionRequest {
            input: &mp4,
            output: &output,
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        });
        assert!(result.is_err(), "accepted unqualified edit {name}");
        assert!(!output.exists());
        std::fs::write(&mp4, &original).unwrap();
    }
    verify_original(&command, &source, &mp4, TargetProtection::Clear);
}

#[test]
fn canonical_joint_mp4_reinput_keeps_language_and_complete_audio_video() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("input");
    drmr_core::native::create_media_dir_all(&input).unwrap();
    shaka_source(&input.join("video"), "video", false);
    let audio = audio_sources_at_rate(&input.join("audio"), Some(44_100)).0;
    shorten_final_declaration(&audio, AAC_44100);
    let source = input.join("master.m3u8");
    std::fs::write(&source, "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Main\",LANGUAGE=\"en\",URI=\"audio/clear/media.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=320x180,AUDIO=\"a\"\nvideo/media.m3u8\n").unwrap();
    let command = native_window_converter("packager");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let protected = keys(TargetProtection::Cbcs);
    let mp4 = root.path().join("joint.mp4");
    convert(
        &command,
        &source,
        &mp4,
        &empty,
        OutputFormat::Mp4,
        TargetProtection::Cbcs,
    );
    for format in [OutputFormat::Dash, OutputFormat::Hls] {
        let output = root.path().join(format!("joint-{}", format.as_str()));
        convert(
            &command,
            &mp4,
            &output,
            &protected,
            format,
            TargetProtection::Clear,
        );
        let manifest = output.join(if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        let compared = command
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &mp4,
                    input_keys: &protected,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: &manifest,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
            })
            .unwrap();
        assert_native_window_proof(&compared);
        let inspection = drmr_core::inspection::inspect(&manifest).unwrap();
        assert_eq!(inspection.tracks.len(), 2);
    }
}

#[test]
fn canonical_mp4_returns_to_ts_with_exact_44100_and_48000_audio_and_optional_video() {
    for profile in [AAC_44100, AAC_48000] {
        let root = drmr_core::native::private_tempdir().unwrap();
        let input = root.path().join("input");
        drmr_core::native::create_media_dir_all(&input).unwrap();
        let audio = audio_sources_at_rate(&input.join("audio"), Some(profile.rate)).0;
        shorten_final_declaration(&audio, profile);
        shaka_source(&input.join("video"), "video", false);
        let master = input.join("master.m3u8");
        // HLS selection values match generated defaults; MP4 does not store those declarations.
        std::fs::write(&master, "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"default-audio-group\",NAME=\"stream_1\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=YES,CHANNELS=\"2\",URI=\"audio/clear/media.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=320x180,AUDIO=\"default-audio-group\"\nvideo/media.m3u8\n").unwrap();
        let before = common::snapshot_tree(&input);
        let command = native_window_converter("packager");
        let empty = InputKeys::default();
        let hls = HlsInputKeys::default();
        for (layout, source) in [("audio", &audio), ("joint", &master)] {
            for target in PROTECTIONS {
                let label = format!("{layout}-{}", target.as_str());
                let mp4 = root.path().join(format!("{label}.mp4"));
                convert(&command, source, &mp4, &empty, OutputFormat::Mp4, target);
                let protected = keys(target);
                let output = root.path().join(format!("ts-{label}"));
                command
                    .create_execution_plan(ExecutionPlanRequest {
                        input: VerificationSource {
                            input: &mp4,
                            input_keys: &protected,
                            hls_keys: &hls,
                        },
                        output: &output,
                        format: Some(OutputFormat::HlsTs),
                        target: TargetProtection::Clear,
                    })
                    .unwrap();
                convert(
                    &command,
                    &mp4,
                    &output,
                    &protected,
                    OutputFormat::HlsTs,
                    TargetProtection::Clear,
                );
                let result = output.join("master.m3u8");
                let proof = command
                    .verify(VerificationRequest {
                        source: VerificationSource {
                            input: source,
                            input_keys: &empty,
                            hls_keys: &hls,
                        },
                        output: VerificationSource {
                            input: &result,
                            input_keys: &empty,
                            hls_keys: &hls,
                        },
                    })
                    .unwrap_or_else(|error| {
                        panic!(
                            "{} {label}: {}; source: {}; result: {}",
                            profile.rate,
                            error.0,
                            std::fs::read_to_string(source).unwrap(),
                            std::fs::read_to_string(&result).unwrap()
                        )
                    });
                assert_native_window_proof(&proof);
            }
        }
        assert_eq!(common::snapshot_tree(&input), before);
    }
}
