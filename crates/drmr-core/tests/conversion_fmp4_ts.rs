//! Fragmented MP4 inputs retain their media when repackaged as transport-stream HLS.
mod common;
#[path = "support/fmp4_ts_fixture.rs"]
mod support;

use drmr_core::{
    conversion::{
        Aes128Key, ContentKey, ConversionRequest, Converter, DecodeEvidence, HlsInputKeys,
        HlsOutputKey, InputKeys, OutputKey, ProcessingMode, SourceValidationPurpose,
        ValidationDepth, VerificationReport, VerificationRequest, VerificationSource, key_file,
    },
    events::Collector,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4")
}

fn converter() -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    (
        Converter::new(
            Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone())),
            "packager",
            "must-not-run-fmp4-ts-ffmpeg",
        )
        .with_ffprobe("must-not-run-fmp4-ts-ffprobe"),
        events,
    )
}

fn fragmented_source(root: &Path) -> std::path::PathBuf {
    direct_source(root, "video")
}

fn direct_source(root: &Path, role: &str) -> PathBuf {
    drmr_core::native::create_media_dir_all(root).unwrap();
    let clip = fixture();
    let result = Command::new("packager")
        .arg("--single_threaded")
        .arg(format!(
            "in={},stream={role},init_segment={},segment_template={}",
            clip.display(),
            root.join("video-init.mp4").display(),
            root.join("video-$Number$.m4s").display()
        ))
        .arg("--mpd_output")
        .arg(root.join("manifest.mpd"))
        .args([
            "--segment_duration",
            "2",
            "--clear_lead",
            "0",
            "--generate_static_live_mpd",
            "--mp4_reset_initial_composition_offset_to_zero=false",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "licensed short fixture generation failed"
    );
    root.join("manifest.mpd")
}

fn assert_native(report: &VerificationReport) {
    assert!(report.passed && report.checks.is_pass(), "{report:?}");
    assert_eq!(report.required_depth, ValidationDepth::Native);
    assert_eq!(report.decode_evidence, DecodeEvidence::not_requested());
    assert!(!report.license_playback_verified);
    if let Some(manifest) = &report.native_manifest {
        assert_eq!(manifest.errors, 0);
    }
}

fn assert_packaging_only(report: &drmr_core::conversion::measurement::MeasurementReport) {
    let measured = serde_json::to_value(report).unwrap();
    for tool in measured["tools"].as_array().unwrap() {
        if tool["tool"] != "packager" {
            assert_eq!(
                tool["calls"], 0,
                "native processing started a decoder/probe"
            );
        }
    }
}

fn source_view<'a>(
    input: &'a Path,
    keys: &'a InputKeys,
    hls: &'a HlsInputKeys,
) -> VerificationSource<'a> {
    VerificationSource {
        input,
        input_keys: keys,
        hls_keys: hls,
    }
}

fn round_trip(
    root: &Path,
    name: &str,
    input: &Path,
    input_keys: &InputKeys,
    encrypted: bool,
    tracks: usize,
) -> (PathBuf, key_file::OperatorKeys) {
    let keys = if encrypted {
        support::fairplay_keys(root, &format!("{name}-keys"), support::KEY, support::IV)
    } else {
        key_file::OperatorKeys::default()
    };
    let output = root.join(name);
    let hls = HlsInputKeys::default();
    let (command, events) = converter();
    let command = command.with_output_signaling(keys.signaling.clone());
    let target = if encrypted {
        TargetProtection::HlsSampleAes
    } else {
        TargetProtection::Clear
    };
    let before = common::snapshot_tree(input.parent().unwrap());
    let (result, measured) = command.measure(|command| {
        command.convert(ConversionRequest {
            input,
            output: &output,
            format: OutputFormat::HlsTs,
            target,
            input_keys,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: keys.hls_output.as_ref(),
        })
    });
    let report = result.unwrap_or_else(|error| panic!("{name}: {error}"));
    assert_native(&report.verification);
    assert_packaging_only(&measured);
    assert_eq!(report.output.tracks.len(), tracks);
    if encrypted {
        assert_eq!(report.output.drm_systems, ["fairplay"]);
    } else {
        assert_eq!(report.output.drm_systems, [] as [String; 0]);
    }
    support::assert_transport_manifest(&output, encrypted, tracks);
    let output_input_keys = keys.output_verification_keys().unwrap();
    let output_hls_keys = keys.output_hls_verification_keys().unwrap();
    let entrypoint = output.join("master.m3u8");
    let (verified, measured) = command.measure(|command| {
        command.verify(VerificationRequest {
            source: source_view(input, input_keys, &hls),
            output: source_view(&entrypoint, &output_input_keys, &output_hls_keys),
        })
    });
    assert_native(&verified.unwrap_or_else(|error| panic!("{name} independent verify: {error}")));
    assert_packaging_only(&measured);
    if encrypted {
        command
            .verify_output_signaling(&entrypoint, OutputFormat::HlsTs, target, &keys)
            .unwrap();
    }
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
    let emitted = serde_json::to_string(&events.records()).unwrap();
    for secret in [support::KEY, support::IV, support::URI] {
        assert!(
            !emitted.contains(secret),
            "operator values entered progress records"
        );
    }
    println!(
        "DRMR_FMP4_TS_CELL {}",
        serde_json::json!({
            "case":name,"input_format":report.input.format,"input_protection":report.input.protection,
            "output_protection":target,"tracks":tracks,"media_verified":true,
            "independent_media_verified":true,"decode_checked":false,"license_playback_verified":false
        })
    );
    (output, keys)
}

fn assert_refused(result: Result<VerificationReport, drmr_core::conversion::ConversionError>) {
    match result {
        Ok(report) => assert!(
            !report.passed,
            "changed media passed independent verification"
        ),
        Err(error) => assert!(
            !matches!(
                error.0,
                "cleanup-unconfirmed" | "media-tool-timeout" | "cancelled" | "resource-budget"
            ),
            "an unrelated runtime failure is not a media refusal"
        ),
    }
}

fn reject_changed_result(
    input: &Path,
    input_keys: &InputKeys,
    output: &Path,
    keys: &key_file::OperatorKeys,
) {
    let (command, _) = converter();
    let empty = InputKeys::default();
    let no_hls = HlsInputKeys::default();
    let output_hls = keys.output_hls_verification_keys().unwrap();
    let entrypoint = output.join("master.m3u8");
    let verify = |hls: &HlsInputKeys| {
        command.verify(VerificationRequest {
            source: source_view(input, input_keys, &no_hls),
            output: source_view(&entrypoint, &empty, hls),
        })
    };
    for (key, iv) in [
        ("303132333435363738393a3b3c3d3e3f", support::IV),
        (support::KEY, "404142434445464748494a4b4c4d4e4f"),
    ] {
        let wrong = drmr_core::conversion::HlsInputKeys::new_with_ivs(vec![(
            support::URI.into(),
            drmr_core::conversion::Aes128Key::from_hex(key).unwrap(),
            Some(drmr_core::conversion::SegmentIv::from_hex(iv).unwrap()),
        )])
        .unwrap();
        assert_refused(verify(&wrong));
    }
    let playlist = support::media_playlists(output).into_iter().next().unwrap();
    let original = std::fs::read_to_string(&playlist).unwrap();
    let changed = original.replace(
        "KEYFORMAT=\"com.apple.streamingkeydelivery\"",
        "KEYFORMAT=\"identity\"",
    );
    assert_ne!(changed, original);
    std::fs::write(&playlist, changed).unwrap();
    assert!(
        command
            .verify_output_signaling(
                &entrypoint,
                OutputFormat::HlsTs,
                TargetProtection::HlsSampleAes,
                keys,
            )
            .is_err()
    );
    std::fs::write(&playlist, &original).unwrap();
    let resource = original
        .lines()
        .rfind(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap();
    let resource = playlist.parent().unwrap().join(resource);
    let bytes = std::fs::read(&resource).unwrap();
    assert!(bytes.len().is_multiple_of(188));
    let mut changed = bytes.clone();
    let payload_packet = changed
        .as_chunks_mut::<188>()
        .0
        .iter_mut()
        .rfind(|packet| {
            if packet[3] & 0x10 == 0 {
                return false;
            }
            let at = if packet[3] & 0x20 == 0 {
                4
            } else {
                5 + usize::from(packet[4])
            };
            packet[1] & 0x40 != 0
                && packet.get(at..at + 3) == Some(&[0, 0, 1])
                && packet
                    .get(at + 3)
                    .is_some_and(|stream| (0xc0..=0xef).contains(stream))
        })
        .expect("the final segment contains encoded media in a PES packet");
    payload_packet[187] ^= 1;
    std::fs::write(&resource, changed).unwrap();
    assert_refused(verify(&output_hls));
    std::fs::write(&resource, bytes).unwrap();
    assert_native(&verify(&output_hls).unwrap());
}

fn decoded_comparison(
    input: &Path,
    input_keys: &InputKeys,
    output: &Path,
    keys: &key_file::OperatorKeys,
) {
    let events = Arc::new(Collector::default());
    let command = Converter::new(
        Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events)),
        "packager",
        "ffmpeg",
    )
    .with_decode(true);
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let output_hls = keys.output_hls_verification_keys().unwrap();
    let (report, measured) = command.measure(|command| {
        command.verify(VerificationRequest {
            source: source_view(input, input_keys, &hls),
            output: source_view(&output.join("master.m3u8"), &empty, &output_hls),
        })
    });
    let report = report.unwrap();
    assert!(report.passed && report.checks.is_pass());
    assert_eq!(report.required_depth, ValidationDepth::Decoded);
    assert_eq!(
        report.decode_evidence,
        DecodeEvidence::ContentCompared { equal: true }
    );
    let measured = serde_json::to_value(measured).unwrap();
    assert!(
        measured["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| { tool["tool"] == "ffmpeg" && tool["calls"].as_u64().unwrap() > 0 })
    );
}

#[test]
fn clear_dash_video_has_native_transport_output_and_fresh_verification() {
    let root = common::private_tempdir().unwrap();
    let input = fragmented_source(&root.path().join("source"));
    let before = common::snapshot_tree(input.parent().unwrap());
    let output = root.path().join("result");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let (command, _) = converter();
    let report = command
        .convert(ConversionRequest {
            input: &input,
            output: &output,
            format: OutputFormat::HlsTs,
            target: TargetProtection::Clear,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    assert!(report.verification.passed);
    assert_eq!(
        report.verification.decode_evidence,
        DecodeEvidence::not_requested()
    );
    let verified = command
        .verify(VerificationRequest {
            source: VerificationSource {
                input: &input,
                input_keys: &empty,
                hls_keys: &hls,
            },
            output: VerificationSource {
                input: &output.join("master.m3u8"),
                input_keys: &empty,
                hls_keys: &hls,
            },
        })
        .unwrap();
    assert!(verified.passed && verified.checks.is_pass());
    assert_eq!(verified.decode_evidence, DecodeEvidence::not_requested());
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
    let (command, _) = converter();
    let refused = root.path().join("segments-cannot-change-container");
    assert_eq!(
        command
            .with_processing(ProcessingMode::Segments)
            .convert(ConversionRequest {
                input: &input,
                output: &refused,
                format: OutputFormat::HlsTs,
                target: TargetProtection::Clear,
                input_keys: &empty,
                hls_input_keys: &hls,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap_err()
            .0,
        "processing-container-change-not-supported"
    );
    assert!(!refused.exists());
}

#[test]
fn encrypted_dash_and_hls_fmp4_keep_joint_transport_media_and_fairplay_declarations() {
    let root = common::private_tempdir().unwrap();
    let original = support::transport_source(&root.path().join("original"), "joint", None);
    let clear_dash = support::fmp4_source(
        &root.path().join("clear-dash"),
        &original,
        OutputFormat::Dash,
        TargetProtection::Clear,
        ProcessingMode::Stitch,
    );
    let empty = InputKeys::default();
    let (clear_ts, _) = round_trip(
        root.path(),
        "dash-clear-to-clear",
        &clear_dash,
        &empty,
        false,
        2,
    );
    let hls = HlsInputKeys::default();
    let (command, _) = converter();
    assert_native(
        &command
            .verify(VerificationRequest {
                source: source_view(&original, &empty, &hls),
                output: source_view(&clear_ts.join("master.m3u8"), &empty, &hls),
            })
            .unwrap(),
    );
    round_trip(
        root.path(),
        "dash-clear-to-fairplay",
        &clear_dash,
        &empty,
        true,
        2,
    );
    for (name, format, protection) in [
        ("dash-cenc", OutputFormat::Dash, TargetProtection::Cenc),
        ("dash-cbcs", OutputFormat::Dash, TargetProtection::Cbcs),
        ("hls-clear", OutputFormat::Hls, TargetProtection::Clear),
        ("hls-cbcs", OutputFormat::Hls, TargetProtection::Cbcs),
    ] {
        let input = support::fmp4_source(
            &root.path().join(name),
            &clear_dash,
            format,
            protection,
            ProcessingMode::Segments,
        );
        let keys = support::source_keys(protection);
        for encrypted in [false, true] {
            let output = if encrypted { "fairplay" } else { "clear" };
            let (output, output_keys) = round_trip(
                root.path(),
                &format!("{name}-to-{output}"),
                &input,
                &keys,
                encrypted,
                2,
            );
            if name == "dash-cenc" && encrypted {
                reject_changed_result(&input, &keys, &output, &output_keys);
                decoded_comparison(&input, &keys, &output, &output_keys);
            }
        }
        if name == "dash-cenc" {
            wrong_input_key_requires_an_independent_reference(root.path(), &input, &keys);
        }
    }
    let hls_cenc = support::cenc_hls_source(&root.path().join("hls-cenc"), &original);
    let keys = support::source_keys(TargetProtection::Cenc);
    for encrypted in [false, true] {
        round_trip(
            root.path(),
            &format!("hls-cenc-to-{encrypted}"),
            &hls_cenc,
            &keys,
            encrypted,
            2,
        );
    }
}

#[test]
fn audio_only_44100_and_48000_keep_native_frame_clocks_and_every_sample() {
    let root = common::private_tempdir().unwrap();
    for rate in [44_100, 48_000] {
        let original = support::transport_source(
            &root.path().join(format!("original-{rate}")),
            "audio",
            (rate == 44_100).then_some(rate),
        );
        let dash = support::fmp4_source(
            &root.path().join(format!("dash-{rate}")),
            &original,
            OutputFormat::Dash,
            TargetProtection::Clear,
            ProcessingMode::Stitch,
        );
        let empty = InputKeys::default();
        for encrypted in [false, true] {
            round_trip(
                root.path(),
                &format!("audio-{rate}-{encrypted}"),
                &dash,
                &empty,
                encrypted,
                1,
            );
        }
        if rate == 44_100 {
            let hls = support::fmp4_source(
                &root.path().join("hls-44100"),
                &dash,
                OutputFormat::Hls,
                TargetProtection::Cbcs,
                ProcessingMode::Segments,
            );
            let keys = support::source_keys(TargetProtection::Cbcs);
            round_trip(
                root.path(),
                "audio-hls-44100-fairplay",
                &hls,
                &keys,
                true,
                1,
            );
        }
    }
}

#[test]
fn fmp4_aac_priming_is_refused_without_publishing_a_transport_result() {
    let root = common::private_tempdir().unwrap();
    let input = direct_source(&root.path().join("primed-audio"), "audio");
    let before = common::snapshot_tree(input.parent().unwrap());
    let output = root.path().join("result");
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let (command, _) = converter();
    assert!(
        command
            .validate_source(
                source_view(&input, &empty, &hls),
                SourceValidationPurpose::Conversion,
            )
            .unwrap()
            .verification
            .passed,
        "the primed AAC source is valid fMP4"
    );
    let result = command.convert(ConversionRequest {
        input: &input,
        output: &output,
        format: OutputFormat::HlsTs,
        target: TargetProtection::Clear,
        input_keys: &empty,
        hls_input_keys: &hls,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    });
    assert_eq!(
        result.unwrap_err().0,
        "ts-output-presentation-not-representable"
    );
    assert!(!output.exists(), "a refused conversion published an output");
    assert_eq!(common::snapshot_tree(input.parent().unwrap()), before);
}

#[test]
fn standalone_fragmented_mp4_and_aes128_transport_outputs_are_verified() {
    use std::io::Write as _;
    let root = common::private_tempdir().unwrap();
    let manifest = fragmented_source(&root.path().join("source"));
    let directory = manifest.parent().unwrap();
    let input = root.path().join("input.mp4");
    let mut output = drmr_core::native::create_private_new(&input).unwrap();
    output
        .write_all(&std::fs::read(directory.join("video-init.mp4")).unwrap())
        .unwrap();
    let mut resources = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "m4s"))
        .collect::<Vec<_>>();
    resources.sort();
    assert_eq!(resources.len(), 5);
    for resource in resources {
        output.write_all(&std::fs::read(resource).unwrap()).unwrap();
    }
    drop(output);
    let empty = InputKeys::default();
    let empty_hls = HlsInputKeys::default();
    let key = HlsOutputKey::new(support::URI, Aes128Key::from_hex(support::KEY).unwrap()).unwrap();
    let hls_keys = HlsInputKeys::new_with_ivs(vec![(
        support::URI.into(),
        Aes128Key::from_hex(support::KEY).unwrap(),
        None,
    )])
    .unwrap();
    let (command, _) = converter();
    for (ordinal, source) in [&manifest, &input].into_iter().enumerate() {
        let destination = root.path().join(format!("aes-{ordinal}"));
        let report = command
            .convert(ConversionRequest {
                input: source,
                output: &destination,
                format: OutputFormat::HlsTs,
                target: TargetProtection::HlsAes128,
                input_keys: &empty,
                hls_input_keys: &empty_hls,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: Some(&key),
            })
            .unwrap();
        assert_native(&report.verification);
        assert_eq!(
            serde_json::to_value(report).unwrap()["processing"]["route"],
            "fmp4-hls-ts"
        );
        let compared = command
            .verify(VerificationRequest {
                source: source_view(source, &empty, &empty_hls),
                output: source_view(&destination.join("master.m3u8"), &empty, &hls_keys),
            })
            .unwrap();
        assert_native(&compared);
    }
}

#[test]
fn whole_segment_aes128_fmp4_input_has_checked_clear_and_fairplay_transport_output() {
    let root = common::private_tempdir().unwrap();
    let original = support::transport_source(&root.path().join("original"), "joint", None);
    let clear_hls = support::fmp4_source(
        &root.path().join("clear"),
        &original,
        OutputFormat::Hls,
        TargetProtection::Clear,
        ProcessingMode::Stitch,
    );
    let aes = HlsOutputKey::new(support::URI, Aes128Key::from_hex(support::KEY).unwrap()).unwrap();
    let source_keys = HlsInputKeys::new_with_ivs(vec![(
        support::URI.into(),
        Aes128Key::from_hex(support::KEY).unwrap(),
        None,
    )])
    .unwrap();
    let empty = InputKeys::default();
    let no_hls = HlsInputKeys::default();
    let input_dir = root.path().join("encrypted");
    let (command, _) = converter();
    converter()
        .0
        .with_processing(ProcessingMode::Segments)
        .convert(ConversionRequest {
            input: &clear_hls,
            output: &input_dir,
            format: OutputFormat::Hls,
            target: TargetProtection::HlsAes128,
            input_keys: &empty,
            hls_input_keys: &no_hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: Some(&aes),
        })
        .unwrap();
    let fairplay = support::fairplay_keys(root.path(), "output-keys", support::KEY, support::IV);
    for (ordinal, target) in [TargetProtection::Clear, TargetProtection::HlsSampleAes]
        .into_iter()
        .enumerate()
    {
        let output = root.path().join(format!("ts-{ordinal}"));
        let encrypted = target == TargetProtection::HlsSampleAes;
        let conversion = converter().0.with_output_signaling(if encrypted {
            fairplay.signaling.clone()
        } else {
            None
        });
        let report = conversion
            .convert(ConversionRequest {
                input: &input_dir.join("master.m3u8"),
                output: &output,
                format: OutputFormat::HlsTs,
                target,
                input_keys: &empty,
                hls_input_keys: &source_keys,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: if encrypted {
                    fairplay.hls_output.as_ref()
                } else {
                    None
                },
            })
            .unwrap();
        assert_native(&report.verification);
        let keys = if encrypted {
            fairplay.output_hls_verification_keys().unwrap()
        } else {
            HlsInputKeys::default()
        };
        let compared = command
            .verify(VerificationRequest {
                source: source_view(&original, &empty, &no_hls),
                output: source_view(&output.join("master.m3u8"), &empty, &keys),
            })
            .unwrap();
        assert_native(&compared);
    }
}

fn wrong_input_key_requires_an_independent_reference(root: &Path, input: &Path, keys: &InputKeys) {
    let destination = root.join("wrong-input-key-output");
    let wrong = InputKeys::new(vec![
        ContentKey::from_hex(support::KID, "404142434445464748494a4b4c4d4e4f").unwrap(),
    ])
    .unwrap();
    let (command, _) = converter();
    let converted = command.convert(ConversionRequest {
        input,
        output: &destination,
        format: OutputFormat::HlsTs,
        target: TargetProtection::Clear,
        input_keys: &wrong,
        hls_input_keys: &HlsInputKeys::default(),
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    });
    if converted.is_ok() {
        // CENC does not authenticate a supplied key. An independent
        // reference using the known correct key must reject this media.
        let compared = command.verify(VerificationRequest {
            source: source_view(input, keys, &HlsInputKeys::default()),
            output: source_view(
                &destination.join("master.m3u8"),
                &InputKeys::default(),
                &HlsInputKeys::default(),
            ),
        });
        assert!(!compared.is_ok_and(|report| report.passed));
    } else {
        assert!(!destination.exists());
    }
}
