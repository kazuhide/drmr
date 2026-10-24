use super::*;
use crate::{
    conversion::{
        ConversionRequest, OutputKey, TargetProtection, native_media::SegmentTransformer,
    },
    inspection::{OutputFormat, TrackKind},
};
use crate::{
    conversion::{
        Converter, HlsInputKeys, InputKeys, SourceValidationPurpose, VerificationRequest,
        VerificationSource,
    },
    events::Collector,
    toolrun::{Secrets, ToolRunner},
};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};

const SOURCE_ID: &str = "000102030405060708090a0b0c0d0e0f";
const SOURCE_KEY: &str = "101112131415161718191a1b1c1d1e1f";
const OUTPUT_ID: &str = "404142434445464748494a4b4c4d4e4f";
const OUTPUT_KEY: &str = "00112233445566778899aabbccddeeff";
mod guards;
mod signaling;
mod windows;

fn converter() -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(runner, "must-not-run-packager", "must-not-run-ffmpeg"),
        events,
    )
}

#[test]
fn cancellation_precedes_manifest_reads_and_media_tools() {
    let (converter, events) = converter();
    let stop = crate::concurrency::Stop::new();
    stop.set();
    let converter = converter.with_stop(stop);
    let keys = InputKeys::default();
    let hls_keys = HlsInputKeys::default();
    let source = VerificationSource {
        input: Path::new("missing.mpd"),
        input_keys: &keys,
        hls_keys: &hls_keys,
    };
    assert_eq!(
        qualify_dash(&converter, source, SourceValidationPurpose::Conversion).err(),
        Some(crate::conversion::ConversionError("cancelled"))
    );
    assert_eq!(
        verify(
            &converter,
            VerificationRequest {
                source,
                output: source
            }
        )
        .err(),
        Some(crate::conversion::ConversionError("cancelled"))
    );
    assert!(
        events
            .records()
            .iter()
            .all(|event| event.get("stage").and_then(serde_json::Value::as_str) != Some("tool"))
    );
}

struct Fixture {
    directory: crate::native::PrivateTempDir,
    parts: Vec<(PathBuf, Vec<PathBuf>, PathBuf)>,
}

fn run(command: &mut Command) -> Vec<u8> {
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "independent fixture or oracle failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stderr.is_empty(),
        "independent oracle reported an error: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

fn fixture(rate: u32) -> Fixture {
    let directory = crate::native::private_tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut parts = Vec::new();
    for kind in ["video", "audio"] {
        let path = directory.path().join(format!("{kind}.mp4"));
        let mut command = Command::new("ffmpeg");
        command
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(&source);
        if kind == "video" {
            command.args([
                "-map",
                "0:v:0",
                "-an",
                "-vf",
                "scale=96:54",
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-pix_fmt",
                "yuv420p",
                "-threads",
                "1",
                "-bf",
                "2",
                "-g",
                "48",
                "-keyint_min",
                "1",
                "-sc_threshold",
                "0",
                "-force_key_frames",
                "1,3.5,5.5,7,8.5",
            ]);
        } else {
            command
                .args([
                    "-map", "0:a:0", "-vn", "-c:a", "aac", "-threads", "1", "-ar",
                ])
                .arg(rate.to_string());
        }
        run(command
            .args([
                "-movflags",
                "+empty_moov+default_base_moof+frag_every_frame",
                "-f",
                "mp4",
            ])
            .arg(&path));
        let (init_bytes, fragments) =
            crate::mp4::split_fragmented(&std::fs::read(&path).unwrap()).unwrap();
        let init = directory.path().join(format!("{kind}-init.mp4"));
        std::fs::write(&init, init_bytes).unwrap();
        let cuts = if kind == "video" {
            vec![0, 24, 84, fragments.len()]
        } else {
            vec![0, 86, 194, fragments.len()]
        };
        let resources = cuts
            .windows(2)
            .enumerate()
            .map(|(ordinal, bounds)| {
                let path = directory.path().join(format!("{kind}-{ordinal}.m4s"));
                std::fs::write(&path, fragments[bounds[0]..bounds[1]].concat()).unwrap();
                path
            })
            .collect::<Vec<_>>();
        parts.push((init, resources, path));
    }
    Fixture { directory, parts }
}

fn manifest_fixture(
    fixture: &Fixture,
    scheme: TargetProtection,
    format: OutputFormat,
    name: &str,
) -> PathBuf {
    let (converter, _) = converter();
    let root = fixture.directory.path().join(name);
    crate::conversion::files::create_dir(&root).unwrap();
    let key = OutputKey::from_hex(SOURCE_ID, SOURCE_KEY).unwrap();
    let empty = InputKeys::default();
    let mut hls = Vec::new();
    let mut graph_parts = Vec::new();
    let mut output_parts = Vec::new();
    for (ordinal, (source_init, original, _)) in fixture.parts.iter().enumerate() {
        let audio =
            crate::inspection::inspect(source_init).unwrap().tracks[0].kind == TrackKind::Audio;
        let directory = root.join(format!("track-{ordinal:04}"));
        crate::conversion::files::create_dir(&directory).unwrap();
        let init = directory.join("init.mp4");
        let transformer = SegmentTransformer::new(
            source_init,
            &init,
            scheme,
            (scheme != TargetProtection::Clear).then_some(&key),
            &converter.backend.stop,
        )
        .unwrap();
        let mut resources = Vec::new();
        let mut source_resources = Vec::new();
        let mut clock = crate::conversion::timing::FragmentClock::read(&init).unwrap();
        for (index, source) in original.iter().enumerate() {
            let path = directory.join(format!("seg_{}.m4s", index + 1));
            transformer
                .rewrite_segment(source, &path, &empty, &converter.backend.stop)
                .unwrap();
            let (time, ticks, timescale) = clock.dash_segment(&path).unwrap();
            let duration = crate::conversion::finite::manifest::seconds(ticks, timescale).unwrap();
            source_resources.push(super::source::Resource {
                path: path.clone(),
                duration: duration.clone(),
                dash: crate::conversion::dash::SegmentBoundary {
                    number: index as u64 + 1,
                    time,
                    duration: ticks,
                    timescale,
                },
            });
            resources.push(super::super::manifest_hls::HlsResource {
                path,
                duration,
                key_tag: None,
            });
        }
        let metadata = if audio {
            crate::conversion::presentation::metadata::Metadata {
                origin: crate::conversion::presentation::metadata::Origin::Hls,
                audio: Some(crate::conversion::presentation::metadata::Rendition {
                    group: "main-audio".into(),
                    name: Some("English".into()),
                    language: "en".into(),
                    is_default: Some(true),
                    autoselect: Some(true),
                    channels: Some("2".into()),
                }),
                ..Default::default()
            }
        } else {
            crate::conversion::presentation::metadata::Metadata {
                origin: crate::conversion::presentation::metadata::Origin::Hls,
                audio_group: (fixture.parts.len() > 1).then_some("main-audio".into()),
                resolution: Some((96, 54)),
                ..Default::default()
            }
        };
        let codecs = if audio {
            vec!["mp4a.40.2".into()]
        } else {
            vec!["avc1.64000a".into()]
        };
        let initial_key_tag = if scheme == TargetProtection::Clear {
            None
        } else {
            Some(format!(
                "#EXT-X-KEY:METHOD={},URI=\"synthetic-key\",KEYFORMAT=\"identity\"",
                if scheme == TargetProtection::Cenc {
                    "SAMPLE-AES-CTR"
                } else {
                    "SAMPLE-AES"
                }
            ))
        };
        let (first, end, scale) = clock.presentation_window().unwrap();
        graph_parts.push(super::source::Part {
            initialization: init.clone(),
            resources: source_resources,
            metadata: metadata.clone(),
            role: Some(if audio {
                TrackKind::Audio
            } else {
                TrackKind::Video
            }),
            outer: crate::inspection::Protection::Clear,
            window: crate::conversion::dash::Window {
                origin: (first, scale),
                first: (first, scale),
                end: (end, scale),
            },
            empty_cue_pair: false,
        });
        let paths = resources
            .iter()
            .map(|resource| resource.path.clone())
            .collect::<Vec<_>>();
        let keys =
            super::proof::output_keys((scheme != TargetProtection::Clear).then_some(&key)).unwrap();
        let native = converter
            .backend
            .native_segmented_media(&init, &paths, &keys)
            .unwrap();
        let bandwidth = super::manifest::bandwidth(&paths, &native).unwrap();
        output_parts.push(super::manifest::OutputPart {
            initialization: init.clone(),
            resources: paths,
            codecs: codecs.clone(),
            resolution: (!audio).then_some((96, 54)),
            bandwidth,
        });
        hls.push(super::super::manifest_hls::HlsPart {
            initialization: Some(init),
            resources,
            codecs,
            resolution: (!audio).then_some((96, 54)),
            metadata,
            initial_key_tag,
        });
    }
    if format == OutputFormat::Hls {
        super::super::manifest_hls::write(&root, &hls, &converter.backend.stop).unwrap();
        root.join("master.m3u8")
    } else {
        let origin = graph_parts
            .iter()
            .map(|part| part.window.first)
            .min_by(|a, b| {
                (u128::from(a.0) * u128::from(b.1)).cmp(&(u128::from(b.0) * u128::from(a.1)))
            })
            .unwrap();
        for part in &mut graph_parts {
            part.window.origin = origin;
        }
        let graph = super::source::Presentation {
            report: crate::inspection::Inspection::new(
                crate::inspection::InputFormat::Dash,
                crate::inspection::Evidence::Manifest,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
            format: OutputFormat::Dash,
            parts: graph_parts,
        };
        let request = ConversionRequest {
            input: Path::new("synthetic"),
            output: &root,
            format,
            target: scheme,
            input_keys: &empty,
            hls_input_keys: &HlsInputKeys::default(),
            output_key: (scheme != TargetProtection::Clear).then_some(&key),
            hls_key_uri: None,
            hls_output_key: None,
        };
        super::manifest::dash_output::write(&converter, &root, &graph, &output_parts, &request)
            .unwrap();
        root.join("manifest.mpd")
    }
}

fn assert_no_tools(events: &Collector) {
    assert!(
        events
            .records()
            .iter()
            .all(|event| event.get("stage").and_then(serde_json::Value::as_str) != Some("tool"))
    );
}

#[test]
fn original_hls_and_dash_cipher_pairs_keep_membership_and_manifest_selection_without_tools() {
    let fixture = fixture(44_100);
    let empty = InputKeys::default();
    let source_keys = InputKeys::new(vec![
        crate::conversion::ContentKey::from_hex(SOURCE_ID, SOURCE_KEY).unwrap(),
    ])
    .unwrap();
    let output_key = OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap();
    let hls_output = crate::conversion::HlsOutputKey::new(
        "https://example.invalid/key",
        crate::conversion::Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
    )
    .unwrap();
    let empty_hls = HlsInputKeys::default();
    for source_format in [OutputFormat::Hls, OutputFormat::Dash] {
        for source_scheme in [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ] {
            let input = manifest_fixture(
                &fixture,
                source_scheme,
                source_format,
                &format!(
                    "source-{}-{}",
                    source_format.as_str(),
                    source_scheme.as_str()
                ),
            );
            let input_keys = if source_scheme == TargetProtection::Clear {
                &empty
            } else {
                &source_keys
            };
            for (format, targets) in [
                (
                    OutputFormat::Dash,
                    vec![
                        TargetProtection::Clear,
                        TargetProtection::Cenc,
                        TargetProtection::Cbcs,
                    ],
                ),
                (
                    OutputFormat::Hls,
                    vec![
                        TargetProtection::Clear,
                        TargetProtection::Cbcs,
                        TargetProtection::HlsAes128,
                    ],
                ),
            ] {
                for target in targets {
                    let (converter, events) = converter();
                    let output = fixture.directory.path().join(format!(
                        "output-{}-{}-{}-{}",
                        source_format.as_str(),
                        source_scheme.as_str(),
                        format.as_str(),
                        target.as_str()
                    ));
                    let request = ConversionRequest {
                        input: &input,
                        output: &output,
                        format,
                        target,
                        input_keys,
                        hls_input_keys: &empty_hls,
                        output_key: matches!(
                            target,
                            TargetProtection::Cenc | TargetProtection::Cbcs
                        )
                        .then_some(&output_key),
                        hls_key_uri: (format == OutputFormat::Hls
                            && target == TargetProtection::Cbcs)
                            .then_some("https://example.invalid/key"),
                        hls_output_key: (target == TargetProtection::HlsAes128)
                            .then_some(&hls_output),
                    };
                    let report = if source_format == OutputFormat::Hls {
                        let source = super::super::source::HlsSource::load(
                            &converter,
                            VerificationSource {
                                input: &input,
                                input_keys,
                                hls_keys: &empty_hls,
                            },
                        )
                        .unwrap();
                        let qualified = qualify_hls(
                            &converter,
                            &source,
                            VerificationSource {
                                input: &input,
                                input_keys,
                                hls_keys: &empty_hls,
                            },
                            SourceValidationPurpose::Conversion,
                        )
                        .unwrap();
                        assert!(qualified.verification.passed);
                        assert_eq!(qualified.actual_format, OutputFormat::Hls);
                        convert_hls(&converter, request, source)
                    } else {
                        convert_dash(&converter, request)
                    }
                    .unwrap_or_else(|error| {
                        panic!(
                            "{} {} to {} {}: {}",
                            source_format.as_str(),
                            source_scheme.as_str(),
                            format.as_str(),
                            target.as_str(),
                            error.0
                        )
                    });
                    assert!(report.verification.passed);
                    assert!(
                        report
                            .verification
                            .native_manifest
                            .as_ref()
                            .unwrap()
                            .is_clean_for_scope()
                    );
                    assert_eq!(
                        report.processing.as_ref().unwrap().route,
                        crate::inspection::ProcessingRouteId::Fmp4Segments
                    );
                    assert!(
                        report
                            .processing
                            .as_ref()
                            .unwrap()
                            .required_tools
                            .is_empty()
                    );
                    let output_keys = super::proof::output_keys(request.output_key).unwrap();
                    let hls_keys = super::proof::output_hls_keys(request.hls_output_key).unwrap();
                    let result = super::super::source::entrypoint(&output, format);
                    assert!(
                        verify(
                            &converter,
                            VerificationRequest {
                                source: VerificationSource {
                                    input: &input,
                                    input_keys,
                                    hls_keys: &empty_hls
                                },
                                output: VerificationSource {
                                    input: &result,
                                    input_keys: &output_keys,
                                    hls_keys: &hls_keys
                                }
                            }
                        )
                        .unwrap()
                        .passed
                    );
                    let snapshot = super::source::Source::load(
                        &converter,
                        VerificationSource {
                            input: &result,
                            input_keys: &output_keys,
                            hls_keys: &hls_keys,
                        },
                    )
                    .unwrap();
                    let actual = snapshot.presentation(&converter).unwrap();
                    for part in &actual.parts {
                        let inspection = crate::inspection::inspect(&part.initialization).unwrap();
                        let index = if inspection.tracks[0].kind == TrackKind::Video {
                            0
                        } else {
                            1
                        };
                        let joined = fixture.directory.path().join("oracle.mp4");
                        let mut file = std::fs::File::create(&joined).unwrap();
                        for path in std::iter::once(&part.initialization)
                            .chain(part.resources.iter().map(|resource| &resource.path))
                        {
                            std::io::copy(&mut std::fs::File::open(path).unwrap(), &mut file)
                                .unwrap();
                        }
                        drop(file);
                        let oracle_key =
                            matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs)
                                .then_some((OUTPUT_ID, OUTPUT_KEY));
                        assert_eq!(
                            packets(&fixture.parts[index].2, None),
                            packets(&joined, oracle_key)
                        );
                        assert_eq!(
                            decoded(&fixture.parts[index].2, None),
                            decoded(&joined, oracle_key)
                        );
                        std::fs::remove_file(joined).unwrap();
                    }
                    assert_no_tools(&events);
                }
            }
        }
    }
}

fn oracle_arguments(command: &mut Command, key: Option<(&str, &str)>) {
    command.args(["-v", "error", "-ignore_editlist", "1"]);
    if let Some((id, key)) = key {
        command.args(["-decryption_keys", &format!("{id}={key}")]);
    }
}
fn packets(path: &Path, key: Option<(&str, &str)>) -> serde_json::Value {
    let mut command = Command::new("ffprobe");
    oracle_arguments(&mut command, key);
    let bytes = run(command
        .args([
            "-show_packets",
            "-show_data_hash",
            "sha256",
            "-show_entries",
            "packet=pts,dts,duration,size,flags,data_hash:packet_side_data=",
            "-of",
            "json",
        ])
        .arg(path));
    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["packets"].clone()
}
fn decoded(path: &Path, key: Option<(&str, &str)>) -> Vec<u8> {
    let mut command = Command::new("ffmpeg");
    oracle_arguments(&mut command, key);
    run(command.args(["-nostdin", "-xerror", "-i"]).arg(path).args([
        "-map",
        "0:0",
        "-c:v",
        "rawvideo",
        "-c:a",
        "pcm_f64le",
        "-f",
        "framehash",
        "-hash",
        "sha256",
        "-",
    ]))
}
