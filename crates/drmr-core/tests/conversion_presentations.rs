//! Joint presentations verify each original track, including full audio decode and timing.
use drmr_core::{
    conversion::{
        Aes128Key, ContentKey, ConversionRequest, Converter, HlsInputKeys, HlsOutputKey, InputKeys,
        LocalTrack, OutputKey, TrackConversionRequest, VerificationRequest, VerificationSource,
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection, TrackKind},
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
fn source(path: &Path, audio: bool) {
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-v", "error", "-f", "lavfi", "-i"]);
    if audio {
        cmd.args([
            "sine=sample_rate=48000:frequency=440",
            "-t",
            "10",
            "-c:a",
            "aac",
        ]);
    } else {
        cmd.args([
            "testsrc2=size=64x64:rate=5",
            "-t",
            "10",
            "-c:v",
            "libx264",
            "-g",
            "5",
        ]);
    }
    cmd.args(["-movflags", "+frag_keyframe+empty_moov"])
        .arg(path);
    assert!(cmd.status().unwrap().success());
}
#[test]
fn joint_audio_video_dash_and_hls_preserve_every_original_track() {
    let d = tempfile::tempdir().unwrap();
    let video = d.path().join("video.mp4");
    let audio = d.path().join("audio.mp4");
    source(&video, false);
    source(&audio, true);
    let tracks = [
        LocalTrack {
            source: video,
            kind: TrackKind::Video,
            index: 0,
        },
        LocalTrack {
            source: audio,
            kind: TrackKind::Audio,
            index: 0,
        },
    ];
    let converter = converter();
    let keys = InputKeys::default();
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    let aes = HlsOutputKey::new(
        "https://operator.invalid/private-label",
        Aes128Key::from_hex(KEY).unwrap(),
    )
    .unwrap();
    for (format, target) in [
        (OutputFormat::Dash, TargetProtection::Clear),
        (OutputFormat::Dash, TargetProtection::Cenc),
        (OutputFormat::Dash, TargetProtection::Cbcs),
        (OutputFormat::Hls, TargetProtection::Clear),
        (OutputFormat::Hls, TargetProtection::Cbcs),
        (OutputFormat::Hls, TargetProtection::HlsAes128),
    ] {
        let output = d
            .path()
            .join(format!("{}-{}", format.as_str(), target.as_str()));
        let report = converter
            .convert_tracks(TrackConversionRequest {
                tracks: &tracks,
                output: &output,
                format,
                target,
                input_keys: &keys,
                output_key: matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs)
                    .then_some(&key),
                hls_key_uri: (target == TargetProtection::Cbcs)
                    .then_some("https://operator.invalid/private-label"),
                hls_output_key: (target == TargetProtection::HlsAes128).then_some(&aes),
            })
            .unwrap();
        if format == OutputFormat::Hls {
            check_hls(&converter, &output, target);
        }
        assert!(report.verification.passed);
        assert_eq!(
            serde_json::to_value(&report).unwrap()["processing"]["route"],
            if format == OutputFormat::Dash {
                "dash-presentation"
            } else {
                "fmp4-hls-presentation"
            }
        );
        assert_eq!(
            serde_json::to_value(&report).unwrap()["processing"]["input_preparation"],
            "local-track-files"
        );
        assert_eq!(report.output.tracks.len(), 2);
        assert!(
            output
                .join(if format == OutputFormat::Dash {
                    "manifest.mpd"
                } else {
                    "master.m3u8"
                })
                .is_file()
        );
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("private-label")
        );
    }
}
#[test]
fn declared_track_roles_cannot_authorize_mismatched_media_or_output() {
    let d = tempfile::tempdir().unwrap();
    let video = d.path().join("source.mp4");
    source(&video, false);
    let output = d.path().join("out");
    let tracks = [LocalTrack {
        source: video,
        kind: TrackKind::Audio,
        index: 0,
    }];
    let keys = InputKeys::default();
    let err = converter()
        .convert_tracks(TrackConversionRequest {
            tracks: &tracks,
            output: &output,
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap_err();
    assert_eq!(err.0, "track-role-mismatch");
    assert!(!output.exists());
}

#[test]
fn unified_dash_to_aes_hls_verifies_actual_joint_media_and_refuses_false_timeline() {
    let d = tempfile::tempdir().unwrap();
    let initial = d.path().join("initial");
    std::fs::create_dir(&initial).unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut cmd = std::process::Command::new("packager");
    for role in ["video", "audio"] {
        let dir = initial.join(role);
        std::fs::create_dir(&dir).unwrap();
        cmd.arg(format!(
            "in={},stream={role},init_segment={}/init.mp4,segment_template={}/seg_$Number$.m4s",
            clip.display(),
            dir.display(),
            dir.display()
        ));
    }
    let manifest = initial.join("manifest.mpd");
    let status = cmd
        .args([
            "--segment_duration",
            "2",
            "--generate_static_live_mpd",
            "--mp4_reset_initial_composition_offset_to_zero=false",
            "--mpd_output",
        ])
        .arg(&manifest)
        .output()
        .unwrap();
    assert!(status.status.success());
    let converter = converter();
    let keys = InputKeys::default();
    let mappings = HlsInputKeys::default();
    let aes = HlsOutputKey::new(
        "https://operator.invalid/secret-label",
        Aes128Key::from_hex(KEY).unwrap(),
    )
    .unwrap();
    let output = d.path().join("output");
    let request = ConversionRequest {
        input: &manifest,
        output: &output,
        format: OutputFormat::Hls,
        target: TargetProtection::HlsAes128,
        input_keys: &keys,
        hls_input_keys: &mappings,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: Some(&aes),
    };
    assert!(converter.convert(request).unwrap().verification.passed);
    let output_mappings = HlsInputKeys::new(vec![(
        aes.key_uri().into(),
        Aes128Key::from_hex(KEY).unwrap(),
    )])
    .unwrap();
    let verification = converter
        .verify(VerificationRequest {
            source: VerificationSource {
                input: &manifest,
                input_keys: &keys,
                hls_keys: &mappings,
            },
            output: VerificationSource {
                input: &output.join("master.m3u8"),
                input_keys: &keys,
                hls_keys: &output_mappings,
            },
        })
        .unwrap();
    assert!(verification.passed);
    let text = std::fs::read_to_string(&manifest).unwrap();
    reject_false_windows(&converter, request, &text);
    let doc = roxmltree::Document::parse(&text).unwrap();
    let duration = doc
        .descendants()
        .find(|n| n.tag_name().name() == "S")
        .unwrap()
        .attribute("d")
        .unwrap();
    let changed = duration.parse::<u64>().unwrap() + 1;
    let altered = text.replacen(
        &format!(" d=\"{duration}\""),
        &format!(" d=\"{changed}\""),
        1,
    );
    assert_ne!(text, altered);
    std::fs::write(&manifest, altered).unwrap();
    let denied = d.path().join("denied");
    let err = converter
        .convert(ConversionRequest {
            output: &denied,
            ..request
        })
        .unwrap_err();
    assert_eq!(err.0, "manifest-fragment-timeline-mismatch");
    assert!(!denied.exists());
}

fn check_hls(converter: &Converter, output: &Path, target: TargetProtection) {
    let actual_keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
    let hls_keys = HlsInputKeys::new(vec![(
        "https://operator.invalid/private-label".into(),
        Aes128Key::from_hex(KEY).unwrap(),
    )])
    .unwrap();
    let checked = converter
        .verify(VerificationRequest {
            source: VerificationSource {
                input: &output.join("master.m3u8"),
                input_keys: &actual_keys,
                hls_keys: &hls_keys,
            },
            output: VerificationSource {
                input: &output.join("master.m3u8"),
                input_keys: &actual_keys,
                hls_keys: &hls_keys,
            },
        })
        .unwrap();
    assert!(checked.passed);
    if target == TargetProtection::Clear {
        preserve_custom_rendition(converter, output);
        let media = output.join("track-0000/media.m3u8");
        let text = std::fs::read_to_string(&media).unwrap();
        let line = text.lines().find(|l| l.starts_with("#EXTINF:")).unwrap();
        std::fs::write(&media, text.replacen(line, "#EXTINF:999.000,", 1)).unwrap();
        let error = converter
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &output.join("master.m3u8"),
                    input_keys: &actual_keys,
                    hls_keys: &hls_keys,
                },
                output: VerificationSource {
                    input: &output.join("master.m3u8"),
                    input_keys: &actual_keys,
                    hls_keys: &hls_keys,
                },
            })
            .unwrap_err();
        assert_eq!(error.0, "manifest-segment-duration-mismatch");
    }
}

fn preserve_custom_rendition(converter: &Converter, output: &Path) {
    let manifest = output.join("master.m3u8");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let name = text
        .split("NAME=\"")
        .nth(1)
        .and_then(|v| v.split('"').next())
        .unwrap();
    let altered = text.replace(&format!("NAME=\"{name}\""), "NAME=\"Commentary\"");
    assert_ne!(altered, text);
    std::fs::write(&manifest, altered).unwrap();
    let denied = output.parent().unwrap().join("denied-label");
    let keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let result = converter
        .convert(ConversionRequest {
            input: &manifest,
            output: &denied,
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &keys,
            hls_input_keys: &hls_keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    assert!(result.verification.passed);
    let dash = denied.join("manifest.mpd");
    assert!(
        std::fs::read_to_string(&dash)
            .unwrap()
            .contains("<Label>Commentary</Label>")
    );
    assert!(
        converter
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &manifest,
                    input_keys: &keys,
                    hls_keys: &hls_keys
                },
                output: VerificationSource {
                    input: &dash,
                    input_keys: &keys,
                    hls_keys: &hls_keys
                },
            })
            .unwrap()
            .passed
    );
    std::fs::write(manifest, text).unwrap();
}

fn reject_false_windows(converter: &Converter, request: ConversionRequest<'_>, text: &str) {
    let shortened = text.replacen(
        "mediaPresentationDuration=\"PT10S\"",
        "mediaPresentationDuration=\"PT5S\"",
        1,
    );
    let period = text.replacen(
        "<Period id=\"0\">",
        "<Period id=\"0\" duration=\"PT5S\">",
        1,
    );
    for (name, altered) in [("mpd-window", shortened), ("period-window", period)] {
        assert_ne!(text, altered);
        std::fs::write(request.input, altered).unwrap();
        let denied = request.output.parent().unwrap().join(name);
        let error = converter
            .convert(ConversionRequest {
                output: &denied,
                ..request
            })
            .unwrap_err();
        assert_eq!(error.0, "dash-presentation-duration-mismatch");
        assert!(!denied.exists());
    }
    std::fs::write(request.input, text).unwrap();
}

#[test]
fn invalid_mp4_stream_selector_fails_before_creating_output() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.mp4");
    source(&input, false);
    let output = directory.path().join("output.mp4");
    let keys = InputKeys::default();
    let tracks = [LocalTrack {
        source: input,
        kind: TrackKind::Video,
        index: 42,
    }];
    let error = converter()
        .convert_tracks(TrackConversionRequest {
            tracks: &tracks,
            output: &output,
            format: OutputFormat::Mp4,
            target: TargetProtection::Clear,
            input_keys: &keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap_err();
    assert_eq!(error.0, "multi-track-mp4-output-not-supported");
    assert!(!output.exists());
}

fn mixed_duration_sources(root: &Path) -> [LocalTrack; 2] {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let video = root.join("video.mp4");
    let audio = root.join("audio.mp4");
    for (path, role) in [(&video, "0:v:0"), (&audio, "0:a:0")] {
        let mut command = std::process::Command::new("ffmpeg");
        command
            .args(["-v", "error", "-nostdin", "-i"])
            .arg(&fixture)
            .args(["-map", role, "-c", "copy"]);
        if role == "0:v:0" {
            command.args(["-t", "6"]);
        } else {
            command.args(["-movflags", "+frag_keyframe+empty_moov"]);
        }
        assert!(command.arg(path).output().unwrap().status.success());
    }
    [
        LocalTrack {
            source: video,
            kind: TrackKind::Video,
            index: 0,
        },
        LocalTrack {
            source: audio,
            kind: TrackKind::Audio,
            index: 0,
        },
    ]
}

fn complete_proof(proof: &drmr_core::conversion::VerificationReport) {
    assert!(
        proof.passed
            && (proof.checks.is_pass()
                && proof
                    .decode_evidence
                    .satisfies(drmr_core::conversion::ValidationDepth::Decoded, proof.scope))
    );
    assert_eq!(
        proof.checks.stream_metadata,
        drmr_core::conversion::CheckOutcome::Pass
    );
    assert!(
        proof.checks.packet_payloads_equal
            && proof.checks.packet_timing_equal
            && proof
                .decode_evidence
                .satisfies(drmr_core::conversion::ValidationDepth::Decoded, proof.scope)
    );
    assert!(!proof.license_playback_verified);
}

fn mixed_manifest_negatives(c: &Converter, request: VerificationRequest<'_>, expected: &str) {
    let original = std::fs::read_to_string(request.output.input).unwrap();
    let ns = expected
        .strip_prefix("PT")
        .unwrap()
        .strip_suffix('S')
        .unwrap()
        .replace('.', "")
        .parse::<u64>()
        .unwrap();
    for value in [6_000_000_000, ns - 1, ns + 1] {
        let bad = format!("PT{}.{:09}S", value / 1_000_000_000, value % 1_000_000_000);
        let altered = original.replace(expected, &bad);
        assert_ne!(altered, original);
        std::fs::write(request.output.input, altered).unwrap();
        assert_eq!(
            c.verify(request).unwrap_err().0,
            "dash-presentation-duration-mismatch"
        );
    }
    let doc = roxmltree::Document::parse(&original).unwrap();
    let template = doc
        .descendants()
        .find(|n| n.tag_name().name() == "SegmentTemplate")
        .unwrap();
    let mut altered = original.clone();
    if let Some(offset) = template.attribute_node("presentationTimeOffset") {
        assert_eq!(offset.value(), "0");
        altered.replace_range(offset.range_value(), "1");
    } else {
        let at = template.range().start + "<SegmentTemplate".len();
        altered.insert_str(at, " presentationTimeOffset=\"1\"");
    }
    std::fs::write(request.output.input, altered).unwrap();
    assert!(c.verify(request).is_err());
    let first = doc
        .descendants()
        .find(|n| n.tag_name().name() == "S")
        .unwrap();
    let start = first.attribute_node("t").unwrap();
    let mut altered = original.clone();
    let moved = (start.value().parse::<u64>().unwrap() + 1).to_string();
    altered.replace_range(start.range_value(), &moved);
    std::fs::write(request.output.input, altered).unwrap();
    assert!(c.verify(request).is_err());
    std::fs::write(request.output.input, &original).unwrap();
    complete_proof(&c.verify(request).unwrap());
}

fn complete_audio_end(path: &Path) -> String {
    let result = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "packet=pts,duration:stream=time_base",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .unwrap();
    assert!(result.status.success());
    let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(value["streams"][0]["time_base"], "1/48000");
    let packets = value["packets"].as_array().unwrap();
    assert_eq!(packets.len(), 470);
    assert_eq!(packets[0]["pts"], 0);
    let end = packets
        .iter()
        .map(|p| p["pts"].as_u64().unwrap() + p["duration"].as_u64().unwrap())
        .max()
        .unwrap();
    assert!(end > 480_000);
    let ns = (u128::from(end) * 1_000_000_000).div_ceil(48_000);
    format!("PT{}.{:09}S", ns / 1_000_000_000, ns % 1_000_000_000)
}

#[test]
fn progressive_video_and_longer_open_ended_audio_keep_the_complete_dash_presentation() {
    let owner = drmr_core::native::private_tempdir().unwrap();
    let root = owner.path();
    let mut tracks = mixed_duration_sources(root);
    let expected_end = complete_audio_end(&tracks[1].source);
    let originals = [
        std::fs::read(&tracks[0].source).unwrap(),
        std::fs::read(&tracks[1].source).unwrap(),
    ];
    for (index, bytes) in originals.iter().enumerate() {
        let boxes = drmr_core::mp4::parse_boxes(bytes).unwrap();
        let moov = boxes.iter().find(|b| b.kind() == *b"moov").unwrap();
        assert_eq!(
            moov.children.iter().any(|b| b.kind() == *b"mvex"),
            index == 1
        );
        if index == 1 {
            let track = moov.children.iter().find(|b| b.kind() == *b"trak").unwrap();
            assert!(!track.children.iter().any(|b| b.kind() == *b"edts"));
        }
    }
    let empty = InputKeys::default();
    let no_hls = HlsInputKeys::default();
    let output_key = OutputKey::from_hex(KID, KEY).unwrap();
    let output_keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
    let c = converter();
    let reference = root.join("reference");
    let request = TrackConversionRequest {
        tracks: &tracks,
        output: &reference,
        format: OutputFormat::Hls,
        target: TargetProtection::Clear,
        input_keys: &empty,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    };
    // This separately packaged reference checks both complete independent original tracks.
    complete_proof(&c.convert_tracks(request).unwrap().verification);
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        let output = root.join(target.as_str());
        complete_proof(
            &c.convert_tracks(TrackConversionRequest {
                output: &output,
                format: OutputFormat::Dash,
                target,
                output_key: (target != TargetProtection::Clear).then_some(&output_key),
                ..request
            })
            .unwrap()
            .verification,
        );
        let manifest = output.join("manifest.mpd");
        let fresh = VerificationRequest {
            source: VerificationSource {
                input: &reference.join("master.m3u8"),
                input_keys: &empty,
                hls_keys: &no_hls,
            },
            output: VerificationSource {
                input: &manifest,
                input_keys: &output_keys,
                hls_keys: &no_hls,
            },
        };
        complete_proof(&c.verify(fresh).unwrap());
        let text = std::fs::read_to_string(&manifest).unwrap();
        let doc = roxmltree::Document::parse(&text).unwrap();
        assert_eq!(
            doc.root_element().attribute("mediaPresentationDuration"),
            Some(expected_end.as_str())
        );
        mixed_manifest_negatives(&c, fresh, &expected_end);
    }
    // Representation order cannot turn a complete open-ended track into a finite edit.
    tracks.reverse();
    complete_proof(
        &c.convert_tracks(TrackConversionRequest {
            tracks: &tracks,
            output: &root.join("audio-first"),
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &empty,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap()
        .verification,
    );
    tracks.reverse();
    for (track, bytes) in tracks.iter().zip(&originals) {
        assert_eq!(std::fs::read(&track.source).unwrap(), *bytes);
    }
}
