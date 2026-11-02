//! Finite fragmented video/AAC reimport remains narrower than arbitrary multi-track MP4.
mod common;

use drmr_core::{
    conversion::{
        ContentKey, ConversionRequest, Converter, HlsInputKeys, InputKeys, OutputKey,
        SourceValidationPurpose, VerificationRequest, VerificationSource,
    },
    events::Collector,
    inspection::{OutputFormat, TargetProtection},
    mp4,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

const VIDEO_KID: &str = "000102030405060708090a0b0c0d0e0f";
const AUDIO_KID: &str = "303132333435363738393a3b3c3d3e3f";
const KEY: &str = "101112131415161718191a1b1c1d1e1f";

fn command() -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(runner, "packager", "absent-ffmpeg").with_ffprobe("absent-ffprobe"),
        events,
    )
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}

fn joint(path: &Path, third_track: bool) {
    let mut ffmpeg = std::process::Command::new("ffmpeg");
    ffmpeg
        .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
        .arg(fixture("hevc-main.mp4"))
        .arg("-i")
        .arg(fixture("clip.mp4"))
        .args([
            "-map",
            "0:v:0",
            "-map",
            "1:a:0",
            "-c",
            "copy",
            "-map_metadata",
            "-1",
        ]);
    if third_track {
        ffmpeg.args([
            "-map",
            "1:a:0",
            "-movflags",
            "empty_moov+frag_keyframe+default_base_moof",
        ]);
    }
    assert!(ffmpeg.arg(path).status().unwrap().success());
}

fn convert(
    command: &Converter,
    source: &Path,
    output: &Path,
    keys: &InputKeys,
    target: TargetProtection,
) -> Result<drmr_core::conversion::ConversionReport, drmr_core::conversion::ConversionError> {
    let key = OutputKey::from_hex(VIDEO_KID, KEY).unwrap();
    command.convert(ConversionRequest {
        input: source,
        output,
        format: OutputFormat::Mp4,
        target,
        input_keys: keys,
        hls_input_keys: &HlsInputKeys::default(),
        output_key: (target != TargetProtection::Clear).then_some(&key),
        hls_key_uri: None,
        hls_output_key: None,
    })
}

fn role_keys() -> InputKeys {
    InputKeys::new(vec![
        ContentKey::from_hex(VIDEO_KID, KEY).unwrap(),
        ContentKey::from_hex(AUDIO_KID, KEY).unwrap(),
    ])
    .unwrap()
}

fn audio_nodes(bytes: &[u8]) -> Vec<mp4::Mp4Box> {
    let nodes = mp4::parse_boxes(bytes).unwrap();
    let track = nodes
        .iter()
        .find(|node| node.kind() == *b"moov")
        .unwrap()
        .children
        .iter()
        .filter(|node| node.kind() == *b"trak")
        .find(|track| {
            mp4::walk(std::slice::from_ref(track)).iter().any(|node| {
                node.kind() == *b"hdlr" && node.header.payload(bytes).get(8..12) == Some(b"soun")
            })
        })
        .unwrap();
    mp4::walk(std::slice::from_ref(track))
        .into_iter()
        .cloned()
        .collect()
}

fn distinct_audio_id(bytes: &mut [u8]) {
    let nodes = audio_nodes(bytes);
    let encryption = nodes.iter().find(|node| node.kind() == *b"tenc").unwrap();
    let at = encryption.header.offset + encryption.header.header_size;
    assert_eq!(bytes[at], 0);
    bytes[at + 8..at + 24].copy_from_slice(&hex::decode(AUDIO_KID).unwrap());
    // Generic source PSSH is optional; remove it while retaining all sample addresses.
    let tree = mp4::parse_boxes(bytes).unwrap();
    for node in mp4::walk(&tree)
        .into_iter()
        .filter(|node| node.kind() == *b"pssh")
    {
        let at = node.header.offset;
        bytes[at + 4..at + 8].copy_from_slice(b"free");
        bytes[at + node.header.header_size..at + node.header.size].fill(0);
    }
}

fn assert_no_commands(events: &Collector) {
    assert!(
        !events
            .records()
            .iter()
            .any(
                |record| record.get("event").and_then(serde_json::Value::as_str) == Some("command")
            )
    );
}

fn refuse_without_tools(source: &Path, output: &Path, keys: &InputKeys, invalid_source: bool) {
    let (denied, events) = command();
    if invalid_source {
        assert!(
            denied
                .validate_source(
                    VerificationSource {
                        input: source,
                        input_keys: keys,
                        hls_keys: &HlsInputKeys::default(),
                    },
                    SourceValidationPurpose::Conversion
                )
                .is_err()
        );
    }
    assert!(convert(&denied, source, output, keys, TargetProtection::Clear).is_err());
    assert!(!output.exists());
    assert_no_commands(&events);
}

#[test]
fn fragmented_joint_reimport_requires_each_role_key_finite_windows_and_exact_track_shape() {
    let owner = common::private_tempdir().unwrap();
    let progressive = owner.path().join("progressive.mp4");
    joint(&progressive, false);
    let protected = owner.path().join("protected.mp4");
    convert(
        &command().0,
        &progressive,
        &protected,
        &InputKeys::default(),
        TargetProtection::Cenc,
    )
    .unwrap();
    let mut bytes = fs::read(&protected).unwrap();
    distinct_audio_id(&mut bytes);
    let source = owner.path().join("two-keys.mp4");
    fs::write(&source, &bytes).unwrap();
    let keys = role_keys();
    let (valid, _) = command();
    let qualification = valid
        .validate_source(
            VerificationSource {
                input: &source,
                input_keys: &keys,
                hls_keys: &HlsInputKeys::default(),
            },
            SourceValidationPurpose::Conversion,
        )
        .unwrap();
    assert!(qualification.eligible_formats.contains(&OutputFormat::Mp4));
    let clear = owner.path().join("clear.mp4");
    assert!(
        convert(&valid, &source, &clear, &keys, TargetProtection::Clear)
            .unwrap()
            .verification
            .passed
    );

    let (denied, events) = command();
    let output = owner.path().join("unused-output-key.mp4");
    let unused_key = OutputKey::from_hex(VIDEO_KID, KEY).unwrap();
    assert_eq!(
        denied
            .convert_mp4(
                &source,
                &output,
                TargetProtection::Clear,
                &keys,
                Some(&unused_key)
            )
            .unwrap_err()
            .0,
        "output-key-not-used"
    );
    assert!(!output.exists());
    assert_no_commands(&events);

    let missing = InputKeys::new(vec![ContentKey::from_hex(VIDEO_KID, KEY).unwrap()]).unwrap();
    refuse_without_tools(
        &source,
        &owner.path().join("missing-key.mp4"),
        &missing,
        true,
    );

    let mut changed = bytes.clone();
    let nodes = audio_nodes(&changed);
    let edit = nodes.iter().find(|node| node.kind() == *b"elst").unwrap();
    let at = edit.header.offset + edit.header.header_size;
    assert_eq!(changed[at], 1);
    changed[at + 8..at + 16].copy_from_slice(&1_000_000_000_u64.to_be_bytes());
    let altered = owner.path().join("outside-window.mp4");
    fs::write(&altered, &changed).unwrap();
    refuse_without_tools(
        &altered,
        &owner.path().join("outside-window-output.mp4"),
        &keys,
        true,
    );

    let third = owner.path().join("three-tracks.mp4");
    joint(&third, true);
    refuse_without_tools(
        &third,
        &owner.path().join("three-track-output.mp4"),
        &InputKeys::default(),
        false,
    );
    assert_eq!(fs::read(source).unwrap(), bytes);
    assert_eq!(fs::read(altered).unwrap(), changed);
}

fn presentation_roundtrips(
    video: &str,
    rate: Option<u32>,
    targets: &[TargetProtection],
    audio_first: bool,
) {
    let owner = common::private_tempdir().unwrap();
    let progressive = owner.path().join("progressive.mp4");
    if let Some(rate) = rate {
        let mut generate = std::process::Command::new("ffmpeg");
        generate
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(fixture(video))
            .arg("-i")
            .arg(fixture("clip.mp4"));
        let maps = if audio_first {
            ["1:a:0", "0:v:0"]
        } else {
            ["0:v:0", "1:a:0"]
        };
        for map in maps {
            generate.args(["-map", map]);
        }
        assert!(
            generate
                .args([
                    "-map_metadata",
                    "-1",
                    "-c:v",
                    "copy",
                    "-c:a",
                    "aac",
                    "-ar",
                    &rate.to_string(),
                    "-threads",
                    "1",
                    "-metadata:s:a:0",
                    "language=eng"
                ])
                .arg(&progressive)
                .status()
                .unwrap()
                .success()
        );
    } else {
        joint(&progressive, false);
    }
    let source = owner.path().join("source.mp4");
    let (command, events) = command();
    convert(
        &command,
        &progressive,
        &source,
        &InputKeys::default(),
        TargetProtection::Clear,
    )
    .unwrap();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let output_key = OutputKey::from_hex(VIDEO_KID, KEY).unwrap();
    let input_keys = role_keys();
    for (ordinal, (format, protection)) in [
        (OutputFormat::Dash, TargetProtection::Clear),
        (OutputFormat::Hls, TargetProtection::Clear),
        (OutputFormat::Dash, TargetProtection::Cenc),
        (OutputFormat::Hls, TargetProtection::Cenc),
        (OutputFormat::Dash, TargetProtection::Cbcs),
        (OutputFormat::Hls, TargetProtection::Cbcs),
    ]
    .into_iter()
    .enumerate()
    {
        if !targets.contains(&protection) {
            continue;
        }
        let presentation = owner.path().join(format!("presentation-{ordinal}"));
        command
            .convert(ConversionRequest {
                input: &source,
                output: &presentation,
                format,
                target: protection,
                input_keys: &empty,
                hls_input_keys: &hls,
                output_key: (protection != TargetProtection::Clear).then_some(&output_key),
                hls_key_uri: (format == OutputFormat::Hls && protection != TargetProtection::Clear)
                    .then_some("https://operator.invalid/joint-key"),
                hls_output_key: None,
            })
            .unwrap();
        let entry = presentation.join(if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        let keys = if protection == TargetProtection::Clear {
            &empty
        } else {
            &input_keys
        };
        let qualification = command
            .validate_source(
                VerificationSource {
                    input: &entry,
                    input_keys: keys,
                    hls_keys: &hls,
                },
                SourceValidationPurpose::Conversion,
            )
            .unwrap();
        assert!(
            qualification.eligible_formats.contains(&OutputFormat::Mp4),
            "{format:?}/{protection:?}"
        );
        for (index, target) in targets.iter().copied().enumerate() {
            let output = owner.path().join(format!("result-{ordinal}-{index}.mp4"));
            let report = convert(&command, &entry, &output, keys, target)
                .unwrap_or_else(|error| panic!("{format:?}/{protection:?}->{target:?}: {error:?}"));
            assert!(report.verification.passed);
            assert_eq!(report.output.tracks.len(), 2);
            let result_keys = if target == TargetProtection::Clear {
                &empty
            } else {
                &input_keys
            };
            assert!(
                command
                    .verify(VerificationRequest {
                        source: VerificationSource {
                            input: &entry,
                            input_keys: keys,
                            hls_keys: &hls
                        },
                        output: VerificationSource {
                            input: &output,
                            input_keys: result_keys,
                            hls_keys: &hls
                        },
                    })
                    .unwrap()
                    .passed
            );
            assert!(
                command
                    .verify(VerificationRequest {
                        source: VerificationSource {
                            input: &source,
                            input_keys: &empty,
                            hls_keys: &hls
                        },
                        output: VerificationSource {
                            input: &output,
                            input_keys: result_keys,
                            hls_keys: &hls
                        },
                    })
                    .unwrap()
                    .passed
            );
        }
    }
    assert!(!events.records().iter().any(|record| {
        serde_json::to_string(record)
            .unwrap()
            .contains("absent-ffmpeg")
            || serde_json::to_string(record)
                .unwrap()
                .contains("absent-ffprobe")
    }));
}

#[test]
fn fragmented_audio_video_presentations_return_to_one_mp4_without_decoders() {
    presentation_roundtrips(
        "hevc-main.mp4",
        None,
        &[
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ],
        false,
    );
}

#[test]
fn avc_aac_44100_and_48000_presentations_preserve_every_sample_and_window() {
    for rate in [44100, 48000] {
        presentation_roundtrips(
            "clip.mp4",
            Some(rate),
            &[
                TargetProtection::Clear,
                TargetProtection::Cenc,
                TargetProtection::Cbcs,
            ],
            false,
        );
    }
}

#[test]
fn main10_audio_first_presentations_recover_the_container_projection() {
    presentation_roundtrips(
        "hevc-main10.mp4",
        Some(44100),
        &[
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ],
        true,
    );
}

#[test]
fn vp9_presentations_return_to_clear_and_cenc_mp4_without_decoders() {
    presentation_roundtrips(
        "vp9-p0-8.mp4",
        Some(48000),
        &[TargetProtection::Clear, TargetProtection::Cenc],
        false,
    );
}

#[test]
fn standalone_mp4_never_discards_custom_or_contradictory_manifest_metadata() {
    let owner = common::private_tempdir().unwrap();
    let progressive = owner.path().join("progressive.mp4");
    joint(&progressive, false);
    let mut bytes = fs::read(&progressive).unwrap();
    let header = audio_nodes(&bytes)
        .into_iter()
        .find(|node| node.kind() == *b"mdhd")
        .unwrap();
    let offset = if header.header.payload(&bytes)[0] == 0 {
        20
    } else {
        32
    };
    let at = header.header.offset + header.header.header_size + offset;
    let language = b"eng"
        .iter()
        .fold(0u16, |value, byte| (value << 5) | u16::from(byte - 0x60));
    bytes[at..at + 2].copy_from_slice(&language.to_be_bytes());
    fs::write(&progressive, bytes).unwrap();
    let source = owner.path().join("source.mp4");
    let (command, events) = command();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    convert(
        &command,
        &progressive,
        &source,
        &empty,
        TargetProtection::Clear,
    )
    .unwrap();
    for format in [OutputFormat::Dash, OutputFormat::Hls] {
        let directory = owner.path().join(format!("metadata-{format:?}"));
        command
            .convert(ConversionRequest {
                input: &source,
                output: &directory,
                format,
                target: TargetProtection::Clear,
                input_keys: &empty,
                hls_input_keys: &hls,
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
            .unwrap();
        let entry = directory.join(if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        });
        let original = fs::read_to_string(&entry).unwrap();
        let edits: &[(&str, &str)] = if format == OutputFormat::Hls {
            &[
                ("NAME=\"stream_1\"", "NAME=\"Commentary\""),
                ("default-audio-group", "custom-audio-group"),
                ("DEFAULT=NO", "DEFAULT=YES"),
                ("AUTOSELECT=YES", "AUTOSELECT=NO"),
                ("CHANNELS=\"2\"", "CHANNELS=\"1\""),
                ("RESOLUTION=320x180", "RESOLUTION=321x180"),
            ]
        } else {
            &[
                ("<Label>stream_1</Label>", "<Label>Commentary</Label>"),
                ("selectionPriority=\"1\"", "selectionPriority=\"2\""),
            ]
        };
        let mut edits = edits.to_vec();
        let language_attribute = if format == OutputFormat::Hls {
            "LANGUAGE=\""
        } else {
            "lang=\""
        };
        let at = original
            .find(language_attribute)
            .expect("declared language");
        let end = at
            + language_attribute.len()
            + original[at + language_attribute.len()..].find('"').unwrap();
        let language = &original[at..=end];
        let wrong_language = if format == OutputFormat::Hls {
            "LANGUAGE=\"jpn\""
        } else {
            "lang=\"jpn\""
        };
        edits.push((language, wrong_language));
        for (index, (from, to)) in edits.iter().enumerate() {
            assert!(original.contains(from), "missing declared field {from}");
            fs::write(&entry, original.replace(from, to)).unwrap();
            let source_view = VerificationSource {
                input: &entry,
                input_keys: &empty,
                hls_keys: &hls,
            };
            let qualified =
                command.validate_source(source_view, SourceValidationPurpose::Conversion);
            assert!(
                qualified.is_err()
                    || !qualified
                        .unwrap()
                        .eligible_formats
                        .contains(&OutputFormat::Mp4),
                "{from}: invalid MP4 admission"
            );
            let output = owner.path().join(format!("refused-{format:?}-{index}.mp4"));
            assert!(
                convert(&command, &entry, &output, &empty, TargetProtection::Clear).is_err(),
                "{from}"
            );
            assert!(!output.exists());
            assert!(
                command
                    .verify(VerificationRequest {
                        source: source_view,
                        output: VerificationSource {
                            input: &source,
                            input_keys: &empty,
                            hls_keys: &hls
                        },
                    })
                    .is_err(),
                "{from}: invalid standalone metadata comparison"
            );
            fs::write(&entry, &original).unwrap();
        }
    }
    for record in events.records() {
        if record.get("event").and_then(serde_json::Value::as_str) == Some("command") {
            assert_eq!(
                record.get("tool").and_then(serde_json::Value::as_str),
                Some("packager")
            );
        }
    }
}
