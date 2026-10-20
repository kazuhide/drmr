//! Adaptive HLS presentations retain every rendition, selection declaration, and original clock.
mod common;
use drmr_core::{
    conversion::{
        Aes128Key, ContentKey, ConversionRequest, Converter, HlsInputKeys, HlsOutputKey, InputKeys,
        OutputKey, SegmentIv, SourceProtection, SourceValidationPurpose, VerificationRequest,
        VerificationSource,
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
#[test]
fn only_uniform_source_protection_matches_a_single_output_target() {
    let targets = [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
        TargetProtection::HlsAes128,
        TargetProtection::HlsSampleAes,
    ];
    for target in targets {
        let source = SourceProtection::from(target);
        assert_eq!(source.as_str(), target.as_str());
        assert_eq!(serde_json::to_value(source).unwrap(), target.as_str());
        for expected in targets {
            assert_eq!(source.matches_target(expected), target == expected);
        }
        assert!(!SourceProtection::Mixed.matches_target(target));
    }
    assert_eq!(SourceProtection::Mixed.as_str(), "mixed");
    assert_eq!(
        serde_json::to_value(SourceProtection::Mixed).unwrap(),
        "mixed"
    );
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
        let protection = vec![SourceProtection::from(target); 5];
        assert_eq!(qualified.track_protection, protection);
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
        vec![SourceProtection::Clear; 5]
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

fn encrypt_first_segment(playlist: &Path) {
    let text = std::fs::read_to_string(playlist).unwrap();
    assert!(!text.contains("#EXT-X-KEY:"));
    let first = text
        .lines()
        .find(|l| !l.starts_with('#') && !l.is_empty())
        .unwrap();
    let segment = playlist.parent().unwrap().join(first);
    let cipher = playlist
        .parent()
        .unwrap()
        .join("encrypted-first-segment.tmp");
    drmr_core::conversion::encrypt_segment(
        &segment,
        &cipher,
        &Aes128Key::from_hex(KEY).unwrap(),
        &SegmentIv::for_sequence(0),
    )
    .unwrap();
    std::fs::rename(cipher, segment).unwrap();
    let mut mixed = String::new();
    let mut replaced = false;
    for line in text.lines() {
        if !replaced && line == first {
            writeln!(mixed, "#EXT-X-KEY:METHOD=AES-128,URI=\"{URI}\",IV=0x00000000000000000000000000000000\n{line}\n#EXT-X-KEY:METHOD=NONE").unwrap();
            replaced = true;
        } else {
            writeln!(mixed, "{line}").unwrap();
        }
    }
    assert!(replaced);
    std::fs::write(playlist, mixed).unwrap();
}
#[test]
fn mixed_clear_and_aes_segments_remain_admitted_and_report_the_actual_outer_protection() {
    let directory = tempfile::tempdir().unwrap();
    let input_root = directory.path().join("input");
    std::fs::create_dir(&input_root).unwrap();
    let input = fixture(&input_root);
    encrypt_first_segment(&input_root.join("video-0/media.m3u8"));
    let keys = InputKeys::default();
    let hls = HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEY).unwrap())]).unwrap();
    let original = common::snapshot_tree(&input_root);
    let source = VerificationSource {
        input: &input,
        input_keys: &keys,
        hls_keys: &hls,
    };
    let qualification = converter()
        .validate_source(source, SourceValidationPurpose::Conversion)
        .unwrap();
    assert_eq!(qualification.actual_format, OutputFormat::HlsTs);
    assert_eq!(
        qualification
            .track_protection
            .iter()
            .filter(|p| **p == SourceProtection::Mixed)
            .count(),
        1
    );
    proof(&qualification.verification);
    let output = directory.path().join("clear");
    let (plan, mut operator) = execute_mixed_clear_plan(&input, &output);
    proof(
        &converter()
            .verify(VerificationRequest {
                source,
                output: VerificationSource {
                    input: &output.join("master.m3u8"),
                    input_keys: &keys,
                    hls_keys: &HlsInputKeys::default(),
                },
            })
            .unwrap(),
    );
    let master = output.join("master.m3u8");
    let master_text = std::fs::read_to_string(&master).unwrap();
    let media = output.join(
        master_text
            .lines()
            .find(|l| !l.starts_with('#') && !l.is_empty())
            .unwrap(),
    );
    encrypt_first_segment(&media);
    let mixed = VerificationSource {
        input: &master,
        input_keys: &keys,
        hls_keys: &hls,
    };
    for purpose in [
        SourceValidationPurpose::Conversion,
        SourceValidationPurpose::Verification,
    ] {
        let result = converter().validate_source(mixed, purpose).unwrap();
        assert_eq!(result.actual_format, OutputFormat::Hls);
        assert_eq!(
            result
                .track_protection
                .iter()
                .filter(|p| **p == SourceProtection::Mixed)
                .count(),
            1
        );
        assert!(
            !result
                .track_protection
                .iter()
                .all(|p| p.matches_target(TargetProtection::Clear))
        );
        proof(&result.verification);
    }
    proof(
        &converter()
            .verify(VerificationRequest {
                source,
                output: mixed,
            })
            .unwrap(),
    );
    operator.hls_output = Some(HlsOutputKey::new(URI, Aes128Key::from_hex(KEY).unwrap()).unwrap());
    let result = converter().verify_plan(&plan, &operator).unwrap();
    assert!(result.source_unchanged && result.verification.passed);
    assert!(!result.output_matches_plan);
    assert_eq!(common::snapshot_tree(&input_root), original);
}

fn execute_mixed_clear_plan(
    input: &Path,
    output: &Path,
) -> (
    drmr_core::conversion::plan::ExecutionPlan,
    drmr_core::conversion::key_file::OperatorKeys,
) {
    use drmr_core::conversion::{key_file::OperatorKeys, plan::ExecutionPlanRequest};
    let mut keys = OperatorKeys::default();
    keys.hls_input =
        HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEY).unwrap())]).unwrap();
    let plan = converter()
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output,
            format: Some(OutputFormat::Hls),
            target: TargetProtection::Clear,
        })
        .unwrap();
    proof(&converter().execute_plan(&plan, &keys).unwrap().verification);
    (plan, keys)
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

#[test]
fn default_audio_keeps_an_omitted_autoselect_attribute() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("input");
    std::fs::create_dir(&root).unwrap();
    let input = fixture(&root);
    let master = std::fs::read_to_string(&input)
        .unwrap()
        .replace(",AUTOSELECT=YES", "");
    std::fs::write(&input, master).unwrap();
    let output = directory.path().join("output");
    clear_output(&input, &output);
    let manifest = output.join("master.m3u8");
    let declaration = std::fs::read_to_string(&manifest).unwrap();
    assert!(declaration.contains("DEFAULT=YES") && !declaration.contains("AUTOSELECT="));
    proof(&verify_clear(&input, &manifest).unwrap());
    std::fs::write(
        &manifest,
        declaration.replace("DEFAULT=YES", "DEFAULT=YES,AUTOSELECT=YES"),
    )
    .unwrap();
    assert_eq!(
        verify_clear(&input, &manifest).unwrap_err().0,
        "presentation-metadata-mismatch"
    );
}
#[test]
fn finite_joint_mp4_qualification_distinguishes_conversion_from_existing_output_verification() {
    let directory = tempfile::tempdir().unwrap();
    let clip = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    let input = VerificationSource {
        input: &clip,
        input_keys: &keys,
        hls_keys: &hls,
    };
    let qualification = converter()
        .validate_source(input, SourceValidationPurpose::Conversion)
        .unwrap();
    assert!(qualification.eligible_formats.contains(&OutputFormat::Mp4));
    proof(&qualification.verification);
    let output = directory.path().join("encrypted.mp4");
    let key = OutputKey::from_hex(KID, KEY).unwrap();
    proof(
        &converter()
            .convert_mp4(&clip, &output, TargetProtection::Cenc, &keys, Some(&key))
            .unwrap()
            .verification,
    );
    let output_keys = InputKeys::new(vec![ContentKey::from_hex(KID, KEY).unwrap()]).unwrap();
    let source = VerificationSource {
        input: &output,
        input_keys: &output_keys,
        hls_keys: &hls,
    };
    for purpose in [
        SourceValidationPurpose::Conversion,
        SourceValidationPurpose::Verification,
    ] {
        let result = converter().validate_source(source, purpose).unwrap();
        assert_eq!(result.actual_format, OutputFormat::Mp4);
        assert_eq!(result.track_protection, vec![SourceProtection::Cenc; 2]);
        assert!(!result.eligible_formats.contains(&OutputFormat::Mp4));
        proof(&result.verification);
    }
    assert_eq!(
        converter()
            .convert(ConversionRequest {
                input: &output,
                output: &directory.path().join("refused.mp4"),
                format: OutputFormat::Mp4,
                target: TargetProtection::Clear,
                input_keys: &output_keys,
                hls_input_keys: &hls,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None
            })
            .unwrap_err()
            .0,
        "single-track-mp4-required"
    );
}
fn tfdt_range(bytes: &[u8]) -> std::ops::Range<usize> {
    let boxes = drmr_core::mp4::parse_boxes(bytes).unwrap();
    let moof = boxes.iter().find(|n| n.kind() == *b"moof").unwrap();
    let traf = moof.children.iter().find(|n| n.kind() == *b"traf").unwrap();
    let tfdt = traf.children.iter().find(|n| n.kind() == *b"tfdt").unwrap();
    let payload = tfdt.header.payload(bytes);
    let width = if payload[0] == 1 { 8 } else { 4 };
    let start = tfdt.header.offset + tfdt.header.header_size + 4;
    start..start + width
}
#[test]
fn the_last_video_payload_audio_clock_and_segment_declarations_are_independent_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("input");
    std::fs::create_dir(&root).unwrap();
    let input = fixture(&root);
    let source_before = common::snapshot_tree(&root);
    let output = directory.path().join("output");
    clear_output(&input, &output);
    let manifest = output.join("master.m3u8");
    let last = output.join("track-0004/seg_5.m4s");
    let original = std::fs::read(&last).unwrap();
    let mut other = std::fs::read(output.join("track-0004/seg_4.m4s")).unwrap();
    let clock = original[tfdt_range(&original)].to_vec();
    let at = tfdt_range(&other);
    assert_eq!(at.len(), clock.len());
    other[at].copy_from_slice(&clock);
    assert_ne!(other, original);
    std::fs::write(&last, other).unwrap();
    assert_eq!(
        verify_clear(&input, &manifest).unwrap_err().0,
        "presentation-track-content-mismatch"
    );
    std::fs::write(&last, &original).unwrap();
    let audio = output.join("track-0000/seg_5.m4s");
    let original_audio = std::fs::read(&audio).unwrap();
    let mut changed = original_audio.clone();
    let range = tfdt_range(&changed);
    changed[range.end - 1] ^= 1;
    std::fs::write(&audio, changed).unwrap();
    assert_eq!(
        verify_clear(&input, &manifest).unwrap_err().0,
        "fragment-timeline-gap-or-overlap"
    );
    std::fs::write(&audio, original_audio).unwrap();
    let playlist = output.join("track-0004/media.m3u8");
    let text = std::fs::read_to_string(&playlist).unwrap();
    for (expected, changed) in [
        (
            "manifest-segment-duration-mismatch",
            text.replacen("#EXTINF:2.000,", "#EXTINF:3.000,", 1),
        ),
        (
            "fragment-timeline-gap-or-overlap",
            text.replace("seg_5.m4s", "seg_4.m4s"),
        ),
        (
            "hls-complete-vod-required",
            text.replace("#EXT-X-ENDLIST", ""),
        ),
    ] {
        assert_ne!(changed, text);
        std::fs::write(&playlist, changed).unwrap();
        assert_eq!(verify_clear(&input, &manifest).unwrap_err().0, expected);
    }
    std::fs::write(&playlist, text).unwrap();
    proof(&verify_clear(&input, &manifest).unwrap());
    assert_eq!(common::snapshot_tree(&root), source_before);
}

#[test]
fn deep_admission_rejects_a_channel_declaration_that_disagrees_with_actual_audio() {
    let directory = tempfile::tempdir().unwrap();
    let input = fixture(directory.path());
    let original = std::fs::read_to_string(&input).unwrap();
    assert!(original.contains("CHANNELS=\"2\""));
    std::fs::write(&input, original.replace("CHANNELS=\"2\"", "CHANNELS=\"1\"")).unwrap();
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    assert_eq!(
        converter()
            .validate_source(
                VerificationSource {
                    input: &input,
                    input_keys: &keys,
                    hls_keys: &hls
                },
                SourceValidationPurpose::Conversion
            )
            .unwrap_err()
            .0,
        "presentation-metadata-mismatch"
    );
}

#[test]
fn dash_labels_language_priority_and_common_time_origin_are_independently_checked() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("input");
    std::fs::create_dir(&root).unwrap();
    let input = fixture(&root);
    let before = common::snapshot_tree(&root);
    let output = directory.path().join("dash");
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    proof(
        &converter()
            .convert(ConversionRequest {
                input: &input,
                output: &output,
                format: OutputFormat::Dash,
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
    let manifest = output.join("manifest.mpd");
    let original = std::fs::read_to_string(&manifest).unwrap();
    for changed in [
        original.replace("<Label>English</Label>", "<Label>Commentary</Label>"),
        original.replace("lang=\"en\"", "lang=\"ja\""),
        original.replace("selectionPriority=\"2\"", "selectionPriority=\"1\""),
    ] {
        assert_ne!(changed, original);
        std::fs::write(&manifest, changed).unwrap();
        assert_eq!(
            verify_clear(&input, &manifest).unwrap_err().0,
            "presentation-metadata-mismatch"
        );
    }
    let document = roxmltree::Document::parse(&original).unwrap();
    let attribute = document
        .descendants()
        .find_map(|n| n.attribute_node("presentationTimeOffset"))
        .unwrap();
    let mut changed = original.clone();
    changed.replace_range(
        attribute.range_value(),
        &(attribute.value().parse::<u64>().unwrap() + 1).to_string(),
    );
    std::fs::write(&manifest, changed).unwrap();
    assert_eq!(
        verify_clear(&input, &manifest).unwrap_err().0,
        "dash-incoherent-presentation-offset"
    );
    std::fs::write(&manifest, original).unwrap();
    proof(&verify_clear(&input, &manifest).unwrap());
    assert_eq!(common::snapshot_tree(&root), before);
}
