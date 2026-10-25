//! Native defaults check full source/output evidence without external inspection tools.
mod common;
#[path = "support/native_presentation_mutations.rs"]
mod mutations;
use drmr_core::{
    conversion::{
        Aes128Key, ContentKey, ConversionRequest, Converter, DecodeEvidence, HlsInputKeys,
        HlsOutputKey, InputKeys, OutputKey, SegmentIv, SourceValidationPurpose,
        VerificationRequest, VerificationSource,
    },
    events::Collector,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}
fn keys() -> InputKeys {
    InputKeys::new(vec![
        ContentKey::from_hex(
            "000102030405060708090a0b0c0d0e0f",
            "101112131415161718191a1b1c1d1e1f",
        )
        .unwrap(),
        ContentKey::from_hex(
            "303132333435363738393a3b3c3d3e3f",
            "202122232425262728292a2b2c2d2e2f",
        )
        .unwrap(),
    ])
    .unwrap()
}
fn converter() -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(runner, "packager", "must-not-run-ffmpeg"),
        events,
    )
}
#[test]
fn progressive_clear_cenc_both_iv_widths_and_cbcs_preserve_original_evidence() {
    let (converter, events) = converter();
    let keys = keys();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let clear = fixture("clip.mp4");
    for name in [
        "clip.mp4",
        "cenc-iv8.mp4",
        "cenc-iv16.mp4",
        "cbcs-joint.mp4",
    ] {
        let input = fixture(name);
        let before = std::fs::read(&input).unwrap();
        let input_keys = if name == "clip.mp4" { &empty } else { &keys };
        let admitted = converter
            .validate_source(
                VerificationSource {
                    input: &input,
                    input_keys,
                    hls_keys: &hls,
                },
                SourceValidationPurpose::Conversion,
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(admitted.verification.passed, "{name}");
        assert_eq!(
            admitted.verification.decode_evidence,
            DecodeEvidence::not_requested()
        );
        for output in [&input, &clear] {
            let report = converter
                .verify(VerificationRequest {
                    source: VerificationSource {
                        input: &input,
                        input_keys,
                        hls_keys: &hls,
                    },
                    output: VerificationSource {
                        input: output,
                        input_keys: if output == &clear { &empty } else { input_keys },
                        hls_keys: &hls,
                    },
                })
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(report.passed, "{name}: {report:?}");
            assert_eq!(report.decode_evidence, DecodeEvidence::not_requested());
        }
        assert_eq!(std::fs::read(input).unwrap(), before);
    }
    assert!(
        events
            .records()
            .iter()
            .all(|record| record.get("stage").and_then(serde_json::Value::as_str) != Some("tool"))
    );
}

#[test]
fn muxed_transport_source_has_native_admission_with_original_segment_checks() {
    let root = common::private_tempdir().unwrap();
    let playlist = root.path().join("media.m3u8");
    let output = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture("clip.mp4"))
        .args([
            "-map",
            "0",
            "-c",
            "copy",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(root.path().join("segment-%d.ts"))
        .arg(&playlist)
        .output()
        .unwrap();
    assert!(output.status.success(), "fixture generation failed");
    let (converter, events) = converter();
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    let (report, measurements) = converter.measure(|converter| {
        converter.validate_source(
            VerificationSource {
                input: &playlist,
                input_keys: &keys,
                hls_keys: &hls,
            },
            SourceValidationPurpose::Conversion,
        )
    });
    let report = report.unwrap();
    assert!(report.verification.passed);
    assert_eq!(
        report.verification.decode_evidence,
        DecodeEvidence::not_requested()
    );
    assert_eq!(report.track_protection.len(), 2);
    let segment_bytes: u64 = std::fs::read_dir(root.path())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "ts"))
        .map(|path| path.metadata().unwrap().len())
        .sum();
    assert!(segment_bytes > 0);
    let observed = serde_json::to_value(measurements).unwrap();
    let staged_bytes = observed["workspace"]["checkpoint_regular_file_bytes_max"]
        .as_u64()
        .unwrap();
    assert!(
        staged_bytes < 2 * segment_bytes,
        "native source checks materialized a redundant joined transport"
    );
    assert!(
        events
            .records()
            .iter()
            .all(|record| record.get("stage").and_then(serde_json::Value::as_str) != Some("tool"))
    );
    // A duration error remains a refusal even when every media byte is intact.
    let text = std::fs::read_to_string(&playlist).unwrap();
    std::fs::write(
        &playlist,
        text.replacen("#EXTINF:2.000000,", "#EXTINF:2.001000,", 1),
    )
    .unwrap();
    assert!(
        converter
            .validate_source(
                VerificationSource {
                    input: &playlist,
                    input_keys: &keys,
                    hls_keys: &hls
                },
                SourceValidationPurpose::Conversion
            )
            .is_err()
    );
}

const MATRIX_KID: &str = "505152535455565758595a5b5c5d5e5f";
const MATRIX_KEY: &str = "606162636465666768696a6b6c6d6e6f";
const MATRIX_IV: &str = "707172737475767778797a7b7c7d7e7f";
const MATRIX_URI: &str = "https://operator.invalid/native-fixture";

fn prepare_muxed_transport(root: &Path) -> PathBuf {
    let directory = root.join("source");
    std::fs::create_dir(&directory).unwrap();
    let source = directory.join("media.m3u8");
    let generated = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture("clip.mp4"))
        .args([
            "-map",
            "0",
            "-c",
            "copy",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(directory.join("segment-%d.ts"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(generated.status.success());
    source
}

fn hls_material(target: TargetProtection) -> (Option<HlsOutputKey>, HlsInputKeys) {
    let key = match target {
        TargetProtection::HlsAes128 => {
            Some(HlsOutputKey::new(MATRIX_URI, Aes128Key::from_hex(MATRIX_KEY).unwrap()).unwrap())
        }
        TargetProtection::HlsSampleAes => Some(
            HlsOutputKey::new_with_iv(
                MATRIX_URI,
                Aes128Key::from_hex(MATRIX_KEY).unwrap(),
                SegmentIv::from_hex(MATRIX_IV).unwrap(),
            )
            .unwrap(),
        ),
        _ => None,
    };
    let keys = if key.is_some() {
        HlsInputKeys::new_with_ivs(vec![(
            MATRIX_URI.into(),
            Aes128Key::from_hex(MATRIX_KEY).unwrap(),
            (target == TargetProtection::HlsSampleAes)
                .then(|| SegmentIv::from_hex(MATRIX_IV).unwrap()),
        )])
        .unwrap()
    } else {
        HlsInputKeys::default()
    };
    (key, keys)
}

fn verify_matrix_output(
    converter: &Converter,
    source: VerificationSource<'_>,
    output: VerificationSource<'_>,
) {
    let admitted = converter
        .validate_source(output, SourceValidationPurpose::Conversion)
        .unwrap();
    assert!(admitted.verification.passed);
    let checked = converter
        .verify(VerificationRequest { source, output })
        .unwrap();
    assert!(checked.passed, "{checked:?}");
    assert_eq!(checked.decode_evidence, DecodeEvidence::not_requested());
    assert_eq!(checked.native_manifest.as_ref().unwrap().errors, 0);
}

fn presentation_mutations(
    converter: &Converter,
    source: VerificationSource<'_>,
    output: VerificationSource<'_>,
    init: &Path,
) {
    let original = std::fs::read(init).unwrap();
    for changed in [
        mutations::rotated(&original),
        mutations::cropped_window(&original),
    ]
    .into_iter()
    .chain(mutations::wrong_header_durations(&original))
    .chain(mutations::changed_track_controls(&original))
    {
        assert_ne!(changed, original);
        std::fs::write(init, changed).unwrap();
        let negative = converter.verify(VerificationRequest { source, output });
        assert!(
            !negative.is_ok_and(|report| report.passed),
            "changed presentation controls must not pass with unchanged samples"
        );
    }
    std::fs::write(init, &original).unwrap();
    assert!(
        converter
            .verify(VerificationRequest { source, output })
            .unwrap()
            .passed
    );
}

fn open_edit_mutations(
    converter: &Converter,
    source: VerificationSource<'_>,
    output: VerificationSource<'_>,
    init: &Path,
) {
    let original = std::fs::read(init).unwrap();
    for changed in [
        mutations::shifted_open_window(&original),
        mutations::without_edits(&original),
    ] {
        assert_ne!(changed, original);
        std::fs::write(init, changed).unwrap();
        assert!(
            !converter
                .verify(VerificationRequest { source, output })
                .is_ok_and(|r| r.passed),
            "an altered clock mapping must fail with unchanged sample clocks"
        );
    }
    std::fs::write(init, original).unwrap();
    assert!(
        converter
            .verify(VerificationRequest { source, output })
            .unwrap()
            .passed
    );
}

fn recovery_mutations(
    converter: &Converter,
    source: VerificationSource<'_>,
    output: VerificationSource<'_>,
    init: &Path,
) {
    let original = std::fs::read(init).unwrap();
    for changed in mutations::changed_recovery_groups(&original) {
        std::fs::write(init, changed).unwrap();
        let admitted = converter
            .validate_source(output, SourceValidationPurpose::Conversion)
            .unwrap();
        assert!(
            admitted.verification.passed,
            "a legal original recovery declaration is not a preservation proof"
        );
        assert!(
            !converter
                .verify(VerificationRequest { source, output })
                .is_ok_and(|r| r.passed)
        );
    }
    std::fs::write(init, original).unwrap();
    assert!(
        converter
            .verify(VerificationRequest { source, output })
            .unwrap()
            .passed
    );
}

#[test]
fn native_transport_and_fragmented_protection_matrix_uses_only_packaging_tools() {
    let root = common::private_tempdir().unwrap();
    let source = prepare_muxed_transport(root.path());
    let directory = source.parent().unwrap();
    let original = common::snapshot_tree(directory);
    let (converter, events) = converter();
    let empty = InputKeys::default();
    let no_hls = HlsInputKeys::default();
    let output_key = OutputKey::from_hex(MATRIX_KID, MATRIX_KEY).unwrap();
    let output_keys =
        InputKeys::new(vec![ContentKey::from_hex(MATRIX_KID, MATRIX_KEY).unwrap()]).unwrap();
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
        let (hls_key, hls_keys) = hls_material(target);
        let output = root
            .path()
            .join(format!("{}-{}", format.as_str(), target.as_str()));
        let report = converter
            .convert(ConversionRequest {
                input: &source,
                output: &output,
                format,
                target,
                input_keys: &empty,
                hls_input_keys: &no_hls,
                output_key: matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs)
                    .then_some(&output_key),
                hls_key_uri: (target == TargetProtection::Cbcs && format == OutputFormat::Hls)
                    .then_some(MATRIX_URI),
                hls_output_key: hls_key.as_ref(),
            })
            .unwrap_or_else(|error| panic!("{} {}: {error}", format.as_str(), target.as_str()));
        assert!(report.verification.passed);
        assert_eq!(
            report.verification.decode_evidence,
            DecodeEvidence::not_requested()
        );
        let entry = output.join(if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        let input_keys = if matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs) {
            &output_keys
        } else {
            &empty
        };
        let source_view = VerificationSource {
            input: &source,
            input_keys: &empty,
            hls_keys: &no_hls,
        };
        let result_view = VerificationSource {
            input: &entry,
            input_keys,
            hls_keys: &hls_keys,
        };
        verify_matrix_output(&converter, source_view, result_view);
        if format == OutputFormat::Dash && target == TargetProtection::Clear {
            presentation_mutations(
                &converter,
                source_view,
                result_view,
                &output.join("track-0000/init.mp4"),
            );
            presentation_mutations(
                &converter,
                source_view,
                result_view,
                &output.join("track-0001/init.mp4"),
            );
            recovery_mutations(
                &converter,
                source_view,
                result_view,
                &output.join("track-0000/init.mp4"),
            );
        }
        assert_eq!(common::snapshot_tree(directory), original);
    }
    for record in events
        .records()
        .iter()
        .filter(|record| record.get("stage").and_then(serde_json::Value::as_str) == Some("tool"))
    {
        assert!(
            !["ffmpeg", "ffprobe"]
                .iter()
                .any(|tool| record.get("tool").and_then(serde_json::Value::as_str) == Some(*tool)),
            "{record:?}"
        );
    }
}

#[test]
fn progressive_media_and_track_controls_are_compared_without_decoding() {
    let root = common::private_tempdir().unwrap();
    let source = fixture("clip.mp4");
    let result = root.path().join("result.mp4");
    let original = std::fs::read(&source).unwrap();
    let (converter, _) = converter();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let source = VerificationSource {
        input: &source,
        input_keys: &empty,
        hls_keys: &hls,
    };
    let output = VerificationSource {
        input: &result,
        input_keys: &empty,
        hls_keys: &hls,
    };
    for changed in mutations::changed_track_controls(&original) {
        std::fs::write(&result, changed).unwrap();
        assert!(
            !converter
                .verify(VerificationRequest { source, output })
                .is_ok_and(|r| r.passed)
        );
    }
    std::fs::write(&result, original).unwrap();
    assert!(
        converter
            .verify(VerificationRequest { source, output })
            .unwrap()
            .passed
    );
}

#[test]
fn non_square_pixel_transport_preserves_b_frame_display_and_clock_evidence() {
    let root = common::private_tempdir().unwrap();
    let source_dir = root.path().join("source");
    std::fs::create_dir(&source_dir).unwrap();
    let source = source_dir.join("media.m3u8");
    let generated = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture("clip.mp4"))
        .args([
            "-map",
            "0:v:0",
            "-an",
            "-vf",
            "setsar=4/3",
            "-c:v",
            "libx264",
            "-threads",
            "1",
            "-g",
            "48",
            "-keyint_min",
            "48",
            "-sc_threshold",
            "0",
            "-bf",
            "2",
            "-pix_fmt",
            "yuv420p",
            "-hls_time",
            "2",
            "-hls_playlist_type",
            "vod",
            "-hls_segment_filename",
        ])
        .arg(source_dir.join("segment-%d.ts"))
        .arg(&source)
        .output()
        .unwrap();
    assert!(generated.status.success());
    let (converter, _) = converter();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let output = root.path().join("result");
    let report = converter
        .convert(ConversionRequest {
            input: &source,
            output: &output,
            format: OutputFormat::Dash,
            target: TargetProtection::Clear,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    assert!(report.verification.passed);
    let source_view = VerificationSource {
        input: &source,
        input_keys: &empty,
        hls_keys: &hls,
    };
    let manifest = output.join("manifest.mpd");
    let result_view = VerificationSource {
        input: &manifest,
        input_keys: &empty,
        hls_keys: &hls,
    };
    open_edit_mutations(
        &converter,
        source_view,
        result_view,
        &output.join("track-0000/init.mp4"),
    );
    let decoded = Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::new()),
            Arc::new(Collector::default()),
        )),
        "packager",
        "ffmpeg",
    )
    .with_decode(true)
    .verify(VerificationRequest {
        source: source_view,
        output: result_view,
    })
    .unwrap();
    assert!(decoded.passed, "{decoded:?}");
    assert_eq!(
        decoded.decode_evidence,
        DecodeEvidence::ContentCompared { equal: true }
    );
}
