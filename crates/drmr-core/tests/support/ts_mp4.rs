//! A standalone MP4 keeps the original TS access units, clocks and track set.
use super::*;
use drmr_core::conversion::{ContentKey, OutputKey, VerificationRequest, VerificationSource};

const KID: &str = "00112233445566778899aabbccddeeff";

#[cfg(unix)]
#[test]
fn mp4_track_jobs_seal_disjoint_private_output_roots() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let fixture = sample_aes_fixture::source(&root.path().join("input"), INPUT_KEY, INPUT_IV);
    let before = common::snapshot_tree(fixture.clear.parent().unwrap());
    let log = root.path().join("output-parents.txt");
    drop(drmr_core::native::create_private_new(&log).unwrap());
    let wrapper = root.path().join("observed-packager");
    common::write_mode(
        &wrapper,
        format!(
            r#"#!/usr/bin/env python3
import os, shutil, sys
if "--enable_raw_key_encryption" in sys.argv:
    descriptor = next(value for value in sys.argv[1:] if value.startswith("input="))
    fields = dict(value.split("=", 1) for value in descriptor.split(","))
    with open({}, "a", encoding="utf-8") as output:
        output.write(os.path.dirname(fields["output"]) + "\n")
packager = shutil.which("packager")
assert packager is not None
os.execv(packager, [packager, *sys.argv[1:]])
"#,
            serde_json::to_string(&log).unwrap()
        )
        .as_bytes(),
        0o700,
    );
    let command = Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(NoSink),
        )),
        wrapper.to_str().unwrap(),
        "unused-native-ts-mp4-ffmpeg",
    );
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let key = OutputKey::from_hex(KID, OUTPUT_KEY).unwrap();
    let output = root.path().join("result.mp4");
    let converted = command.convert(ConversionRequest {
        input: &fixture.clear,
        output: &output,
        format: OutputFormat::Mp4,
        target: TargetProtection::Cenc,
        input_keys: &empty,
        hls_input_keys: &hls,
        output_key: Some(&key),
        hls_key_uri: None,
        hls_output_key: None,
    });
    let text = std::fs::read_to_string(&log).unwrap();
    let parents: Vec<std::path::PathBuf> = text.lines().map(std::path::PathBuf::from).collect();
    assert_eq!(parents.len(), 2);
    assert!(
        !parents[0].starts_with(&parents[1]) && !parents[1].starts_with(&parents[0]),
        "per-track sealing must not walk another track's temporary workspace"
    );
    assert!(converted.unwrap().verification.passed);
    let result_keys = InputKeys::new(vec![ContentKey::from_hex(KID, OUTPUT_KEY).unwrap()]).unwrap();
    let verified = command
        .verify(VerificationRequest {
            source: VerificationSource {
                input: &fixture.clear,
                input_keys: &empty,
                hls_keys: &hls,
            },
            output: VerificationSource {
                input: &output,
                input_keys: &result_keys,
                hls_keys: &hls,
            },
        })
        .unwrap();
    assert!(verified.passed && verified.checks.packet_payloads_equal);
    assert!(verified.checks.packet_timing_equal && verified.checks.stream_metadata.is_pass());
    assert_eq!(
        common::snapshot_tree(fixture.clear.parent().unwrap()),
        before
    );
}

fn row(
    layout: &str,
    input_label: &str,
    source: &Path,
    input_keys: &HlsInputKeys,
    clear: &Path,
    root: &Path,
    counts: (Option<u64>, Option<u64>),
) {
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let c = Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(NoSink),
        )),
        "packager",
        "unused-native-ts-mp4-ffmpeg",
    );
    let key = OutputKey::from_hex(KID, OUTPUT_KEY).unwrap();
    let result_keys = InputKeys::new(vec![ContentKey::from_hex(KID, OUTPUT_KEY).unwrap()]).unwrap();
    let before = common::snapshot_tree(source.parent().unwrap());
    for target in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        let output = root.join(format!("{layout}-{input_label}-{}.mp4", target.as_str()));
        let encrypted = target != TargetProtection::Clear;
        let report = c
            .convert(ConversionRequest {
                input: source,
                output: &output,
                format: OutputFormat::Mp4,
                target,
                input_keys: &empty,
                hls_input_keys: input_keys,
                output_key: encrypted.then_some(&key),
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap_or_else(|error| {
                panic!("{layout}/{input_label}/{}: {}", target.as_str(), error.0)
            });
        assert!(report.verification.passed);
        assert_eq!(
            report.output.tracks.len(),
            usize::from(counts.0.is_some()) + usize::from(counts.1.is_some())
        );
        for (original, keys) in [(source, input_keys), (clear, &hls)] {
            let proof = c
                .verify(VerificationRequest {
                    source: VerificationSource {
                        input: original,
                        input_keys: &empty,
                        hls_keys: keys,
                    },
                    output: VerificationSource {
                        input: &output,
                        input_keys: if encrypted { &result_keys } else { &empty },
                        hls_keys: &hls,
                    },
                })
                .unwrap_or_else(|error| {
                    panic!(
                        "{layout}/{input_label}/{} verify: {}",
                        target.as_str(),
                        error.0
                    )
                });
            assert!(proof.passed);
            assert!(proof.checks.packet_payloads_equal && proof.checks.packet_timing_equal);
        }
        assert_eq!(common::snapshot_tree(source.parent().unwrap()), before);
    }
}

#[test]
fn video_ts_mp4_preserves_all_nine_protection_routes() {
    matrix_with("video", row);
}

#[test]
fn joint_ts_mp4_preserves_all_nine_protection_routes() {
    matrix_with("joint-av", row);
}

#[test]
fn audio_ts_mp4_preserves_all_nine_protection_routes() {
    matrix_with("audio", row);
}

#[test]
fn audio_44100_ts_mp4_preserves_all_nine_protection_routes() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let (clear, sample) = audio_sources_at_rate(root.path(), Some(44_100));
    let aes = aes_source(&clear, &root.path().join("aes"));
    let keys = HlsInputKeys::new_with_ivs(vec![(
        KEY_URI.into(),
        Aes128Key::from_hex(INPUT_KEY).unwrap(),
        Some(SegmentIv::from_hex(INPUT_IV).unwrap()),
    )])
    .unwrap();
    let empty = HlsInputKeys::default();
    let counts = original_counts(root.path(), &clear);
    for (name, input, input_keys) in [
        ("clear", &clear, &empty),
        ("aes", &aes, &keys),
        ("sample", &sample, &keys),
    ] {
        row(
            "audio44100",
            name,
            input,
            input_keys,
            &clear,
            root.path(),
            counts,
        );
    }
}

#[test]
fn ts_mp4_optional_decode_compares_all_selected_media() {
    let root = drmr_core::native::private_tempdir().unwrap();
    let fixture = sample_aes_fixture::source(&root.path().join("input"), INPUT_KEY, INPUT_IV);
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let output = root.path().join("result.mp4");
    let decoded = converter();
    let report = decoded
        .convert(ConversionRequest {
            input: &fixture.clear,
            output: &output,
            format: OutputFormat::Mp4,
            target: TargetProtection::Clear,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    all_checks(&report.verification);
    all_checks(
        &decoded
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: &fixture.clear,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                output: VerificationSource {
                    input: &output,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
            })
            .unwrap(),
    );
}

#[test]
fn ts_mp4_saved_plan_binds_source_output_and_independent_key_verification() {
    use drmr_core::conversion::{
        key_file::OperatorKeys,
        plan::{self, ExecutionPlanRequest},
    };
    let root = drmr_core::native::private_tempdir().unwrap();
    let fixture = sample_aes_fixture::source(&root.path().join("input"), INPUT_KEY, INPUT_IV);
    let mut keys = OperatorKeys::default();
    keys.output = Some(OutputKey::from_hex(KID, OUTPUT_KEY).unwrap());
    let command = converter()
        .with_decode(false)
        .with_ffprobe("unused-ts-mp4-ffprobe");
    let output = root.path().join("result.mp4");
    let intent = command
        .create_execution_plan(ExecutionPlanRequest {
            input: VerificationSource {
                input: &fixture.clear,
                input_keys: &keys.input,
                hls_keys: &keys.hls_input,
            },
            output: &output,
            format: Some(OutputFormat::Mp4),
            target: TargetProtection::Cenc,
        })
        .unwrap();
    let path = root.path().join("conversion.plan.json");
    plan::write_new(&path, &intent).unwrap();
    let before = std::fs::read(&path).unwrap();
    let saved = plan::read(&path).unwrap();
    assert!(
        command
            .execute_plan(&saved, &keys)
            .unwrap()
            .verification
            .passed
    );
    let verified = command.verify_plan(&saved, &keys).unwrap();
    assert!(
        verified.source_unchanged && verified.output_matches_plan && verified.verification.passed
    );
    let mut wrong = OperatorKeys::default();
    wrong.output = Some(OutputKey::from_hex(KID, INPUT_KEY).unwrap());
    let proof = command.verify_plan(&saved, &wrong);
    assert!(proof.is_err() || !proof.unwrap().verification.passed);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let mut changed = saved;
    changed.intent.target = TargetProtection::Cbcs;
    assert!(command.verify_plan(&changed, &keys).is_err());
}

#[test]
fn ts_mp4_refuses_ambiguous_video_selection_and_unrepresentable_language() {
    use drmr_core::conversion::{SourceValidationPurpose, plan::ExecutionPlanRequest};
    let root = drmr_core::native::private_tempdir().unwrap();
    let input = root.path().join("input");
    drmr_core::native::create_media_dir_all(&input).unwrap();
    shaka_source(&input.join("video"), "video", false);
    std::fs::copy(
        input.join("video/media.m3u8"),
        input.join("video/alias.m3u8"),
    )
    .unwrap();
    let ambiguous = input.join("multiple.m3u8");
    std::fs::write(&ambiguous,"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000,RESOLUTION=320x180\nvideo/media.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2000000,RESOLUTION=320x180\nvideo/alias.m3u8\n").unwrap();
    let audio = audio_sources(&input.join("audio")).0;
    let unsupported = input.join("regional.m3u8");
    assert_eq!(audio, input.join("audio/clear/media.m3u8"));
    std::fs::write(&unsupported,"#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"Main\",LANGUAGE=\"en-US\",URI=\"audio/clear/media.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=128000,AUDIO=\"a\",CODECS=\"mp4a.40.2\"\naudio/clear/media.m3u8\n").unwrap();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let command = converter().with_decode(false);
    for (source, code) in [
        (&ambiguous, "mp4-requires-one-video-and-audio-selection"),
        (&unsupported, "mp4-audio-language-not-supported"),
    ] {
        let output = root.path().join("must-not-publish.mp4");
        let admitted = command
            .validate_source(
                VerificationSource {
                    input: source,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                SourceValidationPurpose::Conversion,
            )
            .unwrap();
        assert!(!admitted.eligible_formats.contains(&OutputFormat::Mp4));
        assert_eq!(
            command
                .create_execution_plan(ExecutionPlanRequest {
                    input: VerificationSource {
                        input: source,
                        input_keys: &empty,
                        hls_keys: &hls
                    },
                    output: &output,
                    format: Some(OutputFormat::Mp4),
                    target: TargetProtection::Clear
                })
                .unwrap_err()
                .0,
            "execution-plan-not-supported"
        );
        assert_eq!(
            command
                .convert(ConversionRequest {
                    input: source,
                    output: &output,
                    format: OutputFormat::Mp4,
                    target: TargetProtection::Clear,
                    input_keys: &empty,
                    hls_input_keys: &hls,
                    output_key: None,
                    hls_key_uri: None,
                    hls_output_key: None
                })
                .unwrap_err()
                .0,
            code
        );
        assert!(!output.exists());
    }
}
