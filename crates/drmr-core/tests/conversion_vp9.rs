//! Real VP9 profile/depth pairs retain complete native sample and presentation evidence.
mod common;

use drmr_core::{
    conversion::{
        Aes128Key, ContentKey, ConversionRequest, Converter, DecodeEvidence, HlsInputKeys,
        HlsOutputKey, InputKeys, OutputKey, ProcessingMode, SourceValidationPurpose,
        ValidationDepth, VerificationReport, VerificationRequest, VerificationSource,
        manifest_lint::ManifestProfile,
    },
    events::Collector,
    inspection::{OutputFormat, TargetProtection},
    toolrun::{Secrets, ToolRunner},
};
use sha2::{Digest as _, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

const SEEDS: [(&str, &str); 6] = [
    (
        "vp9-p0-8.mp4",
        "d9bb653ddf7b7011b4739973f5d6bc17009057d4dac46d68c84e0b006bdc0314",
    ),
    (
        "vp9-p1-8.mp4",
        "cb49f95665a8e345a27e29d04c6ce0c16e5d61f75968981768c0720e1e7b3025",
    ),
    (
        "vp9-p2-10.mp4",
        "7e58323555be2e6e04eaf630da55ef7edc7b81a06fd5d13e96514b27d1cba44d",
    ),
    (
        "vp9-p2-12.mp4",
        "f021996104adc0d35370a8ad469a873d39b77bef4237aaad1038dac516128e21",
    ),
    (
        "vp9-p3-10.mp4",
        "abfd16dbd14f30acfce852e0e8f59776e9c47287725ce4b2fdd5027ed65544e4",
    ),
    (
        "vp9-p3-12.mp4",
        "96c93b12834b2997c17d39f5da73a89154d20c932bfaff322c2276b292457d02",
    ),
];
const IDS: [&str; 2] = [
    "000102030405060708090a0b0c0d0e0f",
    "404142434445464748494a4b4c4d4e4f",
];
const KEYS: [&str; 2] = [
    "101112131415161718191a1b1c1d1e1f",
    "505152535455565758595a5b5c5d5e5f",
];
const URI: &str = "https://operator.invalid/vp9-fixture-key";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name)
}

fn command(mode: ProcessingMode, packaging: bool, decode: bool) -> (Converter, Arc<Collector>) {
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
    InputKeys::new(vec![ContentKey::from_hex(IDS[index], KEYS[index]).unwrap()]).unwrap()
}

fn native(report: &VerificationReport) {
    assert!(report.passed && report.checks.is_pass(), "{report:?}");
    assert_eq!(report.decode_evidence, DecodeEvidence::not_requested());
    assert_eq!(report.required_depth, ValidationDepth::Native);
    assert!(!report.license_playback_verified);
    if let Some(manifest) = &report.native_manifest {
        assert!(manifest.is_clean_for_scope());
    }
}

fn no_decoders(events: &Collector, packaging: bool) {
    for record in events.records() {
        if record.get("event").and_then(serde_json::Value::as_str) == Some("command") {
            assert!(packaging, "native route attempted an external media tool");
            assert_eq!(
                record.get("tool").and_then(serde_json::Value::as_str),
                Some("packager")
            );
        }
        let text = serde_json::to_string(&record).unwrap();
        assert!(KEYS.iter().all(|key| !text.contains(key)) && !text.contains(URI));
    }
}

fn verify(
    c: &Converter,
    source: &Path,
    input_keys: &InputKeys,
    output: &Path,
    output_keys: &InputKeys,
) -> Result<VerificationReport, drmr_core::conversion::ConversionError> {
    let hls = HlsInputKeys::default();
    c.verify(VerificationRequest {
        source: VerificationSource {
            input: source,
            input_keys,
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
    c: &Converter,
    input: &Path,
    keys: &InputKeys,
    output: &Path,
    format: OutputFormat,
    target: TargetProtection,
    key_index: usize,
) -> (PathBuf, InputKeys) {
    let key = OutputKey::from_hex(IDS[key_index], KEYS[key_index]).unwrap();
    let protected = target == TargetProtection::Cenc;
    let result = c
        .convert(ConversionRequest {
            input,
            output,
            format,
            target,
            input_keys: keys,
            hls_input_keys: &HlsInputKeys::default(),
            output_key: protected.then_some(&key),
            hls_key_uri: (protected && format == OutputFormat::Hls).then_some(URI),
            hls_output_key: None,
        })
        .unwrap_or_else(|error| panic!("{} {}: {error}", format.as_str(), target.as_str()));
    native(&result.verification);
    let entry = match format {
        OutputFormat::Mp4 => output.to_owned(),
        OutputFormat::Dash => output.join("manifest.mpd"),
        OutputFormat::Hls => output.join("master.m3u8"),
        OutputFormat::HlsTs => unreachable!(),
    };
    let output_keys = if protected {
        input_keys(key_index)
    } else {
        InputKeys::default()
    };
    native(&verify(c, input, keys, &entry, &output_keys).unwrap());
    (entry, output_keys)
}

#[test]
fn all_six_real_vp9_profile_depth_pairs_have_native_admission_without_tools() {
    let (c, events) = command(ProcessingMode::Auto, false, false);
    let keys = InputKeys::default();
    let hls = HlsInputKeys::default();
    for (name, digest) in SEEDS {
        let input = fixture(name);
        let bytes = fs::read(&input).unwrap();
        assert_eq!(hex::encode(Sha256::digest(&bytes)), digest);
        let result = c
            .validate_source(
                VerificationSource {
                    input: &input,
                    input_keys: &keys,
                    hls_keys: &hls,
                },
                SourceValidationPurpose::Conversion,
            )
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        native(&result.verification);
        native(&verify(&c, &input, &keys, &input, &keys).unwrap());
        assert_eq!(fs::read(input).unwrap(), bytes);
    }
    no_decoders(&events, false);
}

fn vpcc(path: &Path) -> Vec<u8> {
    let bytes = fs::read(path).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&bytes).unwrap();
    drmr_core::mp4::walk(&boxes)
        .into_iter()
        .find(|node| node.kind() == *b"vpcC")
        .unwrap()
        .header
        .payload(&bytes)
        .to_vec()
}

#[test]
fn undefined_vp9_level_roundtrips_with_exact_configuration_and_manifest_codecs() {
    let root = common::private_tempdir().unwrap();
    let seed = fixture(SEEDS[0].0);
    let original = fs::read(&seed).unwrap();
    let boxes = drmr_core::mp4::parse_boxes(&original).unwrap();
    let config = drmr_core::mp4::walk(&boxes)
        .into_iter()
        .find(|node| node.kind() == *b"vpcC")
        .unwrap();
    let mut bytes = original.clone();
    bytes[config.header.offset + config.header.header_size + 5] = 0;
    let source = root.path().join("undefined-level.mp4");
    fs::write(&source, &bytes).unwrap();
    let expected = vpcc(&source);
    assert_eq!(expected[5], 0);
    let empty = InputKeys::default();
    let (c, events) = command(ProcessingMode::Stitch, true, false);
    let (fresh, fresh_events) = command(ProcessingMode::Auto, false, false);
    let qualified = fresh
        .validate_source(
            VerificationSource {
                input: &source,
                input_keys: &empty,
                hls_keys: &HlsInputKeys::default(),
            },
            SourceValidationPurpose::Conversion,
        )
        .unwrap();
    native(&qualified.verification);
    let (encrypted, keys) = convert(
        &c,
        &source,
        &empty,
        &root.path().join("cenc.mp4"),
        OutputFormat::Mp4,
        TargetProtection::Cenc,
        0,
    );
    let (clear, _) = convert(
        &c,
        &encrypted,
        &keys,
        &root.path().join("clear.mp4"),
        OutputFormat::Mp4,
        TargetProtection::Clear,
        0,
    );
    for path in [&encrypted, &clear] {
        assert_eq!(vpcc(path), expected);
    }
    native(&verify(&fresh, &source, &empty, &encrypted, &keys).unwrap());
    native(&verify(&fresh, &source, &empty, &clear, &empty).unwrap());
    assert_undefined_level_presentations(&c, &fresh, &source, root.path(), &expected);
    // A meaningful declared level also survives the tool's lower inferred level.
    bytes[config.header.offset + config.header.header_size + 5] = 20;
    let declared = root.path().join("declared-level.mp4");
    fs::write(&declared, &bytes).unwrap();
    let (clear, _) = convert(
        &c,
        &declared,
        &empty,
        &root.path().join("declared-clear.mp4"),
        OutputFormat::Mp4,
        TargetProtection::Clear,
        0,
    );
    assert_eq!(vpcc(&clear), vpcc(&declared));
    assert_eq!(
        fs::read(&source).unwrap()[config.header.offset + config.header.header_size + 5],
        0
    );
    assert_eq!(fs::read(&seed).unwrap(), original);
    no_decoders(&events, true);
    no_decoders(&fresh_events, false);
}

fn assert_undefined_level_presentations(
    c: &Converter,
    fresh: &Converter,
    source: &Path,
    root: &Path,
    expected: &[u8],
) {
    let empty = InputKeys::default();
    for format in [OutputFormat::Hls, OutputFormat::Dash] {
        for target in [TargetProtection::Clear, TargetProtection::Cenc] {
            let output = root.join(format!("{}-{}", format.as_str(), target.as_str()));
            let (entry, keys) = convert(c, source, &empty, &output, format, target, 0);
            assert_eq!(vpcc(&output.join("track-0000/init.mp4")), expected);
            let manifest = fs::read_to_string(&entry).unwrap();
            assert!(
                manifest.contains("vp09.00.00.08.00.02.02.02.00"),
                "{manifest}"
            );
            native(&verify(fresh, source, &empty, &entry, &keys).unwrap());
        }
    }
    assert_outer_aes128_roundtrip(c, source, root);
    let master = fs::read_to_string(root.join("outer-aes128/master.m3u8")).unwrap();
    assert!(master.contains("vp09.00.00.08.00.02.02.02.00"), "{master}");
    assert_eq!(
        vpcc(&root.join("outer-clear/track-0000/init.mp4")),
        expected
    );
    let outer_keys =
        HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEYS[0]).unwrap())]).unwrap();
    native(
        &fresh
            .verify(VerificationRequest {
                source: VerificationSource {
                    input: source,
                    input_keys: &empty,
                    hls_keys: &HlsInputKeys::default(),
                },
                output: VerificationSource {
                    input: &root.join("outer-aes128/master.m3u8"),
                    input_keys: &empty,
                    hls_keys: &outer_keys,
                },
            })
            .unwrap(),
    );
    native(
        &verify(
            fresh,
            source,
            &empty,
            &root.join("outer-clear/master.m3u8"),
            &empty,
        )
        .unwrap(),
    );
}

#[test]
fn all_profiles_roundtrip_clear_cenc_rekey_clear_and_explicitly_compare_decoded_pictures() {
    let root = common::private_tempdir().unwrap();
    let empty = InputKeys::default();
    let (c, events) = command(ProcessingMode::Auto, true, false);
    let (fresh, fresh_events) = command(ProcessingMode::Auto, false, false);
    let (decode, decoded_events) = command(ProcessingMode::Auto, false, true);
    for (profile, (name, _)) in SEEDS.into_iter().enumerate() {
        let seed = fixture(name);
        let original = fs::read(&seed).unwrap();
        let mut current = seed.clone();
        let mut keys = InputKeys::default();
        for (step, target) in [
            TargetProtection::Cenc,
            TargetProtection::Cenc,
            TargetProtection::Clear,
        ]
        .into_iter()
        .enumerate()
        {
            let (output, output_keys) = convert(
                &c,
                &current,
                &keys,
                &root.path().join(format!("{profile}-{step}.mp4")),
                OutputFormat::Mp4,
                target,
                step % 2,
            );
            native(&verify(&fresh, &seed, &empty, &output, &output_keys).unwrap());
            if step == 0 {
                let proof = verify(&decode, &seed, &empty, &output, &output_keys).unwrap();
                assert!(proof.passed && proof.checks.is_pass());
                assert_eq!(proof.required_depth, ValidationDepth::Decoded);
                assert_eq!(
                    proof.decode_evidence,
                    DecodeEvidence::ContentCompared { equal: true }
                );
                let wrong =
                    InputKeys::new(vec![ContentKey::from_hex(IDS[0], KEYS[1]).unwrap()]).unwrap();
                assert!(
                    !verify(&fresh, &seed, &empty, &output, &wrong)
                        .is_ok_and(|report| report.passed)
                );
                let unpublished = root.path().join(format!("missing-key-{profile}.mp4"));
                let refused = c.convert(ConversionRequest {
                    input: &output,
                    output: &unpublished,
                    format: OutputFormat::Mp4,
                    target: TargetProtection::Clear,
                    input_keys: &empty,
                    hls_input_keys: &HlsInputKeys::default(),
                    output_key: None,
                    hls_key_uri: None,
                    hls_output_key: None,
                });
                assert_eq!(refused.unwrap_err().0, "input-key-required");
                assert!(!unpublished.exists());
            }
            current = output;
            keys = output_keys;
        }
        let cbcs = root.path().join(format!("nonstandard-cbcs-{profile}.mp4"));
        let output_key = OutputKey::from_hex(IDS[0], KEYS[0]).unwrap();
        let refused = c.convert(ConversionRequest {
            input: &seed,
            output: &cbcs,
            format: OutputFormat::Mp4,
            target: TargetProtection::Cbcs,
            input_keys: &empty,
            hls_input_keys: &HlsInputKeys::default(),
            output_key: Some(&output_key),
            hls_key_uri: None,
            hls_output_key: None,
        });
        assert_eq!(
            refused.unwrap_err().0,
            "vp9-protection-scheme-not-supported"
        );
        assert!(!cbcs.exists());
        assert_eq!(fs::read(seed).unwrap(), original);
    }
    no_decoders(&events, true);
    no_decoders(&fresh_events, false);
    assert!(
        decoded_events
            .records()
            .iter()
            .any(|record| record.get("tool").and_then(serde_json::Value::as_str) == Some("ffmpeg"))
    );
}

#[test]
fn vp9_with_aac_roundtrips_a_joint_mp4_with_native_verification() {
    let root = common::private_tempdir().unwrap();
    let source = root.path().join("vp9-aac.mp4");
    let prepared = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i"])
        .arg(fixture(SEEDS[0].0))
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
            "-shortest",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "independent joint fixture preparation failed"
    );
    let inspected = drmr_core::inspection::inspect(&source).unwrap();
    assert_eq!(inspected.tracks.len(), 2);
    assert_eq!(inspected.tracks[0].codec.as_deref(), Some("vp09"));
    assert_eq!(inspected.tracks[1].codec.as_deref(), Some("mp4a"));
    let original = fs::read(&source).unwrap();
    let empty = InputKeys::default();
    let (c, events) = command(ProcessingMode::Auto, true, false);
    let (fresh, fresh_events) = command(ProcessingMode::Auto, false, false);
    let proof = fresh
        .validate_source(
            VerificationSource {
                input: &source,
                input_keys: &empty,
                hls_keys: &HlsInputKeys::default(),
            },
            SourceValidationPurpose::Conversion,
        )
        .unwrap();
    native(&proof.verification);
    let (encrypted, keys) = convert(
        &c,
        &source,
        &empty,
        &root.path().join("joint-cenc.mp4"),
        OutputFormat::Mp4,
        TargetProtection::Cenc,
        0,
    );
    let (clear, _) = convert(
        &c,
        &encrypted,
        &keys,
        &root.path().join("joint-clear.mp4"),
        OutputFormat::Mp4,
        TargetProtection::Clear,
        0,
    );
    native(&verify(&fresh, &source, &empty, &clear, &empty).unwrap());
    assert_eq!(fs::read(&source).unwrap(), original);
    no_decoders(&events, true);
    no_decoders(&fresh_events, false);
}

fn video_playlist(entry: &Path) -> PathBuf {
    let text = fs::read_to_string(entry).unwrap();
    entry.parent().unwrap().join(
        text.lines()
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .unwrap(),
    )
}

fn last_resource(entry: &Path) -> PathBuf {
    let playlist = video_playlist(entry);
    let text = fs::read_to_string(&playlist).unwrap();
    playlist.parent().unwrap().join(
        text.lines()
            .rev()
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .unwrap(),
    )
}

fn late_payload_clock_sync_offsets(bytes: &[u8]) -> Vec<usize> {
    let nodes = drmr_core::mp4::parse_boxes(bytes).unwrap();
    let mut offsets = Vec::new();
    for node in drmr_core::mp4::walk(&nodes) {
        let at = node.header.offset + node.header.header_size;
        let payload = node.header.payload(bytes);
        match &node.kind() {
            b"mdat" => offsets.push(at + payload.len() / 2),
            b"tfdt" => offsets.push(at + if payload[0] == 0 { 7 } else { 11 }),
            b"tfhd" => {
                let flags = u32::from_be_bytes(payload[..4].try_into().unwrap()) & 0x00ff_ffff;
                if flags & 0x20 != 0 {
                    let field = 8
                        + usize::from(flags & 1 != 0) * 8
                        + usize::from(flags & 2 != 0) * 4
                        + usize::from(flags & 8 != 0) * 4
                        + usize::from(flags & 0x10 != 0) * 4;
                    offsets.push(at + field + 1);
                }
            }
            b"trun" => {
                let flags = u32::from_be_bytes(payload[..4].try_into().unwrap()) & 0x00ff_ffff;
                let base = 8 + usize::from(flags & 1 != 0) * 4;
                if flags & 4 != 0 {
                    offsets.push(at + base + 1);
                } else if flags & 0x400 != 0 {
                    offsets.push(
                        at + base
                            + usize::from(flags & 0x100 != 0) * 4
                            + usize::from(flags & 0x200 != 0) * 4
                            + 1,
                    );
                }
            }
            _ => {}
        }
    }
    assert!(offsets.len() >= 3);
    offsets
}

fn merge_first_two_resources(entry: &Path) {
    let playlist = video_playlist(entry);
    let mut lines: Vec<_> = fs::read_to_string(&playlist)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
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
    let path = root.join(&lines[first + 1]);
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend(fs::read(root.join(&lines[second + 1])).unwrap());
    fs::write(path, bytes).unwrap();
    lines[first] = format!("#EXTINF:{:.9},", duration + next);
    let bound = intervals
        .iter()
        .skip(2)
        .fold(duration + next, |maximum, (_, duration)| {
            maximum.max(*duration)
        })
        .ceil();
    let target = lines
        .iter()
        .position(|line| line.starts_with("#EXT-X-TARGETDURATION:"))
        .unwrap();
    lines[target] = format!("#EXT-X-TARGETDURATION:{bound:.0}");
    lines.drain(second..=second + 1);
    fs::write(playlist, lines.join("\n") + "\n").unwrap();
}

fn assert_manifest_and_late_mutations(
    fresh: &Converter,
    segments: &Converter,
    seed: &Path,
    clear_hls: &Path,
    dash: &Path,
    profile: usize,
) {
    let empty = InputKeys::default();
    assert!(
        fresh
            .lint_manifest_with_profile(clear_hls, OutputFormat::Hls, ManifestProfile::Base)
            .unwrap()
            .is_clean_for_scope()
    );
    let apple = fresh
        .lint_manifest_with_profile(clear_hls, OutputFormat::Hls, ManifestProfile::AppleVod)
        .unwrap();
    assert!(!apple.is_clean_for_scope());
    assert!(
        apple
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.reference.section == "1.1"
                && diagnostic.severity == drmr_manifest::Severity::Error),
        "VP9 must fail the Apple video-codec rule independently of variant-count rules"
    );
    let media = last_resource(clear_hls);
    let intact = fs::read(&media).unwrap();
    for offset in late_payload_clock_sync_offsets(&intact) {
        let mut changed = intact.clone();
        changed[offset] ^= 1;
        fs::write(&media, changed).unwrap();
        assert!(!verify(fresh, seed, &empty, clear_hls, &empty).is_ok_and(|report| report.passed));
    }
    fs::write(&media, intact).unwrap();
    native(&verify(fresh, seed, &empty, clear_hls, &empty).unwrap());
    if profile == 0 {
        let playlist = video_playlist(clear_hls);
        let text = fs::read_to_string(&playlist).unwrap();
        let mut lines: Vec<_> = text.lines().map(str::to_owned).collect();
        let interval = lines
            .iter()
            .position(|line| line.starts_with("#EXTINF:"))
            .unwrap();
        lines[interval] = "#EXTINF:3.000000000,".into();
        fs::write(&playlist, lines.join("\n") + "\n").unwrap();
        assert!(!verify(fresh, seed, &empty, clear_hls, &empty).is_ok_and(|report| report.passed));
        fs::write(playlist, text).unwrap();
        merge_first_two_resources(clear_hls);
        native(&verify(fresh, seed, &empty, clear_hls, &empty).unwrap());
        assert!(
            !verify(segments, dash, &empty, clear_hls, &empty).is_ok_and(|report| report.passed)
        );
    }
}

#[test]
fn all_profiles_cross_fmp4_hls_dash_with_native_segments_and_selected_stitch() {
    let root = common::private_tempdir().unwrap();
    let empty = InputKeys::default();
    let (auto, auto_events) = command(ProcessingMode::Auto, true, false);
    let (segments, segment_events) = command(ProcessingMode::Segments, false, false);
    let (fresh, fresh_events) = command(ProcessingMode::Auto, false, false);
    for (profile, (name, _)) in SEEDS.into_iter().enumerate() {
        let seed = fixture(name);
        let original = fs::read(&seed).unwrap();
        let (dash, _) = convert(
            &auto,
            &seed,
            &empty,
            &root.path().join(format!("dash-{profile}")),
            OutputFormat::Dash,
            TargetProtection::Clear,
            0,
        );
        let baseline = common::snapshot_tree(dash.parent().unwrap());
        let (hls, hls_keys) = convert(
            &segments,
            &dash,
            &empty,
            &root.path().join(format!("hls-cenc-{profile}")),
            OutputFormat::Hls,
            TargetProtection::Cenc,
            0,
        );
        native(&verify(&fresh, &seed, &empty, &hls, &hls_keys).unwrap());
        let (clear_dash, _) = convert(
            &segments,
            &hls,
            &hls_keys,
            &root.path().join(format!("dash-clear-{profile}")),
            OutputFormat::Dash,
            TargetProtection::Clear,
            0,
        );
        let (clear_hls, _) = convert(
            &segments,
            &clear_dash,
            &empty,
            &root.path().join(format!("hls-clear-{profile}")),
            OutputFormat::Hls,
            TargetProtection::Clear,
            0,
        );
        native(&verify(&fresh, &seed, &empty, &clear_hls, &empty).unwrap());
        if matches!(profile, 0 | 2) {
            let (stitch, stitch_events) = command(ProcessingMode::Stitch, true, false);
            let (rekeyed, keys) = convert(
                &stitch,
                &hls,
                &hls_keys,
                &root.path().join(format!("stitched-dash-{profile}")),
                OutputFormat::Dash,
                TargetProtection::Cenc,
                1,
            );
            let (file, _) = convert(
                &stitch,
                &rekeyed,
                &keys,
                &root.path().join(format!("from-dash-{profile}.mp4")),
                OutputFormat::Mp4,
                TargetProtection::Clear,
                0,
            );
            native(&verify(&fresh, &seed, &empty, &file, &empty).unwrap());
            no_decoders(&stitch_events, true);
        }
        assert_manifest_and_late_mutations(&fresh, &segments, &seed, &clear_hls, &dash, profile);
        assert_eq!(common::snapshot_tree(dash.parent().unwrap()), baseline);
        assert_eq!(fs::read(seed).unwrap(), original);
    }
    no_decoders(&auto_events, true);
    no_decoders(&segment_events, false);
    no_decoders(&fresh_events, false);
}

fn assert_malformed_native_source(validation: &Converter, input: &Path, name: &str, label: &str) {
    let failure = validation
        .validate_source(
            VerificationSource {
                input,
                input_keys: &InputKeys::default(),
                hls_keys: &HlsInputKeys::default(),
            },
            SourceValidationPurpose::Conversion,
        )
        .unwrap_err();
    assert_eq!(
        failure.0,
        if label == "configuration-version" {
            "native-vp9-configuration-not-supported"
        } else {
            "invalid-vp9-syntax"
        },
        "{name}/{label} must fail native admission"
    );
}

#[test]
fn malformed_vpcc_late_frame_markers_and_superframe_lengths_never_publish() {
    let root = common::private_tempdir().unwrap();
    let (validation, validation_events) = command(ProcessingMode::Auto, false, false);
    let (c, events) = command(ProcessingMode::Auto, true, false);
    for (profile, (name, _)) in SEEDS.into_iter().enumerate() {
        let seed = fixture(name);
        let original = fs::read(&seed).unwrap();
        let boxes = drmr_core::mp4::parse_boxes(&original).unwrap();
        let config = drmr_core::mp4::walk(&boxes)
            .into_iter()
            .find(|node| node.kind() == *b"vpcC")
            .unwrap();
        let result = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_packets",
                "-show_entries",
                "packet=pos,size",
                "-of",
                "json",
            ])
            .arg(&seed)
            .output()
            .unwrap();
        assert!(result.status.success() && result.stderr.is_empty());
        let packets: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        let ranges: Vec<_> = packets["packets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|packet| {
                let start = packet["pos"].as_str().unwrap().parse::<usize>().unwrap();
                let size = packet["size"].as_str().unwrap().parse::<usize>().unwrap();
                start..start + size
            })
            .collect();
        let late = ranges.last().unwrap().start;
        let indexed = ranges
            .iter()
            .rev()
            .find(|range| original[range.end - 1] & 0xe0 == 0xc0)
            .unwrap();
        let marker = original[indexed.end - 1];
        let index_size = 2 + usize::from((marker & 7) + 1) * usize::from(((marker >> 3) & 3) + 1);
        for (label, offset, mask) in [
            (
                "configuration-version",
                config.header.offset + config.header.header_size,
                1,
            ),
            ("late-frame-marker", late, 0x40),
            ("late-superframe-length", indexed.end - index_size + 1, 1),
        ] {
            let mut broken = original.clone();
            broken[offset] ^= mask;
            let input = root.path().join(format!("{profile}-{label}.mp4"));
            let output = input.with_extension("output.mp4");
            fs::write(&input, &broken).unwrap();
            assert_malformed_native_source(&validation, &input, name, label);
            let result = c.convert(ConversionRequest {
                input: &input,
                output: &output,
                format: OutputFormat::Mp4,
                target: TargetProtection::Clear,
                input_keys: &InputKeys::default(),
                hls_input_keys: &HlsInputKeys::default(),
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            });
            assert!(result.is_err(), "{label} must prevent conversion");
            assert!(!output.exists());
            assert_eq!(fs::read(input).unwrap(), broken);
        }
        assert_eq!(fs::read(seed).unwrap(), original);
    }
    no_decoders(&events, true);
    no_decoders(&validation_events, false);
}

fn dependent_source(root: &Path) -> PathBuf {
    fs::create_dir(root).unwrap();
    let seed = fixture(SEEDS[0].0);
    let result = std::process::Command::new("packager")
        .arg("--single_threaded")
        .arg(format!(
            "input={},stream=video,init_segment={},segment_template={},playlist_name=video.m3u8",
            seed.display(),
            root.join("init.mp4").display(),
            root.join("segment-$Number$.m4s").display(),
        ))
        .arg("--hls_master_playlist_output")
        .arg(root.join("master.m3u8"))
        .args([
            "--segment_duration",
            "1",
            "--fragment_duration",
            "1",
            "--segment_sap_aligned=false",
            "--fragment_sap_aligned=false",
            "--mp4_reset_initial_composition_offset_to_zero=false",
        ])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "independent non-SAP fixture preparation failed"
    );
    let second = root.join("second-resource-probe.mp4");
    let mut bytes = fs::read(root.join("init.mp4")).unwrap();
    bytes.extend(fs::read(root.join("segment-2.m4s")).unwrap());
    fs::write(&second, bytes).unwrap();
    let result = std::process::Command::new("ffprobe")
        .args([
            "-v",
            "quiet",
            "-show_packets",
            "-show_entries",
            "packet=flags",
            "-of",
            "json",
        ])
        .arg(&second)
        .output()
        .unwrap();
    assert!(result.status.success());
    let packets: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(packets["packets"].as_array().unwrap().len(), 24);
    assert!(
        !packets["packets"][0]["flags"]
            .as_str()
            .unwrap()
            .contains('K')
    );
    fs::remove_file(second).unwrap();
    root.join("master.m3u8")
}

fn assert_cbcs_refusal(source: &Path, root: &Path) {
    let empty = InputKeys::default();
    let cbcs = root.join("independent-cbcs.mp4");
    let seed = fixture(SEEDS[0].0);
    let prepared = std::process::Command::new("packager")
        .arg("--single_threaded")
        .arg(format!(
            "input={},stream=video,output={},drm_label=MEDIA",
            seed.display(),
            cbcs.display(),
        ))
        .args([
            "--enable_raw_key_encryption",
            "--keys",
            &format!("label=MEDIA:key_id={}:key={}", IDS[0], KEYS[0]),
            "--protection_scheme",
            "cbcs",
            "--clear_lead",
            "0",
            "--segment_duration",
            "2",
            "--mp4_reset_initial_composition_offset_to_zero=false",
        ])
        .output()
        .unwrap();
    assert!(
        prepared.status.success(),
        "independent CBCS reference preparation failed"
    );
    let cbcs_bytes = fs::read(&cbcs).unwrap();
    let cbcs_keys = input_keys(0);
    let (admission, admission_events) = command(ProcessingMode::Auto, false, false);
    let error = admission
        .validate_source(
            VerificationSource {
                input: &cbcs,
                input_keys: &cbcs_keys,
                hls_keys: &HlsInputKeys::default(),
            },
            SourceValidationPurpose::Conversion,
        )
        .unwrap_err();
    assert_eq!(error.0, "vp9-protection-scheme-not-supported");
    for purpose in [
        SourceValidationPurpose::Conversion,
        SourceValidationPurpose::Verification,
    ] {
        let error = admission
            .validate_source(
                VerificationSource {
                    input: &cbcs,
                    input_keys: &empty,
                    hls_keys: &HlsInputKeys::default(),
                },
                purpose,
            )
            .unwrap_err();
        assert_eq!(error.0, "vp9-protection-scheme-not-supported");
    }
    assert_eq!(fs::read(cbcs).unwrap(), cbcs_bytes);
    no_decoders(&admission_events, false);
    let (stitch, stitch_events) = command(ProcessingMode::Stitch, false, false);
    let output_key = OutputKey::from_hex(IDS[0], KEYS[0]).unwrap();
    for format in [OutputFormat::Mp4, OutputFormat::Hls, OutputFormat::Dash] {
        let destination = root.join(format!("refused-stitch-{}", format.as_str()));
        let error = stitch
            .convert(ConversionRequest {
                input: source,
                output: &destination,
                format,
                target: TargetProtection::Cbcs,
                input_keys: &empty,
                hls_input_keys: &HlsInputKeys::default(),
                output_key: Some(&output_key),
                hls_key_uri: (format == OutputFormat::Hls).then_some(URI),
                hls_output_key: None,
            })
            .unwrap_err();
        assert_eq!(error.0, "vp9-protection-scheme-not-supported", "{format:?}");
        assert!(!destination.exists());
    }
    no_decoders(&stitch_events, false);
}

fn assert_outer_aes128_roundtrip(c: &Converter, source: &Path, root: &Path) {
    let empty = InputKeys::default();
    let outer_key = HlsOutputKey::new(URI, Aes128Key::from_hex(KEYS[0]).unwrap()).unwrap();
    let outer_keys =
        HlsInputKeys::new(vec![(URI.into(), Aes128Key::from_hex(KEYS[0]).unwrap())]).unwrap();
    let destination = root.join("outer-aes128");
    let result = c
        .convert(ConversionRequest {
            input: source,
            output: &destination,
            format: OutputFormat::Hls,
            target: TargetProtection::HlsAes128,
            input_keys: &empty,
            hls_input_keys: &HlsInputKeys::default(),
            output_key: None,
            hls_key_uri: None,
            hls_output_key: Some(&outer_key),
        })
        .unwrap();
    native(&result.verification);
    let encrypted = destination.join("master.m3u8");
    let hls = HlsInputKeys::default();
    native(
        &c.verify(VerificationRequest {
            source: VerificationSource {
                input: source,
                input_keys: &empty,
                hls_keys: &hls,
            },
            output: VerificationSource {
                input: &encrypted,
                input_keys: &empty,
                hls_keys: &outer_keys,
            },
        })
        .unwrap(),
    );
    let destination = root.join("outer-clear");
    let result = c
        .convert(ConversionRequest {
            input: &encrypted,
            output: &destination,
            format: OutputFormat::Hls,
            target: TargetProtection::Clear,
            input_keys: &empty,
            hls_input_keys: &outer_keys,
            output_key: None,
            hls_key_uri: None,
            hls_output_key: None,
        })
        .unwrap();
    native(&result.verification);
    native(&verify(c, source, &empty, &destination.join("master.m3u8"), &empty).unwrap());
}

#[test]
fn dependent_one_second_resources_keep_vp9_state_and_outer_aes128_is_separate_from_cbcs() {
    let root = common::private_tempdir().unwrap();
    let source = dependent_source(&root.path().join("source"));
    let original = common::snapshot_tree(source.parent().unwrap());
    let empty = InputKeys::default();
    let (c, events) = command(ProcessingMode::Segments, false, false);
    assert_cbcs_refusal(&source, root.path());
    let (encrypted, keys) = convert(
        &c,
        &source,
        &empty,
        &root.path().join("cenc"),
        OutputFormat::Hls,
        TargetProtection::Cenc,
        0,
    );
    let (clear, _) = convert(
        &c,
        &encrypted,
        &keys,
        &root.path().join("clear"),
        OutputFormat::Hls,
        TargetProtection::Clear,
        0,
    );
    native(&verify(&c, &source, &empty, &clear, &empty).unwrap());
    assert_outer_aes128_roundtrip(&c, &source, root.path());
    assert_eq!(common::snapshot_tree(source.parent().unwrap()), original);
    no_decoders(&events, false);
}
