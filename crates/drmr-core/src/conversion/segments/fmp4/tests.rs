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
mod final_signaling;
mod guards;
mod review;
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
        let FixtureResources {
            hls: resources,
            source: source_resources,
            window,
        } = fixture_resources(&converter, &transformer, &init, original, &directory);
        let metadata = fixture_metadata(audio, fixture.parts.len() > 1);
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
            window,
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
        finish_fixture_dash(&converter, &root, graph_parts, &output_parts, scheme, &key)
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

struct Oracle {
    original: Vec<u8>,
    packets: serde_json::Value,
    decoded: Vec<u8>,
}
struct MatrixKeys {
    empty: InputKeys,
    source: InputKeys,
    output: OutputKey,
    hls_output: crate::conversion::HlsOutputKey,
    empty_hls: HlsInputKeys,
    aes_hls: HlsInputKeys,
}
fn matrix_keys() -> MatrixKeys {
    MatrixKeys {
        empty: InputKeys::default(),
        source: InputKeys::new(vec![
            crate::conversion::ContentKey::from_hex(SOURCE_ID, SOURCE_KEY).unwrap(),
        ])
        .unwrap(),
        output: OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap(),
        hls_output: crate::conversion::HlsOutputKey::new(
            "https://example.invalid/key",
            crate::conversion::Aes128Key::from_hex(OUTPUT_KEY).unwrap(),
        )
        .unwrap(),
        empty_hls: HlsInputKeys::default(),
        aes_hls: HlsInputKeys::new(vec![(
            "https://example.invalid/source-key".into(),
            crate::conversion::Aes128Key::from_hex(SOURCE_KEY).unwrap(),
        )])
        .unwrap(),
    }
}
fn matrix_sources(fixture: &Fixture) -> Vec<(OutputFormat, TargetProtection, PathBuf)> {
    let mut sources = Vec::new();
    for format in [OutputFormat::Hls, OutputFormat::Dash] {
        let schemes = if format == OutputFormat::Hls {
            vec![
                TargetProtection::Clear,
                TargetProtection::Cenc,
                TargetProtection::Cbcs,
                TargetProtection::HlsAes128,
            ]
        } else {
            vec![
                TargetProtection::Clear,
                TargetProtection::Cenc,
                TargetProtection::Cbcs,
            ]
        };
        for scheme in schemes {
            let name = format!("source-{}-{}", format.as_str(), scheme.as_str());
            let path = if scheme == TargetProtection::HlsAes128 {
                aes_fixture(fixture, &name)
            } else {
                manifest_fixture(fixture, scheme, format, &name)
            };
            sources.push((format, scheme, path));
        }
    }
    sources
}

#[test]
fn original_hls_and_dash_cipher_pairs_keep_membership_and_manifest_selection_without_tools() {
    let fixture = fixture(44_100);
    let keys = matrix_keys();
    let expected = fixture
        .parts
        .iter()
        .map(|(_, _, path)| Oracle {
            original: std::fs::read(path).unwrap(),
            packets: packets(path, None),
            decoded: decoded(path, None),
        })
        .collect::<Vec<_>>();
    let mut cases = 0;
    for (source_format, source_scheme, input) in matrix_sources(&fixture) {
        let input_keys = if matches!(
            source_scheme,
            TargetProtection::Clear | TargetProtection::HlsAes128
        ) {
            &keys.empty
        } else {
            &keys.source
        };
        let hls_input_keys = if source_scheme == TargetProtection::HlsAes128 {
            &keys.aes_hls
        } else {
            &keys.empty_hls
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
                    hls_input_keys,
                    output_key: matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs)
                        .then_some(&keys.output),
                    hls_key_uri: (format == OutputFormat::Hls && target == TargetProtection::Cbcs)
                        .then_some("https://example.invalid/key"),
                    hls_output_key: (target == TargetProtection::HlsAes128)
                        .then_some(&keys.hls_output),
                };
                assert_matrix_case(&fixture, &expected, request, source_format, source_scheme);
                cases += 1;
            }
        }
    }
    assert_eq!(cases, 42);
    for ((_, _, path), oracle) in fixture.parts.iter().zip(expected) {
        assert_eq!(std::fs::read(path).unwrap(), oracle.original);
    }
    eprintln!(
        "verified {cases} native conversions against independent packet and complete decoded-content oracles"
    );
}

fn execute_matrix_case(
    converter: &Converter,
    request: ConversionRequest<'_>,
    source_format: OutputFormat,
    source_scheme: TargetProtection,
) -> crate::conversion::ConversionReport {
    let report = if source_format == OutputFormat::Hls {
        let input = VerificationSource {
            input: request.input,
            input_keys: request.input_keys,
            hls_keys: request.hls_input_keys,
        };
        let source = super::super::source::HlsSource::load(converter, input).unwrap();
        let qualified = qualify_hls(
            converter,
            &source,
            input,
            SourceValidationPurpose::Conversion,
        )
        .unwrap();
        assert!(qualified.verification.passed);
        assert_eq!(qualified.actual_format, OutputFormat::Hls);
        if source_scheme == TargetProtection::HlsAes128 {
            assert!(qualified.track_protection.iter().all(|protection| *protection == crate::conversion::SourceProtection::HlsAes128));
        }
        convert_hls(converter, request, source)
    } else {
        convert_dash(converter, request)
    };
    report.unwrap_or_else(|error| {
        panic!(
            "{} {} to {} {}: {}",
            source_format.as_str(),
            source_scheme.as_str(),
            request.format.as_str(),
            request.target.as_str(),
            error.0
        )
    })
}

fn assert_matrix_case(
    fixture: &Fixture,
    expected: &[Oracle],
    request: ConversionRequest<'_>,
    source_format: OutputFormat,
    source_scheme: TargetProtection,
) {
    let (converter, events) = converter();
    let report = execute_matrix_case(&converter, request, source_format, source_scheme);
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
    assert_eq!(
        report
            .processing
            .as_ref()
            .unwrap()
            .required_tools
            .as_slice(),
        []
    );
    let output_keys = super::proof::output_keys(request.output_key).unwrap();
    let hls_keys = super::proof::output_hls_keys(request.hls_output_key).unwrap();
    let result = super::super::source::entrypoint(request.output, request.format);
    assert!(
        verify(
            &converter,
            VerificationRequest {
                source: VerificationSource {
                    input: request.input,
                    input_keys: request.input_keys,
                    hls_keys: request.hls_input_keys
                },
                output: VerificationSource {
                    input: &result,
                    input_keys: &output_keys,
                    hls_keys: &hls_keys
                },
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
    assert_matrix_oracles(fixture, expected, &actual, request.target);
    assert_no_tools(&events);
}

fn assert_matrix_oracles(
    fixture: &Fixture,
    expected: &[Oracle],
    actual: &super::source::Presentation,
    target: TargetProtection,
) {
    for part in &actual.parts {
        let inspection = crate::inspection::inspect(&part.initialization).unwrap();
        let index = usize::from(inspection.tracks[0].kind != TrackKind::Video);
        let joined = fixture.directory.path().join("oracle.mp4");
        let mut file = std::fs::File::create(&joined).unwrap();
        for path in std::iter::once(&part.initialization)
            .chain(part.resources.iter().map(|resource| &resource.path))
        {
            std::io::copy(&mut std::fs::File::open(path).unwrap(), &mut file).unwrap();
        }
        drop(file);
        let key = matches!(target, TargetProtection::Cenc | TargetProtection::Cbcs)
            .then_some((OUTPUT_ID, OUTPUT_KEY));
        assert_eq!(expected[index].packets, packets(&joined, key));
        assert_eq!(expected[index].decoded, decoded(&joined, key));
        std::fs::remove_file(joined).unwrap();
    }
}

fn aes_fixture(fixture: &Fixture, name: &str) -> PathBuf {
    let input = manifest_fixture(
        fixture,
        TargetProtection::Clear,
        OutputFormat::Hls,
        &format!("{name}-clear"),
    );
    let output = fixture.directory.path().join(name);
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    let key = crate::conversion::HlsOutputKey::new(
        "https://example.invalid/source-key",
        crate::conversion::Aes128Key::from_hex(SOURCE_KEY).unwrap(),
    )
    .unwrap();
    let (converter, events) = converter();
    let source = super::super::source::HlsSource::load(
        &converter,
        VerificationSource {
            input: &input,
            input_keys: &empty,
            hls_keys: &hls,
        },
    )
    .unwrap();
    let report = convert_hls(
        &converter,
        ConversionRequest {
            input: &input,
            output: &output,
            format: OutputFormat::Hls,
            target: TargetProtection::HlsAes128,
            input_keys: &empty,
            hls_input_keys: &hls,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: Some(&key),
        },
        source,
    )
    .unwrap();
    assert!(report.verification.passed);
    for ordinal in 0..2 {
        let text =
            std::fs::read_to_string(output.join(format!("track-{ordinal:04}/media.m3u8"))).unwrap();
        assert!(text.contains("METHOD=AES-128"));
        assert!(text.contains("IV=0x"));
        assert!(text.contains("#EXT-X-MAP:"));
        let bytes = std::fs::read(output.join(format!("track-{ordinal:04}/init.mp4"))).unwrap();
        assert!(crate::inspection::inspect_mp4_metadata(&bytes).is_err());
    }
    assert_no_tools(&events);
    output.join("master.m3u8")
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

fn fixture_metadata(
    audio: bool,
    separate: bool,
) -> crate::conversion::presentation::metadata::Metadata {
    if audio {
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
            audio_group: separate.then_some("main-audio".into()),
            resolution: Some((96, 54)),
            ..Default::default()
        }
    }
}

struct FixtureResources {
    hls: Vec<super::super::manifest_hls::HlsResource>,
    source: Vec<super::source::Resource>,
    window: crate::conversion::dash::Window,
}
fn fixture_resources(
    converter: &Converter,
    transformer: &SegmentTransformer<'_>,
    init: &Path,
    original: &[PathBuf],
    directory: &Path,
) -> FixtureResources {
    let mut resources = Vec::new();
    let mut source_resources = Vec::new();
    let mut clock = crate::conversion::timing::FragmentClock::read(init).unwrap();
    for (index, source) in original.iter().enumerate() {
        let path = directory.join(format!("seg_{}.m4s", index + 1));
        transformer
            .rewrite_segment(
                source,
                &path,
                &InputKeys::default(),
                &converter.backend.stop,
            )
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

    let (first, end, scale) = clock.presentation_window().unwrap();
    FixtureResources {
        hls: resources,
        source: source_resources,
        window: crate::conversion::dash::Window {
            origin: (first, scale),
            first: (first, scale),
            end: (end, scale),
        },
    }
}
fn finish_fixture_dash(
    converter: &Converter,
    root: &Path,
    mut graph_parts: Vec<super::source::Part>,
    output_parts: &[super::manifest::OutputPart],
    scheme: TargetProtection,
    key: &OutputKey,
) -> PathBuf {
    let empty = InputKeys::default();
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
        output: root,
        format: OutputFormat::Dash,
        target: scheme,
        input_keys: &empty,
        hls_input_keys: &HlsInputKeys::default(),
        output_key: (scheme != TargetProtection::Clear).then_some(key),
        hls_key_uri: None,
        hls_output_key: None,
    };
    super::manifest::dash_output::write(converter, root, &graph, output_parts, &request).unwrap();
    root.join("manifest.mpd")
}
