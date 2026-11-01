//! Attributed HEVC seeds exercise complete native conversion separately from decoding.
mod common;

use drmr_core::{
    conversion::{
        ContentKey, ConversionReport, ConversionRequest, Converter, DecodeEvidence, HlsInputKeys,
        InputKeys, OutputKey, ProcessingMode, SourceValidationPurpose, VerificationReport,
        VerificationRequest, VerificationSource,
    },
    events::Collector,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use sha2::{Digest as _, Sha256};
use std::{fs, path::Path, path::PathBuf, sync::Arc};

const SEEDS: [(&str, &str); 2] = [
    (
        "hevc-main.mp4",
        "bb6264c4a9a6aaa74b5afc00e157eea8129a3d38f4d2263b0226c091ccbfbf3a",
    ),
    (
        "hevc-main10.mp4",
        "5cd4bf734245767211030e735f70f0bbcd03c5e070b138d1a99fe76f2234d3fb",
    ),
];
const KIDS: [&str; 2] = [
    "000102030405060708090a0b0c0d0e0f",
    "404142434445464748494a4b4c4d4e4f",
];
const KEYS: [&str; 2] = [
    "101112131415161718191a1b1c1d1e1f",
    "505152535455565758595a5b5c5d5e5f",
];

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}

fn converter(mode: ProcessingMode, packaging: bool, decode: bool) -> (Converter, Arc<Collector>) {
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    (
        Converter::new(
            runner,
            if packaging {
                "packager"
            } else {
                "absent-packager"
            },
            if decode { "ffmpeg" } else { "absent-ffmpeg" },
        )
        .with_ffprobe(if decode { "ffprobe" } else { "absent-ffprobe" })
        .with_decode(decode)
        .with_processing(mode),
        events,
    )
}

fn input_keys(index: usize) -> InputKeys {
    InputKeys::new(vec![
        ContentKey::from_hex(KIDS[index], KEYS[index]).unwrap(),
    ])
    .unwrap()
}

fn assert_native(report: &VerificationReport) {
    assert!(report.passed && report.checks.is_pass(), "{report:?}");
    assert_eq!(report.decode_evidence, DecodeEvidence::not_requested());
    assert!(!report.license_playback_verified);
    if let Some(manifest) = &report.native_manifest {
        assert_eq!(manifest.errors, 0);
        assert!(!manifest.incomplete && !manifest.truncated);
    }
}

fn assert_tools(events: &Collector, packaging: bool) {
    for record in events.records() {
        if record.get("event").and_then(serde_json::Value::as_str) == Some("command") {
            assert!(packaging, "native processing must not invoke a media tool");
            assert_eq!(
                record.get("tool").and_then(serde_json::Value::as_str),
                Some("packager")
            );
        }
        let text = serde_json::to_string(&record).unwrap();
        assert!(KEYS.iter().all(|key| !text.contains(key)));
    }
}

fn verify(
    command: &Converter,
    source: &Path,
    source_keys: &InputKeys,
    output: &Path,
    output_keys: &InputKeys,
) -> Result<VerificationReport, drmr_core::conversion::ConversionError> {
    let hls = HlsInputKeys::default();
    command.verify(VerificationRequest {
        source: VerificationSource {
            input: source,
            input_keys: source_keys,
            hls_keys: &hls,
        },
        output: VerificationSource {
            input: output,
            input_keys: output_keys,
            hls_keys: &hls,
        },
    })
}

fn convert(
    command: &Converter,
    source: &Path,
    source_keys: &InputKeys,
    destination: &Path,
    format: OutputFormat,
    target: TargetProtection,
    key_index: usize,
) -> (PathBuf, InputKeys, ConversionReport) {
    let key = OutputKey::from_hex(KIDS[key_index], KEYS[key_index]).unwrap();
    let protected = target != TargetProtection::Clear;
    let report = command
        .convert(ConversionRequest {
            input: source,
            output: destination,
            format,
            target,
            input_keys: source_keys,
            hls_input_keys: &HlsInputKeys::default(),
            output_key: protected.then_some(&key),
            hls_key_uri: (protected && format == OutputFormat::Hls)
                .then_some("skd://hevc-public-fixture"),
            hls_output_key: None,
        })
        .unwrap_or_else(|error| panic!("{} {}: {error}", format.as_str(), target.as_str()));
    assert_native(&report.verification);
    let entry = match format {
        OutputFormat::Mp4 => destination.to_owned(),
        OutputFormat::Dash => destination.join("manifest.mpd"),
        OutputFormat::Hls => destination.join("master.m3u8"),
        OutputFormat::HlsTs => unreachable!(),
    };
    let keys = if protected {
        input_keys(key_index)
    } else {
        InputKeys::default()
    };
    assert_native(&verify(command, source, source_keys, &entry, &keys).unwrap());
    (entry, keys, report)
}

#[test]
fn real_main_and_main10_sources_have_native_admission_without_decoder_tools() {
    let (command, events) = converter(ProcessingMode::Auto, false, false);
    let empty = InputKeys::default();
    let hls = HlsInputKeys::default();
    for (name, digest) in SEEDS {
        let input = fixture(name);
        let original = fs::read(&input).unwrap();
        assert_eq!(hex::encode(Sha256::digest(&original)), digest);
        let admitted = command
            .validate_source(
                VerificationSource {
                    input: &input,
                    input_keys: &empty,
                    hls_keys: &hls,
                },
                SourceValidationPurpose::Conversion,
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_native(&admitted.verification);
        assert_native(&verify(&command, &input, &empty, &input, &empty).unwrap());
        assert_eq!(fs::read(input).unwrap(), original);
    }
    assert_tools(&events, false);
}

#[test]
fn native_mp4_clear_cenc_cbcs_clear_roundtrips_keep_the_original() {
    let root = common::private_tempdir().unwrap();
    let empty = InputKeys::default();
    let (command, events) = converter(ProcessingMode::Auto, true, false);
    for (profile, (name, _)) in SEEDS.into_iter().enumerate() {
        let original = fixture(name);
        let before = fs::read(&original).unwrap();
        let mut current = original.clone();
        let mut keys = InputKeys::default();
        for (ordinal, target) in [
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
            TargetProtection::Clear,
        ]
        .into_iter()
        .enumerate()
        {
            let output = root.path().join(format!("profile-{profile}-{ordinal}.mp4"));
            let (result, result_keys, _) = convert(
                &command,
                &current,
                &keys,
                &output,
                OutputFormat::Mp4,
                target,
                ordinal % 2,
            );
            assert_native(&verify(&command, &original, &empty, &result, &result_keys).unwrap());
            if ordinal == 0 {
                assert!(
                    !verify(&command, &original, &empty, &result, &empty)
                        .is_ok_and(|report| report.passed)
                );
                let wrong =
                    InputKeys::new(vec![ContentKey::from_hex(KIDS[0], KEYS[1]).unwrap()]).unwrap();
                assert!(
                    !verify(&command, &original, &empty, &result, &wrong)
                        .is_ok_and(|report| report.passed)
                );
            }
            current = result;
            keys = result_keys;
        }
        assert_eq!(fs::read(original).unwrap(), before);
    }
    assert_tools(&events, true);
}

fn last_resource(entry: &Path) -> PathBuf {
    let root = entry.parent().unwrap();
    let mut pending = vec![root.to_owned()];
    let mut resources = Vec::new();
    while let Some(directory) = pending.pop() {
        for item in fs::read_dir(directory).unwrap() {
            let path = item.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "m4s") {
                resources.push(path);
            }
        }
    }
    resources.sort();
    resources.pop().expect("fragmented video resources")
}

fn mutation_offsets(bytes: &[u8]) -> Vec<usize> {
    let nodes = drmr_core::mp4::parse_boxes(bytes).unwrap();
    let mut offsets = Vec::new();
    for node in drmr_core::mp4::walk(&nodes) {
        let at = node.header.offset + node.header.header_size;
        let payload = node.header.payload(bytes);
        match &node.kind() {
            b"mdat" => offsets.push(at + payload.len() - 1),
            b"tfdt" => offsets.push(at + if payload[0] == 0 { 7 } else { 11 }),
            b"trun" => {
                let flags = u32::from_be_bytes(payload[..4].try_into().unwrap()) & 0x00ff_ffff;
                let base = 8 + usize::from(flags & 1 != 0) * 4;
                if flags & 4 != 0 {
                    offsets.push(at + base + 1);
                } else if flags & 0x400 != 0 {
                    let sample_flags = base
                        + usize::from(flags & 0x100 != 0) * 4
                        + usize::from(flags & 0x200 != 0) * 4;
                    offsets.push(at + sample_flags + 1);
                }
            }
            _ => {}
        }
    }
    assert!(
        offsets.len() >= 3,
        "payload, clock and sync fields must be covered"
    );
    offsets
}

fn merge_first_two_resources(entry: &Path) {
    let master = fs::read_to_string(entry).unwrap();
    let child = master
        .lines()
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap();
    let playlist = entry.parent().unwrap().join(child);
    let text = fs::read_to_string(&playlist).unwrap();
    let mut lines: Vec<_> = text.lines().map(str::to_owned).collect();
    let intervals: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.strip_prefix("#EXTINF:")
                .map(|value| (index, value.trim_end_matches(',').parse::<f64>().unwrap()))
        })
        .collect();
    assert!(intervals.len() >= 3);
    let (first, duration) = intervals[0];
    let (second, next) = intervals[1];
    let root = playlist.parent().unwrap();
    let first_media = root.join(&lines[first + 1]);
    let mut bytes = fs::read(&first_media).unwrap();
    bytes.extend(fs::read(root.join(&lines[second + 1])).unwrap());
    fs::write(first_media, bytes).unwrap();
    let merged = duration + next;
    lines[first] = format!("#EXTINF:{merged:.9},");
    let bound = intervals
        .iter()
        .skip(2)
        .fold(merged, |maximum, (_, duration)| maximum.max(*duration))
        .ceil();
    let target = lines
        .iter()
        .position(|line| line.starts_with("#EXT-X-TARGETDURATION:"))
        .unwrap();
    lines[target] = format!("#EXT-X-TARGETDURATION:{bound:.0}");
    lines.drain(second..=second + 1);
    fs::write(playlist, lines.join("\n") + "\n").unwrap();
}

#[test]
fn fmp4_hls_dash_modes_and_late_mutations_use_fresh_native_verification() {
    let root = common::private_tempdir().unwrap();
    let empty = InputKeys::default();
    let (packaging, package_events) = converter(ProcessingMode::Auto, true, false);
    let (independent, verify_events) = converter(ProcessingMode::Auto, false, false);
    for (profile, (name, _)) in SEEDS.into_iter().enumerate() {
        let seed = fixture(name);
        let original = fs::read(&seed).unwrap();
        let (baseline, _, _) = convert(
            &packaging,
            &seed,
            &empty,
            &root.path().join(format!("baseline-{profile}")),
            OutputFormat::Dash,
            TargetProtection::Clear,
            0,
        );
        let baseline_bytes = common::snapshot_tree(baseline.parent().unwrap());
        for mode in [
            ProcessingMode::Auto,
            ProcessingMode::Stitch,
            ProcessingMode::Segments,
        ] {
            let (command, events) = converter(mode, mode != ProcessingMode::Segments, false);
            let mut current = baseline.clone();
            let mut keys = InputKeys::default();
            for (ordinal, (format, target)) in [
                (OutputFormat::Hls, TargetProtection::Cenc),
                (OutputFormat::Dash, TargetProtection::Cbcs),
                (OutputFormat::Hls, TargetProtection::Clear),
            ]
            .into_iter()
            .enumerate()
            {
                let output = root.path().join(format!("{profile}-{mode:?}-{ordinal}"));
                let (result, result_keys, report) = convert(
                    &command,
                    &current,
                    &keys,
                    &output,
                    format,
                    target,
                    ordinal % 2,
                );
                assert_native(&verify(&independent, &seed, &empty, &result, &result_keys).unwrap());
                assert_eq!(
                    report.processing.as_ref().unwrap().mode,
                    Some(if mode == ProcessingMode::Auto {
                        ProcessingMode::Stitch
                    } else {
                        mode
                    })
                );
                current = result;
                keys = result_keys;
            }
            if mode == ProcessingMode::Auto {
                let (file, file_keys, _) = convert(
                    &command,
                    &current,
                    &keys,
                    &root.path().join(format!("from-hls-{profile}.mp4")),
                    OutputFormat::Mp4,
                    TargetProtection::Cbcs,
                    1,
                );
                assert_native(&verify(&independent, &seed, &empty, &file, &file_keys).unwrap());
            }
            let media = last_resource(&current);
            let intact = fs::read(&media).unwrap();
            for offset in mutation_offsets(&intact) {
                let mut changed = intact.clone();
                changed[offset] ^= 1;
                fs::write(&media, changed).unwrap();
                assert!(
                    !verify(&independent, &seed, &empty, &current, &keys)
                        .is_ok_and(|report| report.passed),
                    "late payload, clock or sync mutation passed"
                );
            }
            fs::write(&media, intact).unwrap();
            assert_native(&verify(&independent, &seed, &empty, &current, &keys).unwrap());
            if mode == ProcessingMode::Segments {
                merge_first_two_resources(&current);
                // All packets still correspond; only their resource membership changed.
                assert_native(&verify(&independent, &seed, &empty, &current, &keys).unwrap());
                assert!(
                    !verify(&command, &baseline, &empty, &current, &keys)
                        .is_ok_and(|report| report.passed)
                );
            }
            assert_tools(&events, mode != ProcessingMode::Segments);
        }
        assert_eq!(
            common::snapshot_tree(baseline.parent().unwrap()),
            baseline_bytes
        );
        assert_eq!(fs::read(seed).unwrap(), original);
    }
    assert_tools(&package_events, true);
    assert_tools(&verify_events, false);
}

#[test]
fn malformed_configuration_and_a_late_forbidden_nal_never_publish() {
    let root = common::private_tempdir().unwrap();
    let (validation, validation_events) = converter(ProcessingMode::Auto, false, false);
    let (command, events) = converter(ProcessingMode::Auto, true, false);
    for (profile, (name, _)) in SEEDS.into_iter().enumerate() {
        let seed = fixture(name);
        let original = fs::read(&seed).unwrap();
        let nodes = drmr_core::mp4::parse_boxes(&original).unwrap();
        let config = drmr_core::mp4::walk(&nodes)
            .into_iter()
            .find(|node| node.kind() == *b"hvcC")
            .unwrap();
        let output = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_packets",
                "-show_entries",
                "packet=pos",
                "-of",
                "json",
            ])
            .arg(&seed)
            .output()
            .unwrap();
        assert!(output.status.success() && output.stderr.is_empty());
        let packets: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let last = packets["packets"].as_array().unwrap().last().unwrap()["pos"]
            .as_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        for (label, offset, mask) in [
            (
                "configuration",
                config.header.offset + config.header.header_size,
                1,
            ),
            ("late-nal", last + 4, 0x80),
        ] {
            let mut broken = original.clone();
            broken[offset] ^= mask;
            let source = root.path().join(format!("{profile}-{label}.mp4"));
            let destination = source.with_extension("result.mp4");
            fs::write(&source, &broken).unwrap();
            assert!(
                validation
                    .validate_source(
                        VerificationSource {
                            input: &source,
                            input_keys: &InputKeys::default(),
                            hls_keys: &HlsInputKeys::default(),
                        },
                        SourceValidationPurpose::Conversion,
                    )
                    .is_err(),
                "{label} must fail complete native admission without a media tool"
            );
            let result = command.convert(ConversionRequest {
                input: &source,
                output: &destination,
                format: OutputFormat::Mp4,
                target: TargetProtection::Clear,
                input_keys: &InputKeys::default(),
                hls_input_keys: &HlsInputKeys::default(),
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            });
            let error = result.expect_err("malformed HEVC must fail conversion");
            assert!(
                matches!(
                    error.0,
                    "invalid-hevc-syntax" | "native-hevc-configuration-not-supported"
                ),
                "{label}: {error}"
            );
            assert!(!destination.exists());
            assert_eq!(fs::read(source).unwrap(), broken);
        }
        assert_eq!(fs::read(seed).unwrap(), original);
    }
    assert_tools(&validation_events, false);
    assert_tools(&events, true);
}

#[test]
fn explicit_decode_adds_complete_main_and_main10_picture_comparison() {
    let root = common::private_tempdir().unwrap();
    let empty = InputKeys::default();
    let (native, _) = converter(ProcessingMode::Auto, true, false);
    let (decoded, events) = converter(ProcessingMode::Auto, true, true);
    for (profile, (name, _)) in SEEDS.into_iter().enumerate() {
        let original = fixture(name);
        let (output, keys, _) = convert(
            &native,
            &original,
            &empty,
            &root.path().join(format!("decode-{profile}.mp4")),
            OutputFormat::Mp4,
            TargetProtection::Cbcs,
            1,
        );
        let report = verify(&decoded, &original, &empty, &output, &keys).unwrap();
        assert!(report.passed && report.checks.is_pass());
        assert_eq!(
            report.decode_evidence,
            DecodeEvidence::ContentCompared { equal: true }
        );
    }
    assert!(
        events
            .records()
            .iter()
            .any(|record| record.get("tool").and_then(serde_json::Value::as_str) == Some("ffmpeg"))
    );
}

#[test]
fn hevc_with_aac_retains_finite_audio_presentations_without_decoder_processes() {
    let root = common::private_tempdir().unwrap();
    let audio = fixture("clip.mp4");
    let original_audio = fs::read(&audio).unwrap();
    let (command, events) = converter(ProcessingMode::Auto, true, false);
    for (profile, (name, _)) in SEEDS.into_iter().enumerate() {
        let source = root.path().join(format!("joint-{profile}.mp4"));
        let status = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-nostdin", "-threads", "1", "-i"])
            .arg(fixture(name))
            .arg("-i")
            .arg(&audio)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-c",
                "copy",
                "-map_metadata",
                "-1",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(status.success());
        let original = fs::read(&source).unwrap();
        let mut current = source.clone();
        let mut keys = InputKeys::default();
        for (ordinal, target) in [
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
            TargetProtection::Clear,
        ]
        .into_iter()
        .enumerate()
        {
            let destination = root.path().join(format!("joint-{profile}-{ordinal}.mp4"));
            let (result, next_keys, _) = convert(
                &command,
                &current,
                &keys,
                &destination,
                OutputFormat::Mp4,
                target,
                ordinal % 2,
            );
            assert_native(
                &verify(
                    &command,
                    &source,
                    &InputKeys::default(),
                    &result,
                    &next_keys,
                )
                .unwrap(),
            );
            current = result;
            keys = next_keys;
        }
        assert_eq!(fs::read(source).unwrap(), original);
    }
    assert_eq!(fs::read(audio).unwrap(), original_audio);
    assert_tools(&events, true);
}
