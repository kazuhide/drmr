//! Adaptive HLS presentations retain every rendition, selection declaration, and original clock.
mod common;
use drmr_core::{
    conversion::{
        Aes128Key, ContentKey, ConversionRequest, Converter, HlsInputKeys, HlsOutputKey, InputKeys,
        OutputKey, SegmentIv, SourceValidationPurpose, VerificationRequest, VerificationSource,
    },
    events::NoSink,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::fmt::Write as _;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
const KID: &str = "000102030405060708090a0b0c0d0e0f";
const KEY: &str = "42424242424242424242424242424242";
const URI: &str = "https://operator.invalid/public-fixture";
fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(NoSink),
        )),
        "packager",
        "ffmpeg",
    )
}
fn run(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "fixture generation failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn fixture(root: &Path) -> PathBuf {
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut master = String::from(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"aac\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"2\",URI=\"audio/media.m3u8\"\n",
    );
    for (index, (width, height)) in [(160, 90), (224, 126), (256, 144), (320, 180)]
        .into_iter()
        .enumerate()
    {
        let dir = root.join(format!("video-{index}"));
        std::fs::create_dir_all(&dir).unwrap();
        run(Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(&clip)
            .args(["-map", "0:v:0", "-an", "-vf"])
            .arg(format!("scale={width}:{height}"))
            .args([
                "-c:v",
                "libx264",
                "-threads",
                "1",
                "-preset",
                "ultrafast",
                "-g",
                "48",
                "-keyint_min",
                "48",
                "-sc_threshold",
                "0",
                "-bf",
                "0",
                "-pix_fmt",
                "yuv420p",
                "-hls_time",
                "2",
                "-hls_playlist_type",
                "vod",
                "-hls_segment_filename",
            ])
            .arg(dir.join("segment-%d.ts"))
            .arg(dir.join("media.m3u8")));
        let media = std::fs::read_to_string(dir.join("media.m3u8")).unwrap();
        assert_eq!(media.matches("#EXTINF:2.000000,").count(), 5);
        writeln!(master,"#EXT-X-STREAM-INF:BANDWIDTH={},RESOLUTION={width}x{height},FRAME-RATE=24.000,AUDIO=\"aac\"\nvideo-{index}/media.m3u8", 200_000 * (index+1)).unwrap();
    }
    let audio = root.join("audio");
    std::fs::create_dir_all(&audio).unwrap();
    run(Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(&clip)
        .args([
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(audio.join("segment-%d.ts"))
        .arg(audio.join("media.m3u8")));
    let path = root.join("master.m3u8");
    std::fs::write(&path, master).unwrap();
    path
}
fn proof(result: &drmr_core::conversion::VerificationReport) {
    assert!(result.passed && result.media_verified);
    assert!(result.checks.stream_metadata.is_pass());
    assert!(
        result.checks.packet_payloads_equal
            && result.checks.packet_timing_equal
            && result.checks.decoded_content_equal
    );
}
fn assert_english_manifest(manifest: &Path, format: OutputFormat) {
    let text = std::fs::read_to_string(manifest).unwrap();
    assert!(
        text.contains("English")
            && text.contains(if format == OutputFormat::Dash {
                "lang=\"en\""
            } else {
                "LANGUAGE=\"en\""
            })
    );
    if matches!(format, OutputFormat::Hls | OutputFormat::HlsTs) {
        assert_eq!(text.matches("#EXT-X-STREAM-INF:").count(), 4);
        assert_eq!(text.matches("AUDIO=\"aac\"").count(), 4);
        assert!(text.contains("DEFAULT=YES,AUTOSELECT=YES"));
    }
}
#[test]
fn four_video_renditions_and_named_english_audio_convert_and_verify_independently() {
    let dir = tempfile::tempdir().unwrap();
    let input_root = dir.path().join("input");
    std::fs::create_dir(&input_root).unwrap();
    let input = fixture(&input_root);
    let original = common::snapshot_tree(&input_root);
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    let aes = HlsOutputKey::new(URI, Aes128Key::from_hex(KEY).unwrap()).unwrap();
    let sample = HlsOutputKey::new_with_iv(
        URI,
        Aes128Key::from_hex(KEY).unwrap(),
        SegmentIv::from_hex("20202020202020202020202020202020").unwrap(),
    )
    .unwrap();
    for (format, target) in [
        (OutputFormat::Dash, TargetProtection::Clear),
        (OutputFormat::Dash, TargetProtection::Cenc),
        (OutputFormat::Dash, TargetProtection::Cbcs),
        (OutputFormat::Hls, TargetProtection::Clear),
        (OutputFormat::Hls, TargetProtection::Cbcs),
        (OutputFormat::Hls, TargetProtection::HlsAes128),
        (OutputFormat::HlsTs, TargetProtection::Clear),
        (OutputFormat::HlsTs, TargetProtection::HlsAes128),
        (OutputFormat::HlsTs, TargetProtection::HlsSampleAes),
    ] {
        let output = dir
            .path()
            .join(format!("{}-{}", format.as_str(), target.as_str()));
        let result = converter()
            .convert(ConversionRequest {
                input: &input,
                output: &output,
                format,
                target,
                input_keys: &keys,
                hls_input_keys: &hls,
                output_key: matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs)
                    .then_some(&key),
                hls_key_uri: (target == TargetProtection::Cbcs).then_some(URI),
                hls_output_key: if target == TargetProtection::HlsSampleAes {
                    Some(&sample)
                } else {
                    (target == TargetProtection::HlsAes128).then_some(&aes)
                },
            })
            .unwrap();
        proof(&result.verification);
        assert_eq!(result.output.tracks.len(), 5);
        let manifest = output.join(if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        assert_english_manifest(&manifest, format);
        let output_keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
        let output_hls = HlsInputKeys::new_with_ivs(vec![(
            URI.into(),
            Aes128Key::from_hex(KEY).unwrap(),
            (target == TargetProtection::HlsSampleAes)
                .then(|| SegmentIv::from_hex("20202020202020202020202020202020").unwrap()),
        )])
        .unwrap();
        proof(
            &converter()
                .verify(VerificationRequest {
                    source: VerificationSource {
                        input: &input,
                        input_keys: &keys,
                        hls_keys: &hls,
                    },
                    output: VerificationSource {
                        input: &manifest,
                        input_keys: &output_keys,
                        hls_keys: &output_hls,
                    },
                })
                .unwrap(),
        );
        let qualified = converter()
            .validate_source(
                VerificationSource {
                    input: &manifest,
                    input_keys: &output_keys,
                    hls_keys: &output_hls,
                },
                SourceValidationPurpose::Verification,
            )
            .unwrap();
        assert_eq!(qualified.actual_format, format);
        assert_eq!(qualified.track_protection, vec![target; 5]);
        proof(&qualified.verification);
        println!(
            "DRMR_ABR_HLS_CELL {}",
            serde_json::json!({"format":format,"target":target,"video_renditions":4,"audio_renditions":1,"video_segments":5,"source_unchanged":common::snapshot_tree(&input_root)==original,"verification":result.verification})
        );
        assert_eq!(common::snapshot_tree(&input_root), original);
    }
}

#[test]
fn deep_admission_reports_actual_ts_format_and_never_admits_missing_media() {
    let directory = tempfile::tempdir().unwrap();
    let input = fixture(directory.path());
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    let source = VerificationSource {
        input: &input,
        input_keys: &keys,
        hls_keys: &hls,
    };
    let before = common::snapshot_tree(directory.path());
    let qualification = converter()
        .validate_source(source, SourceValidationPurpose::Conversion)
        .unwrap();
    assert_eq!(qualification.actual_format, OutputFormat::HlsTs);
    assert_eq!(
        qualification.track_protection,
        vec![TargetProtection::Clear; 5]
    );
    assert_eq!(
        qualification.eligible_formats,
        vec![OutputFormat::Dash, OutputFormat::Hls, OutputFormat::HlsTs]
    );
    proof(&qualification.verification);
    assert!(!qualification.input.media_verified);
    assert_eq!(common::snapshot_tree(directory.path()), before);
    std::fs::remove_file(directory.path().join("video-3/segment-4.ts")).unwrap();
    assert!(
        converter()
            .validate_source(source, SourceValidationPurpose::Conversion)
            .is_err()
    );
}

fn verify_clear(
    input: &Path,
    output: &Path,
) -> Result<drmr_core::conversion::VerificationReport, drmr_core::conversion::ConversionError> {
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    converter().verify(VerificationRequest {
        source: VerificationSource {
            input,
            input_keys: &keys,
            hls_keys: &hls,
        },
        output: VerificationSource {
            input: output,
            input_keys: &keys,
            hls_keys: &hls,
        },
    })
}
fn clear_output(input: &Path, output: &Path) {
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    proof(
        &converter()
            .convert(ConversionRequest {
                input,
                output,
                format: OutputFormat::Hls,
                target: TargetProtection::Clear,
                input_keys: &keys,
                hls_input_keys: &hls,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap()
            .verification,
    );
}
fn reversed_variants(original: &str) -> String {
    let mut header = Vec::new();
    let mut variants = Vec::new();
    let mut pending = None;
    for line in original.lines() {
        if line.starts_with("#EXT-X-STREAM-INF:") {
            pending = Some(line);
        } else if let Some(tag) = pending.take() {
            variants.push(format!("{tag}\n{line}"));
        } else {
            header.push(line);
        }
    }
    variants.reverse();
    format!("{}\n{}\n", header.join("\n"), variants.join("\n"))
}
#[test]
fn declaration_reordering_and_aliases_preserve_identity_but_semantic_changes_fail() {
    let directory = tempfile::tempdir().unwrap();
    let input_root = directory.path().join("input");
    std::fs::create_dir(&input_root).unwrap();
    let input = fixture(&input_root);
    let before = common::snapshot_tree(&input_root);
    let output = directory.path().join("output");
    clear_output(&input, &output);
    let manifest = output.join("master.m3u8");
    let original = std::fs::read_to_string(&manifest).unwrap();
    std::fs::write(&manifest, reversed_variants(&original)).unwrap();
    proof(&verify_clear(&input, &manifest).unwrap());
    std::fs::write(
        &manifest,
        original.replace("track-0000/media.m3u8", "track-0000/./media.m3u8"),
    )
    .unwrap();
    proof(&verify_clear(&input, &manifest).unwrap());
    for (label, changed, expected) in [
        (
            "name",
            original.replace("NAME=\"English\"", "NAME=\"Commentary\""),
            "presentation-metadata-mismatch",
        ),
        (
            "language",
            original.replace("LANGUAGE=\"en\"", "LANGUAGE=\"ja\""),
            "presentation-metadata-mismatch",
        ),
        (
            "default",
            original.replace("DEFAULT=YES", "DEFAULT=NO"),
            "presentation-metadata-mismatch",
        ),
        (
            "automatic",
            original.replace("AUTOSELECT=YES", "AUTOSELECT=NO"),
            "presentation-metadata-mismatch",
        ),
        (
            "group",
            original.replace("AUDIO=\"aac\"", "AUDIO=\"missing\""),
            "hls-audio-association-not-supported",
        ),
        (
            "role-alias",
            original.replace(
                "URI=\"track-0000/media.m3u8\"",
                "URI=\"track-0001/media.m3u8\"",
            ),
            "hls-audio-association-not-supported",
        ),
    ] {
        assert_ne!(
            changed, original,
            "mutation {label} did not change the fixture"
        );
        std::fs::write(&manifest, changed).unwrap();
        assert_eq!(
            verify_clear(&input, &manifest).unwrap_err().0,
            expected,
            "{label}"
        );
    }
    std::fs::write(&manifest, &original).unwrap();
    let mut lines = original.lines().map(str::to_string).collect::<Vec<_>>();
    let at = lines
        .iter()
        .position(|s| s.starts_with("#EXT-X-STREAM-INF:"))
        .unwrap();
    lines.drain(at..at + 2);
    std::fs::write(&manifest, format!("{}\n", lines.join("\n"))).unwrap();
    assert_eq!(
        verify_clear(&input, &manifest).unwrap_err().0,
        "presentation-track-count-mismatch"
    );
    std::fs::write(
        &manifest,
        original
            .replace("track-0001/media.m3u8", "SWAP")
            .replace("track-0002/media.m3u8", "track-0001/media.m3u8")
            .replace("SWAP", "track-0002/media.m3u8"),
    )
    .unwrap();
    assert_eq!(
        verify_clear(&input, &manifest).unwrap_err().0,
        "presentation-metadata-mismatch"
    );
    std::fs::write(&manifest, &original).unwrap();
    proof(&verify_clear(&input, &manifest).unwrap());
    assert_eq!(common::snapshot_tree(&input_root), before);
}

#[test]
fn sole_named_audio_with_omitted_selection_flags_is_retained_across_hls_and_dash() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("input");
    std::fs::create_dir(&root).unwrap();
    let input = fixture(&root);
    let original = std::fs::read_to_string(&input)
        .unwrap()
        .replace("NAME=\"English\"", "NAME=\"HLS Audio Media\"")
        .replace(",DEFAULT=YES,AUTOSELECT=YES", "");
    std::fs::write(&input, &original).unwrap();
    let before = common::snapshot_tree(&root);
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    for format in [OutputFormat::Dash, OutputFormat::Hls] {
        let output = directory.path().join(format.as_str());
        proof(
            &converter()
                .convert(ConversionRequest {
                    input: &input,
                    output: &output,
                    format,
                    target: TargetProtection::Clear,
                    input_keys: &keys,
                    hls_input_keys: &hls,
                    output_key: None,
                    hls_key_uri: None,
                    hls_output_key: None,
                })
                .unwrap()
                .verification,
        );
        let manifest = output.join(if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        let declaration = std::fs::read_to_string(&manifest).unwrap();
        assert!(declaration.contains("HLS Audio Media"));
        if format == OutputFormat::Hls {
            assert!(declaration.contains("DEFAULT=NO") && !declaration.contains("AUTOSELECT="));
        } else {
            assert!(
                declaration.contains("lang=\"en\"")
                    && declaration.contains("selectionPriority=\"1\"")
            );
        }
        proof(&verify_clear(&input, &manifest).unwrap());
    }
    assert_eq!(common::snapshot_tree(&root), before);
}
