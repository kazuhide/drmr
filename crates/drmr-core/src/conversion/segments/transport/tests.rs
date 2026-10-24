use super::*;
use crate::{
    concurrency::Stop,
    conversion::{Aes128Key, HlsInputKeys, HlsOutputKey, InputKeys, SegmentIv},
    events::NoSink,
    toolrun::{RunOptions, Secrets, ToolRunner},
};
use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

fn converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(NoSink),
        )),
        "native-must-not-use-packager",
        "native-must-not-use-ffmpeg",
    )
    .with_ffprobe("native-must-not-use-ffprobe")
}

fn tool(program: &str, arguments: Vec<String>) {
    let runner = ToolRunner::new(Arc::new(Secrets::default()), Arc::new(NoSink));
    let argv = std::iter::once(program.to_owned())
        .chain(arguments)
        .collect::<Vec<_>>();
    let shown = [program.to_owned(), "[fixture arguments redacted]".into()];
    let result = runner
        .run_tool(
            &argv,
            program,
            &RunOptions {
                timeout: Some(Duration::from_secs(30)),
                argv_redacted: Some(&shown),
                log_stdout: false,
                max_output_chars: 0,
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.returncode, Some(0), "required fixture tool failed");
}

fn fixture(directory: &Path, rate: u32, video: bool, audio: bool, irregular: bool) -> PathBuf {
    files::create_dir(directory).unwrap();
    let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut argv = vec![
        "-v".into(),
        "error".into(),
        "-nostdin".into(),
        "-threads".into(),
        "1".into(),
        "-i".into(),
        files::tool_path(&input).unwrap(),
    ];
    if video {
        argv.extend(["-map".into(), "0:v:0".into(), "-c:v".into()]);
        if irregular {
            argv.extend([
                "libx264".into(),
                "-preset".into(),
                "ultrafast".into(),
                "-threads:v".into(),
                "1".into(),
                "-g".into(),
                "240".into(),
                "-sc_threshold".into(),
                "0".into(),
                "-force_key_frames".into(),
                "0,1,4,7".into(),
            ]);
        } else {
            argv.push("copy".into());
        }
    } else {
        argv.push("-vn".into());
    }
    if audio {
        argv.extend([
            "-map".into(),
            "0:a:0".into(),
            "-c:a".into(),
            "aac".into(),
            "-ar".into(),
            rate.to_string(),
            "-threads:a".into(),
            "1".into(),
        ]);
    } else {
        argv.push("-an".into());
    }
    argv.extend([
        "-hls_time".into(),
        "2".into(),
        "-hls_playlist_type".into(),
        "vod".into(),
        "-hls_segment_filename".into(),
        files::tool_path(&directory.join("segment-%03d.ts")).unwrap(),
        files::tool_path(&directory.join("media.m3u8")).unwrap(),
    ]);
    tool("ffmpeg", argv);
    directory.join("media.m3u8")
}

fn output_key(sample: bool) -> HlsOutputKey {
    let key = Aes128Key::from_hex("000102030405060708090a0b0c0d0e0f").unwrap();
    if sample {
        HlsOutputKey::new_with_iv(
            "skd://synthetic",
            key,
            SegmentIv::from_hex("101112131415161718191a1b1c1d1e1f").unwrap(),
        )
        .unwrap()
    } else {
        HlsOutputKey::new("skd://synthetic", key).unwrap()
    }
}

fn hls_keys(sample: bool) -> HlsInputKeys {
    HlsInputKeys::new_with_ivs(vec![(
        "skd://synthetic".into(),
        Aes128Key::from_hex("000102030405060708090a0b0c0d0e0f").unwrap(),
        sample.then(|| SegmentIv::from_hex("101112131415161718191a1b1c1d1e1f").unwrap()),
    )])
    .unwrap()
}

fn load(converter: &Converter, path: &Path, keys: &HlsInputKeys) -> HlsSource {
    HlsSource::load(
        converter,
        VerificationSource {
            input: path,
            input_keys: &InputKeys::default(),
            hls_keys: keys,
        },
    )
    .unwrap()
}

fn pcr(path: &Path) -> Vec<[u8; 6]> {
    std::fs::read(path)
        .unwrap()
        .as_chunks::<188>()
        .0
        .iter()
        .filter(|packet| packet[3] & 0x20 != 0 && packet[4] >= 7 && packet[5] & 0x10 != 0)
        .map(|packet| packet[6..12].try_into().unwrap())
        .collect()
}

fn protected_inputs(
    converter: &Converter,
    root: &Path,
    original: &Path,
    clear: HlsInputKeys,
) -> Vec<(PathBuf, HlsInputKeys)> {
    let mut inputs = vec![(original.to_owned(), clear)];
    for target in [TargetProtection::HlsAes128, TargetProtection::HlsSampleAes] {
        let output = root.join(if target == TargetProtection::HlsAes128 {
            "aes-input"
        } else {
            "sample-input"
        });
        let key = output_key(target == TargetProtection::HlsSampleAes);
        let request = ConversionRequest {
            input: original,
            output: &output,
            format: OutputFormat::HlsTs,
            target,
            input_keys: &InputKeys::default(),
            hls_input_keys: &inputs[0].1,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: Some(&key),
        };
        assert!(
            convert(converter, request, load(converter, original, &inputs[0].1))
                .unwrap()
                .verification
                .passed
        );
        inputs.push((
            output.join("master.m3u8"),
            hls_keys(target == TargetProtection::HlsSampleAes),
        ));
    }
    inputs
}

fn assert_original_segments(original: &HlsSource, actual: &HlsSource) {
    assert_eq!(actual.parts.len(), original.parts.len());
    for (a, b) in original.parts.iter().zip(&actual.parts) {
        assert_eq!(a.media.resources.len(), b.media.resources.len());
        for ((a, duration_a), (b, duration_b)) in a.media.resources.iter().zip(&b.media.resources) {
            assert_eq!(duration_a, duration_b);
            assert_eq!(pcr(a), pcr(b));
        }
        for (pid, expected) in &a.media.native {
            let actual = b.media.native.get(pid).unwrap();
            assert!(
                expected
                    .evidence()
                    .unwrap()
                    .payload_equal(&actual.evidence().unwrap())
            );
            assert!(
                expected
                    .evidence()
                    .unwrap()
                    .timing_equal(&actual.evidence().unwrap())
            );
            assert_eq!(
                expected.segment_timings().unwrap(),
                actual.segment_timings().unwrap()
            );
        }
    }
}

#[test]
fn all_transport_encryptions_preserve_original_boundaries_clocks_and_native_content() {
    for (rate, video, irregular) in [
        (44_100, true, true),
        (48_000, true, false),
        (44_100, false, false),
        (48_000, false, false),
    ] {
        let root = crate::native::private_tempdir().unwrap();
        let original = fixture(&root.path().join("original"), rate, video, true, irregular);
        let converter = converter();
        let clear = HlsInputKeys::default();
        let original_source = load(&converter, &original, &clear);
        let original_bytes = original_source
            .parts
            .iter()
            .flat_map(|part| {
                part.media
                    .resources
                    .iter()
                    .map(|(path, _)| std::fs::read(path).unwrap())
            })
            .collect::<Vec<_>>();
        let inputs = protected_inputs(&converter, root.path(), &original, clear);
        for (input_index, (input, keys)) in inputs.iter().enumerate() {
            for (target_index, target) in [
                TargetProtection::Clear,
                TargetProtection::HlsAes128,
                TargetProtection::HlsSampleAes,
            ]
            .into_iter()
            .enumerate()
            {
                let output = root
                    .path()
                    .join(format!("output-{input_index}-{target_index}"));
                let key = output_key(target == TargetProtection::HlsSampleAes);
                let request = ConversionRequest {
                    input,
                    output: &output,
                    format: OutputFormat::HlsTs,
                    target,
                    input_keys: &InputKeys::default(),
                    hls_input_keys: keys,
                    output_key: None,
                    hls_key_uri: None,
                    hls_output_key: (target != TargetProtection::Clear).then_some(&key),
                };
                let result = convert(&converter, request, load(&converter, input, keys)).unwrap();
                assert!(result.verification.passed);
                assert!(
                    result
                        .verification
                        .native_manifest
                        .as_ref()
                        .unwrap()
                        .is_clean_for_scope()
                );
                assert!(
                    result
                        .processing
                        .as_ref()
                        .unwrap()
                        .required_tools
                        .is_empty()
                );
                let result_keys = if target == TargetProtection::Clear {
                    HlsInputKeys::default()
                } else {
                    hls_keys(target == TargetProtection::HlsSampleAes)
                };
                let actual = load(&converter, &output.join("master.m3u8"), &result_keys);
                assert_original_segments(&original_source, &actual);
                assert!(!output.join("combined.ts").exists());
                let value = serde_json::to_value(result.processing).unwrap();
                assert_eq!(value["mode"], "segments");
                assert_eq!(value["boundaries"], "preserve");
            }
        }
        let after = original_source
            .parts
            .iter()
            .flat_map(|part| {
                part.media
                    .resources
                    .iter()
                    .map(|(path, _)| std::fs::read(path).unwrap())
            })
            .collect::<Vec<_>>();
        assert_eq!(original_bytes, after);
        original_source.check_original().unwrap();
    }
}

#[test]
fn source_admission_refuses_declared_roles_dimensions_channels_and_duration_changes() {
    let root = crate::native::private_tempdir().unwrap();
    let original = fixture(&root.path().join("original"), 44_100, true, true, true);
    let converter = converter();
    let keys = HlsInputKeys::default();
    let mut source = load(&converter, &original, &keys);
    let input_keys = InputKeys::default();
    let input = VerificationSource {
        input: &original,
        input_keys: &input_keys,
        hls_keys: &keys,
    };
    let qualified = qualify(
        &converter,
        &source,
        input,
        SourceValidationPurpose::Conversion,
    )
    .unwrap();
    assert!(qualified.verification.passed);
    assert_eq!(qualified.actual_format, OutputFormat::HlsTs);
    source.parts[0].declaration.role = Some(TrackKind::Audio);
    assert_eq!(
        qualify(
            &converter,
            &source,
            input,
            SourceValidationPurpose::Conversion
        )
        .unwrap_err()
        .0,
        "ts-presentation-layout-not-supported"
    );
    source.parts[0].declaration.role = None;
    source.parts[0].declaration.metadata.resolution = Some((1, 1));
    assert_eq!(
        qualify(
            &converter,
            &source,
            input,
            SourceValidationPurpose::Conversion
        )
        .unwrap_err()
        .0,
        "presentation-metadata-mismatch"
    );
    source.parts[0].declaration.metadata.resolution = None;
    source.parts[0].media.resources[0].1 = "0.000001".into();
    assert!(
        qualify(
            &converter,
            &source,
            input,
            SourceValidationPurpose::Conversion
        )
        .is_err()
    );
}

#[test]
fn cancelled_or_existing_destinations_never_publish_partial_output() {
    let root = crate::native::private_tempdir().unwrap();
    let original = fixture(&root.path().join("original"), 44_100, false, true, false);
    let converter = converter();
    let keys = HlsInputKeys::default();
    let output = root.path().join("foreign");
    files::create_dir(&output).unwrap();
    std::fs::write(output.join("keep"), b"foreign").unwrap();
    let input_keys = InputKeys::default();
    let request = ConversionRequest {
        input: &original,
        output: &output,
        format: OutputFormat::HlsTs,
        target: TargetProtection::Clear,
        input_keys: &input_keys,
        hls_input_keys: &keys,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: None,
    };
    assert!(convert(&converter, request, load(&converter, &original, &keys)).is_err());
    assert_eq!(std::fs::read(output.join("keep")).unwrap(), b"foreign");
    let source = load(&converter, &original, &keys);
    let stop = Stop::new();
    stop.set();
    let cancelled = converter.with_stop(stop);
    let output = root.path().join("cancelled");
    assert_eq!(
        convert(
            &cancelled,
            ConversionRequest {
                output: &output,
                ..request
            },
            source
        )
        .unwrap_err()
        .0,
        "cancelled"
    );
    assert!(!output.exists());
}

#[test]
fn separate_audio_retains_language_selection_resolution_and_channels() {
    let root = crate::native::private_tempdir().unwrap();
    let original = root.path().join("original");
    files::create_dir(&original).unwrap();
    let video = fixture(&original.join("video"), 48_000, true, false, true);
    fixture(&original.join("audio"), 44_100, false, true, false);
    let converter = converter();
    let empty = HlsInputKeys::default();
    let video_source = load(&converter, &video, &empty);
    let metadata = video_source.parts[0]
        .media
        .native
        .values()
        .next()
        .unwrap()
        .metadata()
        .unwrap();
    let ts::canonical::TrackMetadata::Video(video) = metadata else {
        panic!("video fixture required")
    };
    let master = original.join("master.m3u8");
    std::fs::write(&master, format!("#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"stereo\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"2\",URI=\"audio/media.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,CODECS=\"avc1.{:02x}{:02x}{:02x},mp4a.40.2\",RESOLUTION={}x{},AUDIO=\"stereo\"\nvideo/media.m3u8\n", video.profile_idc, video.constraint_flags, video.level_idc, video.width, video.height)).unwrap();
    let source = load(&converter, &master, &empty);
    let output = root.path().join("encrypted");
    let key = output_key(true);
    let input_keys = InputKeys::default();
    let request = ConversionRequest {
        input: &master,
        output: &output,
        format: OutputFormat::HlsTs,
        target: TargetProtection::HlsSampleAes,
        input_keys: &input_keys,
        hls_input_keys: &empty,
        output_key: None,
        hls_key_uri: None,
        hls_output_key: Some(&key),
    };
    let report = convert(&converter, request, load(&converter, &master, &empty)).unwrap();
    assert!(report.verification.passed);
    let keys = hls_keys(true);
    let output_manifest = output.join("master.m3u8");
    let result = load(&converter, &output_manifest, &keys);
    let report = verify(
        &converter,
        &source,
        &result,
        VerificationRequest {
            source: VerificationSource {
                input: &master,
                input_keys: &input_keys,
                hls_keys: &empty,
            },
            output: VerificationSource {
                input: &output_manifest,
                input_keys: &input_keys,
                hls_keys: &keys,
            },
        },
    )
    .unwrap();
    assert!(report.passed);
    let master = std::fs::read_to_string(output_manifest).unwrap();
    assert!(master.contains(
        "GROUP-ID=\"stereo\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES"
    ));
    assert!(master.contains("CHANNELS=\"2\""));
    assert!(master.contains(&format!(
        "RESOLUTION={}x{},AUDIO=\"stereo\"",
        video.width, video.height
    )));
    let mut changed = load(&converter, request.input, &empty);
    changed
        .parts
        .iter_mut()
        .find_map(|part| part.declaration.metadata.audio.as_mut())
        .unwrap()
        .channels = Some("1".into());
    assert_eq!(
        qualify(
            &converter,
            &changed,
            VerificationSource {
                input: request.input,
                input_keys: request.input_keys,
                hls_keys: request.hls_input_keys
            },
            SourceValidationPurpose::Conversion
        )
        .unwrap_err()
        .0,
        "presentation-metadata-mismatch"
    );
}

#[test]
fn equal_whole_track_content_cannot_hide_changed_resource_membership() {
    let root = crate::native::private_tempdir().unwrap();
    let original = fixture(&root.path().join("original"), 48_000, true, true, false);
    let converter = converter();
    let keys = HlsInputKeys::default();
    let source = load(&converter, &original, &keys);
    let resources = &source.parts[0].media.resources;
    assert!(resources.len() >= 3);
    let directory = root.path().join("changed");
    files::create_dir(&directory).unwrap();
    let mut merged = std::fs::read(&resources[0].0).unwrap();
    merged.extend_from_slice(&std::fs::read(&resources[1].0).unwrap());
    std::fs::write(directory.join("merged.ts"), merged).unwrap();
    let mut manifest = "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:4\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:4,\nmerged.ts\n".to_owned();
    for (index, (path, duration)) in resources.iter().enumerate().skip(2) {
        let name = format!("rest-{index}.ts");
        std::fs::copy(path, directory.join(&name)).unwrap();
        writeln!(manifest, "#EXTINF:{duration},\n{name}").unwrap();
    }
    manifest.push_str("#EXT-X-ENDLIST\n");
    let output = directory.join("media.m3u8");
    std::fs::write(&output, manifest).unwrap();
    let result = load(&converter, &output, &keys);
    for (pid, first) in &source.parts[0].media.native {
        let second = result.parts[0].media.native.get(pid).unwrap();
        assert!(
            first
                .evidence()
                .unwrap()
                .payload_equal(&second.evidence().unwrap())
        );
        assert!(
            first
                .evidence()
                .unwrap()
                .timing_equal(&second.evidence().unwrap())
        );
    }
    let input_keys = InputKeys::default();
    let report = verify(
        &converter,
        &source,
        &result,
        VerificationRequest {
            source: VerificationSource {
                input: &original,
                input_keys: &input_keys,
                hls_keys: &keys,
            },
            output: VerificationSource {
                input: &output,
                input_keys: &input_keys,
                hls_keys: &keys,
            },
        },
    )
    .unwrap();
    assert!(!report.passed);
    assert!(!report.checks.packet_payloads_equal);
    assert!(!report.checks.packet_timing_equal);
}

#[test]
fn explicit_decode_adds_complete_source_and_output_evidence() {
    for video in [false, true] {
        let root = crate::native::private_tempdir().unwrap();
        let original = fixture(&root.path().join("original"), 44_100, video, true, video);
        let converter = Converter::new(
            Arc::new(ToolRunner::new(
                Arc::new(Secrets::default()),
                Arc::new(NoSink),
            )),
            "packager",
            "ffmpeg",
        )
        .with_decode(true);
        let empty = HlsInputKeys::default();
        let input_keys = InputKeys::default();
        let input = VerificationSource {
            input: &original,
            input_keys: &input_keys,
            hls_keys: &empty,
        };
        let source = load(&converter, &original, &empty);
        let admitted = qualify(
            &converter,
            &source,
            input,
            SourceValidationPurpose::Conversion,
        )
        .unwrap();
        assert_eq!(
            admitted.verification.decode_evidence,
            super::super::super::DecodeEvidence::SourceDecoded
        );
        let output = root.path().join("encrypted");
        let key = output_key(true);
        let report = convert(
            &converter,
            ConversionRequest {
                input: &original,
                output: &output,
                format: OutputFormat::HlsTs,
                target: TargetProtection::HlsSampleAes,
                input_keys: &input_keys,
                hls_input_keys: &empty,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: Some(&key),
            },
            source,
        )
        .unwrap();
        assert!(report.verification.passed);
        assert_eq!(
            report.verification.decode_evidence,
            super::super::super::DecodeEvidence::ContentCompared { equal: true }
        );
        let processing = report.processing.unwrap();
        assert!(
            processing
                .required_tools
                .contains(&inspection::RequiredTool::Ffmpeg)
        );
        assert!(
            processing
                .required_tools
                .contains(&inspection::RequiredTool::Ffprobe)
        );
        assert!(
            processing
                .required_tools
                .contains(&inspection::RequiredTool::ShakaPackager)
        );
    }
}
