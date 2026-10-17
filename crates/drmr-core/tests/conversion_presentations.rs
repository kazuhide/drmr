//! Joint presentations verify each original track, including full audio decode and timing.
#![cfg(unix)]
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
