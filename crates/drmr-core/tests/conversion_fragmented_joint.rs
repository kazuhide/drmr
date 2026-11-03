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

fn roundtrip_fixture(progressive: &Path, video: &str, rate: Option<u32>, audio_first: bool) {
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
                .arg(progressive)
                .status()
                .unwrap()
                .success()
        );
    } else {
        joint(progressive, false);
    }
}

fn assert_cbcs_reimport(
    command: &Converter,
    output: &Path,
    reimported: &Path,
    format: OutputFormat,
    result_keys: &InputKeys,
    video: &str,
    rate: Option<u32>,
) {
    let hls = HlsInputKeys::default();
    let empty = InputKeys::default();
    let report = command
        .convert(ConversionRequest {
            input: output,
            output: reimported,
            format,
            target: TargetProtection::Clear,
            input_keys: result_keys,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap_or_else(|error| panic!("CBCS MP4 reimport {video}/{rate:?}/{format:?}: {error:?}"));
    assert!(report.verification.passed);
    let returned = reimported.join(if format == OutputFormat::Dash {
        "manifest.mpd"
    } else {
        "master.m3u8"
    });
    assert!(
        command
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: output,
                    input_keys: result_keys,
                    hls_keys: &hls
                },
                output: VerificationSource {
                    input: &returned,
                    input_keys: &empty,
                    hls_keys: &hls
                },
            })
            .unwrap()
            .passed
    );
}

fn assert_mp4_return_proofs(
    command: &Converter,
    source: &Path,
    entry: &Path,
    output: &Path,
    keys: &InputKeys,
    result_keys: &InputKeys,
) {
    let hls = HlsInputKeys::default();
    let empty = InputKeys::default();
    assert!(
        command
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: entry,
                    input_keys: keys,
                    hls_keys: &hls
                },
                output: VerificationSource {
                    input: output,
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
                    input: source,
                    input_keys: &empty,
                    hls_keys: &hls
                },
                output: VerificationSource {
                    input: output,
                    input_keys: result_keys,
                    hls_keys: &hls
                },
            })
            .unwrap()
            .passed
    );
}

fn assert_mp4_admitted(command: &Converter, source: VerificationSource<'_>, context: &str) {
    let qualification = command
        .validate_source(source, SourceValidationPurpose::Conversion)
        .unwrap();
    assert!(
        qualification.eligible_formats.contains(&OutputFormat::Mp4),
        "{context}"
    );
}

fn presentation_roundtrips(
    video: &str,
    rate: Option<u32>,
    targets: &[TargetProtection],
    audio_first: bool,
) {
    let owner = common::private_tempdir().unwrap();
    let progressive = owner.path().join("progressive.mp4");
    roundtrip_fixture(&progressive, video, rate, audio_first);
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
        assert_mp4_admitted(
            &command,
            VerificationSource {
                input: &entry,
                input_keys: keys,
                hls_keys: &hls,
            },
            &format!("{format:?}/{protection:?}"),
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
            if target == TargetProtection::Cbcs {
                assert_cbcs_reimport(
                    &command,
                    &output,
                    &owner.path().join(format!("reimport-{ordinal}-{index}")),
                    format,
                    result_keys,
                    video,
                    rate,
                );
            }

            assert_mp4_return_proofs(&command, &source, &entry, &output, keys, result_keys);
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

fn assert_audio_first_declaration(entry: &Path, format: OutputFormat) {
    let manifest = fs::read_to_string(entry).unwrap();
    let expected = if format == OutputFormat::Dash {
        "<Label>stream_0</Label>"
    } else {
        "NAME=\"stream_0\""
    };
    assert!(manifest.contains(expected), "{format:?}: {expected}");
}

fn audio_first_manifest(
    command: &Converter,
    source: &Path,
    output: &Path,
    format: OutputFormat,
    protection: TargetProtection,
) -> PathBuf {
    let key = OutputKey::from_hex(VIDEO_KID, KEY).unwrap();
    command
        .convert(ConversionRequest {
            input: source,
            output,
            format,
            target: protection,
            input_keys: &InputKeys::default(),
            hls_input_keys: &HlsInputKeys::default(),
            output_key: (protection != TargetProtection::Clear).then_some(&key),
            hls_key_uri: (format == OutputFormat::Hls && protection != TargetProtection::Clear)
                .then_some("https://operator.invalid/joint-key"),
            hls_output_key: None,
        })
        .unwrap();
    let entry = output.join(if format == OutputFormat::Dash {
        "manifest.mpd"
    } else {
        "master.m3u8"
    });
    assert_audio_first_declaration(&entry, format);
    entry
}

fn assert_audio_first_manifest_return(
    command: &Converter,
    source: VerificationSource<'_>,
    output: &Path,
    returned: &Path,
    format: OutputFormat,
    mp4_protection: TargetProtection,
    manifest_protection: TargetProtection,
) {
    let converted = convert(
        command,
        source.input,
        output,
        source.input_keys,
        mp4_protection,
    )
    .unwrap();
    assert!(converted.verification.passed);
    let keys = if mp4_protection == TargetProtection::Clear {
        InputKeys::default()
    } else {
        role_keys()
    };
    let key = OutputKey::from_hex(VIDEO_KID, KEY).unwrap();
    let hls = HlsInputKeys::default();
    command
        .convert(ConversionRequest {
            input: output,
            output: returned,
            format,
            target: manifest_protection,
            input_keys: &keys,
            hls_input_keys: &hls,
            output_key: (manifest_protection != TargetProtection::Clear).then_some(&key),
            hls_key_uri: (format == OutputFormat::Hls
                && manifest_protection != TargetProtection::Clear)
                .then_some("https://operator.invalid/joint-key"),
            hls_output_key: None,
        })
        .unwrap();
    let entry = returned.join(if format == OutputFormat::Dash {
        "manifest.mpd"
    } else {
        "master.m3u8"
    });
    let returned_keys = if manifest_protection == TargetProtection::Clear {
        InputKeys::default()
    } else {
        role_keys()
    };
    let verified = command.verify(VerificationRequest {
        source,
        output: VerificationSource { input: &entry, input_keys: &returned_keys, hls_keys: &hls },
    }).unwrap_or_else(|error| panic!("original manifest {format:?}/{manifest_protection:?} via MP4/{mp4_protection:?}: {error:?}"));
    assert!(verified.passed);
    assert_audio_first_declaration(&entry, format);
    assert_eq!(
        converted.output.tracks[0].kind,
        drmr_core::inspection::TrackKind::Audio
    );
}

#[test]
fn audio_first_canonical_manifests_preserve_selection_through_mp4_and_back() {
    let owner = common::private_tempdir().unwrap();
    let source = owner.path().join("audio-first.mp4");
    roundtrip_fixture(&source, "hevc-main10.mp4", Some(44100), true);
    assert_eq!(
        drmr_core::inspection::inspect(&source).unwrap().tracks[0].kind,
        drmr_core::inspection::TrackKind::Audio
    );
    let (command, _) = command();
    let command = command.with_max_workers(Some(1)).unwrap();
    let protections = [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ];
    let hls = HlsInputKeys::default();
    for format in [OutputFormat::Dash, OutputFormat::Hls] {
        for protection in protections {
            let entry = audio_first_manifest(
                &command,
                &source,
                &owner
                    .path()
                    .join(format!("source-{format:?}-{protection:?}")),
                format,
                protection,
            );
            let keys = if protection == TargetProtection::Clear {
                InputKeys::default()
            } else {
                role_keys()
            };
            let original = VerificationSource {
                input: &entry,
                input_keys: &keys,
                hls_keys: &hls,
            };
            assert_mp4_admitted(&command, original, "audio-first canonical manifest");
            for target in protections {
                assert_audio_first_manifest_return(
                    &command,
                    original,
                    &owner
                        .path()
                        .join(format!("result-{format:?}-{protection:?}-{target:?}.mp4")),
                    &owner
                        .path()
                        .join(format!("returned-{format:?}-{protection:?}-{target:?}")),
                    format,
                    target,
                    protection,
                );
            }
        }
    }
}

fn reverse_container_track_order(input: &Path, output: &Path) {
    let mut bytes = fs::read(input).unwrap();
    let tree = mp4::parse_boxes(&bytes).unwrap();
    let movie = tree.iter().find(|node| node.kind() == *b"moov").unwrap();
    let mut tracks: Vec<_> = movie
        .children
        .iter()
        .filter(|node| node.kind() == *b"trak")
        .map(|node| node.header.bytes(&bytes).to_vec())
        .collect();
    assert_eq!(tracks.len(), 2);
    tracks.reverse();
    let mut tracks = tracks.into_iter();
    let mut payload = Vec::new();
    for child in &movie.children {
        if child.kind() == *b"trak" {
            payload.extend(tracks.next().unwrap());
        } else {
            payload.extend(child.header.bytes(&bytes));
        }
    }
    let at = movie.header.offset + movie.header.header_size;
    assert_eq!(payload.len(), movie.header.size - movie.header.header_size);
    bytes[at..at + payload.len()].copy_from_slice(&payload);
    fs::write(output, bytes).unwrap();
    assert_eq!(
        drmr_core::inspection::inspect(output).unwrap().tracks[0].kind,
        drmr_core::inspection::TrackKind::Video
    );
}

fn decoded_converter() -> Converter {
    Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::new()),
            Arc::new(Collector::default()),
        )),
        "packager",
        "ffmpeg",
    )
    .with_ffprobe("ffprobe")
    .with_decode(true)
    .with_max_workers(Some(1))
    .unwrap()
}

#[test]
fn independent_verification_rejects_a_container_order_that_changes_canonical_selection() {
    let owner = common::private_tempdir().unwrap();
    let source = owner.path().join("audio-first.mp4");
    roundtrip_fixture(&source, "hevc-main10.mp4", Some(44100), true);
    let (command, _) = command();
    let command = command.with_max_workers(Some(1)).unwrap();
    let decoded = decoded_converter();
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let mut unexpected = Vec::new();
    for format in [OutputFormat::Dash, OutputFormat::Hls] {
        let entry = audio_first_manifest(
            &command,
            &source,
            &owner.path().join(format!("source-{format:?}")),
            format,
            TargetProtection::Clear,
        );
        let original = VerificationSource {
            input: &entry,
            input_keys: &empty,
            hls_keys: &hls,
        };
        for protection in [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ] {
            let output = owner
                .path()
                .join(format!("audio-first-{format:?}-{protection:?}.mp4"));
            assert!(
                convert(&command, &entry, &output, &empty, protection)
                    .unwrap()
                    .verification
                    .passed
            );
            let reordered = owner
                .path()
                .join(format!("video-first-{format:?}-{protection:?}.mp4"));
            reverse_container_track_order(&output, &reordered);
            let keys = if protection == TargetProtection::Clear {
                InputKeys::default()
            } else {
                role_keys()
            };
            let result = VerificationSource {
                input: &reordered,
                input_keys: &keys,
                hls_keys: &hls,
            };
            assert!(
                command
                    .verify(VerificationRequest {
                        source: VerificationSource {
                            input: &output,
                            input_keys: &keys,
                            hls_keys: &hls
                        },
                        output: result,
                    })
                    .unwrap()
                    .passed,
                "Reordering must leave all original media and metadata equal by role"
            );
            for (mode, verifier) in [("native", &command), ("decoded", &decoded)] {
                let verified = verifier.verify(VerificationRequest {
                    source: original,
                    output: result,
                });
                match verified {
                    Err(error) if error.0 == "presentation-metadata-mismatch" => (),
                    Ok(report) if report.passed => {
                        unexpected.push((format, protection, mode, "passed"));
                    }
                    Ok(_) => unexpected.push((format, protection, mode, "wrong-failure-kind")),
                    Err(error) => unexpected.push((format, protection, mode, error.0)),
                }
            }
        }
    }
    assert!(
        unexpected.is_empty(),
        "Canonical audio order was not rejected: {unexpected:?}"
    );
}

fn assert_audio_only_selection_refused(command: &Converter, entry: &Path, original: &str) {
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let doc = roxmltree::Document::parse(original).unwrap();
    let video = doc
        .descendants()
        .find(|node| {
            node.tag_name().name() == "AdaptationSet"
                && node.attribute("contentType") == Some("video")
        })
        .unwrap();
    let mut single_audio = original.to_owned();
    single_audio.replace_range(video.range(), "");
    fs::write(entry, single_audio).unwrap();
    let single = command
        .validate_source(
            VerificationSource {
                input: entry,
                input_keys: &empty,
                hls_keys: &hls,
            },
            SourceValidationPurpose::Conversion,
        )
        .unwrap();
    assert!(
        !single.eligible_formats.contains(&OutputFormat::Mp4),
        "A labeled audio-only manifest has no invertible container declaration"
    );
    fs::write(entry, original).unwrap();
}

fn manifest_metadata_edits(format: OutputFormat, original: &str) -> Vec<(&str, &str)> {
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
    edits
}

fn assert_manifest_metadata_refused(
    command: &Converter,
    source_view: VerificationSource<'_>,
    output: &Path,
    original: &Path,
    from: &str,
) {
    let qualified = command.validate_source(source_view, SourceValidationPurpose::Conversion);
    assert!(
        qualified.is_err()
            || !qualified
                .unwrap()
                .eligible_formats
                .contains(&OutputFormat::Mp4),
        "{from}: invalid MP4 admission"
    );
    assert!(
        convert(
            command,
            source_view.input,
            output,
            source_view.input_keys,
            TargetProtection::Clear
        )
        .is_err(),
        "{from}"
    );
    assert!(!output.exists());
    assert!(
        command
            .verify(VerificationRequest {
                source: source_view,
                output: VerificationSource {
                    input: original,
                    input_keys: source_view.input_keys,
                    hls_keys: source_view.hls_keys
                },
            })
            .is_err(),
        "{from}: invalid standalone metadata comparison"
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
        if format == OutputFormat::Dash {
            assert_audio_only_selection_refused(&command, &entry, &original);
        }

        let edits = manifest_metadata_edits(format, &original);
        for (index, (from, to)) in edits.iter().enumerate() {
            assert!(original.contains(from), "missing declared field {from}");
            fs::write(&entry, original.replace(from, to)).unwrap();
            let source_view = VerificationSource {
                input: &entry,
                input_keys: &empty,
                hls_keys: &hls,
            };
            assert_manifest_metadata_refused(
                &command,
                source_view,
                &owner.path().join(format!("refused-{format:?}-{index}.mp4")),
                &source,
                from,
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

fn explicitly_english_manifest(format: OutputFormat, original: &str) -> String {
    if format == OutputFormat::Hls {
        if original.contains("LANGUAGE=\"und\"") {
            original.replace("LANGUAGE=\"und\"", "LANGUAGE=\"en\"")
        } else {
            original.replace("TYPE=AUDIO,", "TYPE=AUDIO,LANGUAGE=\"en\",")
        }
    } else {
        let doc = roxmltree::Document::parse(original).unwrap();
        let audio = doc
            .descendants()
            .find(|node| {
                node.tag_name().name() == "AdaptationSet"
                    && node.attribute("contentType") == Some("audio")
            })
            .unwrap();
        let mut value = original.to_owned();
        if let Some(language) = audio.attribute_node("lang") {
            value.replace_range(language.range_value(), "en");
        } else {
            let at = audio.range().start + "<AdaptationSet".len();
            value.insert_str(at, " lang=\"en\"");
        }
        value
    }
}

fn assert_physical_language_and_tampering(
    command: &Converter,
    source: VerificationSource<'_>,
    output: VerificationSource<'_>,
    directory: &Path,
    case: &str,
) {
    let bytes = fs::read(output.input).unwrap();
    let header = audio_nodes(&bytes)
        .into_iter()
        .find(|node| node.kind() == *b"mdhd")
        .unwrap();
    let payload = header.header.payload(&bytes);
    let offset = if payload[0] == 0 { 20 } else { 32 };
    let language = u16::from_be_bytes(payload[offset..offset + 2].try_into().unwrap());
    assert_eq!(
        language,
        b"eng"
            .iter()
            .fold(0u16, |value, byte| (value << 5) | u16::from(byte - 0x60))
    );
    for wrong in [b"und", b"fra"] {
        let mut changed = bytes.clone();
        let at = header.header.offset + header.header.header_size + offset;
        let code = wrong
            .iter()
            .fold(0u16, |value, byte| (value << 5) | u16::from(byte - 0x60));
        changed[at..at + 2].copy_from_slice(&code.to_be_bytes());
        let tampered = directory.join(format!(
            "bad-{case}-{}.mp4",
            std::str::from_utf8(wrong).unwrap()
        ));
        fs::write(&tampered, changed).unwrap();
        let proof = command.verify(VerificationRequest {
            source,
            output: VerificationSource {
                input: &tampered,
                input_keys: output.input_keys,
                hls_keys: output.hls_keys,
            },
        });
        assert!(proof.is_err() || !proof.unwrap().passed);
    }
}

fn assert_bare_mp4_language_is_physical(
    command: &Converter,
    source: &Path,
    output: VerificationSource<'_>,
) {
    let empty = InputKeys::default();
    let physical_comparison = command.verify(VerificationRequest {
        source: VerificationSource {
            input: source,
            input_keys: &empty,
            hls_keys: output.hls_keys,
        },
        output: VerificationSource {
            input: output.input,
            input_keys: output.input_keys,
            hls_keys: output.hls_keys,
        },
    });
    assert!(
        physical_comparison.is_err() || !physical_comparison.unwrap().passed,
        "A bare MP4 must not inherit a manifest language override"
    );
}

fn assert_effective_language_decoded(
    source: VerificationSource<'_>,
    output: VerificationSource<'_>,
) {
    let decoded = Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::new()),
            Arc::new(Collector::default()),
        )),
        "packager",
        "ffmpeg",
    )
    .with_ffprobe("ffprobe")
    .with_decode(true)
    .with_max_workers(Some(1))
    .unwrap();
    assert!(
        decoded
            .verify(VerificationRequest {
                source,
                output: VerificationSource {
                    input: output.input,
                    input_keys: output.input_keys,
                    hls_keys: output.hls_keys
                }
            })
            .unwrap()
            .passed
    );
}

fn assert_effective_language_manifest_return(
    command: &Converter,
    source: VerificationSource<'_>,
    output: VerificationSource<'_>,
    returned: &Path,
    format: OutputFormat,
    target: TargetProtection,
) {
    let empty = InputKeys::default();
    command
        .convert(ConversionRequest {
            input: output.input,
            output: returned,
            format,
            target: TargetProtection::Clear,
            input_keys: output.input_keys,
            hls_input_keys: output.hls_keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap_or_else(|error| panic!("return {format:?}/{target:?}: {error:?}"));
    let returned_entry = returned.join(if format == OutputFormat::Dash {
        "manifest.mpd"
    } else {
        "master.m3u8"
    });
    assert!(
        command
            .verify(VerificationRequest {
                source,
                output: VerificationSource {
                    input: &returned_entry,
                    input_keys: &empty,
                    hls_keys: output.hls_keys
                }
            })
            .unwrap()
            .passed
    );
}

#[test]
fn undefined_container_language_uses_the_validated_manifest_language_in_mp4() {
    let owner = common::private_tempdir().unwrap();
    let progressive = owner.path().join("progressive.mp4");
    joint(&progressive, false);
    let source = owner.path().join("source.mp4");
    let (command, _) = command();
    let command = command.with_max_workers(Some(1)).unwrap();
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
        let directory = owner.path().join(format!("language-{format:?}"));
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
        let changed = explicitly_english_manifest(format, &original);
        assert_ne!(original, changed);
        fs::write(&entry, changed).unwrap();
        let source_view = VerificationSource {
            input: &entry,
            input_keys: &empty,
            hls_keys: &hls,
        };
        assert_mp4_admitted(&command, source_view, &format!("{format:?}"));
        for target in [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ] {
            let output = owner
                .path()
                .join(format!("language-{format:?}-{target:?}.mp4"));
            assert!(
                convert(&command, &entry, &output, &empty, target)
                    .unwrap()
                    .verification
                    .passed
            );
            let keys = if target == TargetProtection::Clear {
                InputKeys::default()
            } else {
                role_keys()
            };
            let output_view = VerificationSource {
                input: &output,
                input_keys: &keys,
                hls_keys: &hls,
            };
            assert!(
                command
                    .verify(VerificationRequest {
                        source: source_view,
                        output: output_view
                    })
                    .unwrap()
                    .passed
            );
            assert_physical_language_and_tampering(
                &command,
                source_view,
                output_view,
                owner.path(),
                &format!("{format:?}-{target:?}"),
            );
            assert_bare_mp4_language_is_physical(&command, &source, output_view);
            assert_effective_language_manifest_return(
                &command,
                source_view,
                output_view,
                &owner.path().join(format!("returned-{format:?}-{target:?}")),
                format,
                target,
            );
            if target == TargetProtection::Clear {
                assert_effective_language_decoded(source_view, output_view);
            }
        }
    }
}
