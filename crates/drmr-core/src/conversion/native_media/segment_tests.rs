use super::*;
use crate::inspection::TargetProtection;
use sha2::Digest as _;
use std::{collections::BTreeMap, process::Command};

const SOURCE_ID: &str = "000102030405060708090a0b0c0d0e0f";
const SOURCE_KEY: &str = "101112131415161718191a1b1c1d1e1f";
const OUTPUT_ID: &str = "404142434445464748494a4b4c4d4e4f";
const OUTPUT_KEY: &str = "00112233445566778899aabbccddeeff";

fn run_tool(command: &mut Command) -> Vec<u8> {
    let result = command.output().unwrap();
    assert!(
        result.status.success(),
        "independent fixture or oracle failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    if command.get_program() == "ffprobe" {
        assert!(
            result.stderr.is_empty(),
            "independent oracle reported an error: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    result.stdout
}
fn oracle(path: &Path, key: Option<(&str, &str)>) -> BTreeMap<u64, Vec<serde_json::Value>> {
    let mut command = Command::new("ffprobe");
    command.args(["-v", "error", "-ignore_editlist", "1"]);
    if let Some((id, key)) = key {
        command.args(["-decryption_keys", &format!("{id}={key}")]);
    }
    let bytes = run_tool(
        command
            .args([
                "-show_packets",
                "-show_data_hash",
                "sha256",
                "-show_entries",
                "packet=stream_index,pts,dts,duration,size,flags,data_hash:packet_side_data=",
                "-of",
                "json",
            ])
            .arg(path),
    );
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let mut tracks = BTreeMap::<u64, Vec<serde_json::Value>>::new();
    for packet in json["packets"].as_array().unwrap() {
        tracks
            .entry(packet["stream_index"].as_u64().unwrap())
            .or_default()
            .push(packet.clone());
    }
    tracks
}
fn decoded_frames(path: &Path, key: Option<(&str, &str)>) -> Vec<u8> {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-nostdin",
        "-v",
        "error",
        "-xerror",
        "-ignore_editlist",
        "1",
    ]);
    if let Some((id, key)) = key {
        command.args(["-decryption_keys", &format!("{id}={key}")]);
    }
    run_tool(command.arg("-i").arg(path).args([
        "-map",
        "0:v:0",
        "-map",
        "0:a:0",
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
fn assert_packets(
    actual: BTreeMap<u64, Vec<serde_json::Value>>,
    expected: &BTreeMap<u64, Vec<serde_json::Value>>,
    label: &str,
) {
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>(),
        "{label}"
    );
    for (track, packets) in actual {
        assert_eq!(
            packets.len(),
            expected[&track].len(),
            "{label}, track {track}"
        );
        for (ordinal, (a, b)) in packets.iter().zip(&expected[&track]).enumerate() {
            assert_eq!(a, b, "{label}, track {track}, sample {ordinal}");
        }
    }
}
fn split_resources(path: &Path) -> (Vec<u8>, Vec<Vec<u8>>) {
    let bytes = std::fs::read(path).unwrap();
    let nodes = crate::mp4::parse_boxes(&bytes).unwrap();
    let mut init = Vec::new();
    let mut segments = Vec::<Vec<u8>>::new();
    for node in nodes {
        match &node.kind() {
            b"ftyp" | b"moov" => init.extend_from_slice(node.header.bytes(&bytes)),
            b"moof" => segments.push(node.header.bytes(&bytes).to_vec()),
            b"mdat" => segments
                .last_mut()
                .unwrap()
                .extend_from_slice(node.header.bytes(&bytes)),
            b"mfra" | b"sidx" | b"free" | b"skip" => {}
            _ => panic!("unexpected independent fixture box"),
        }
    }
    (init, segments)
}
fn muxed_fixture(
    root: &Path,
) -> (
    std::path::PathBuf,
    Vec<std::path::PathBuf>,
    BTreeMap<u64, Vec<serde_json::Value>>,
) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let media = root.join("muxed.mp4");
    run_tool(
        Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(fixture)
            .args([
                "-map",
                "0:v:0",
                "-map",
                "0:a:0",
                "-c",
                "copy",
                "-movflags",
                "empty_moov+default_base_moof+frag_keyframe",
                "-frag_duration",
                "2000000",
            ])
            .arg(&media),
    );
    let expected = oracle(&media, None);
    let (init, mut resources) = split_resources(&media);
    assert!(resources.len() > 2);
    // The first original resource deliberately includes an internal keyframe/moof.
    // Its duration differs from the remaining resources and must not be normalized.
    let second = resources.remove(1);
    resources[0].extend(second);
    let init_path = root.join("source-init.mp4");
    std::fs::write(&init_path, init).unwrap();
    let resources = resources
        .into_iter()
        .enumerate()
        .map(|(index, bytes)| {
            let path = root.join(format!("source-{index}.m4s"));
            std::fs::write(&path, bytes).unwrap();
            path
        })
        .collect();
    (init_path, resources, expected)
}
fn combined(init: &Path, resources: &[std::path::PathBuf], output: &Path) {
    let mut bytes = std::fs::read(init).unwrap();
    for resource in resources {
        bytes.extend(std::fs::read(resource).unwrap());
    }
    std::fs::write(output, bytes).unwrap();
}
fn assert_output_auxiliary(context: &SegmentInput, media: &Path, target: TargetProtection) {
    let initialization = init::parse(&context.layout).unwrap();
    let bytes = std::fs::read(media).unwrap();
    let nodes = crate::mp4::parse_boxes(&bytes).unwrap();
    for track in &initialization.tracks {
        if target == TargetProtection::Clear {
            assert!(track.encryption.is_none());
        } else {
            let encryption = track.encryption.as_ref().unwrap();
            assert_eq!(
                encryption.iv_size,
                if target == TargetProtection::Cbcs {
                    0
                } else {
                    16
                }
            );
            assert_eq!(
                encryption.constant_iv.as_ref().map(|iv| iv.len()),
                (target == TargetProtection::Cbcs).then_some(16)
            );
            assert_eq!(
                (encryption.crypt, encryption.skip),
                if target == TargetProtection::Cbcs
                    && track.descriptor.kind == crate::inspection::TrackKind::Video
                {
                    (1, 9)
                } else {
                    (0, 0)
                }
            );
        }
    }
    for traf in crate::mp4::walk(&nodes)
        .into_iter()
        .filter(|node| node.kind() == *b"traf")
    {
        let tfhd = read::one(&traf.children, *b"tfhd")
            .unwrap()
            .header
            .payload(&bytes);
        let id = read::integer(tfhd, 4).unwrap();
        let track = initialization
            .tracks
            .iter()
            .find(|track| track.descriptor.track_id == id)
            .unwrap();
        if target == TargetProtection::Clear {
            assert!(
                traf.children
                    .iter()
                    .all(|node| !matches!(&node.kind(), b"senc" | b"saiz" | b"saio"))
            );
        } else {
            let senc = read::one(&traf.children, *b"senc")
                .unwrap()
                .header
                .payload(&bytes);
            let video = track.descriptor.kind == crate::inspection::TrackKind::Video;
            assert_eq!(read::integer(senc, 0).unwrap(), if video { 2 } else { 0 });
            if target == TargetProtection::Cbcs && !video {
                assert_eq!(senc.len(), 8);
            }
        }
    }
}

fn structural_segment() -> (
    crate::native::PrivateTempDir,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    let root = crate::native::private_tempdir().unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let stop = Stop::new();
    let mut reader = read::Reader::open(&fixture, &stop, false).unwrap();
    let layout = reader.layout().unwrap();
    reader.finish().unwrap();
    let combined = [
        test_files::boxed(*b"ftyp", b"isom\0\0\0\0isomiso6"),
        test_files::interleaved_fragments(&layout, false),
    ]
    .concat();
    let (init, parts) = crate::mp4::split_fragmented(&combined).unwrap();
    assert_eq!(parts.len(), 1);
    let init_path = root.path().join("init.mp4");
    let media_path = root.path().join("segment.m4s");
    std::fs::write(&init_path, init).unwrap();
    std::fs::write(&media_path, &parts[0]).unwrap();
    (root, init_path, media_path)
}

#[test]
fn separate_initialization_and_resource_keep_raw_sample_clocks_without_joining() {
    let (_root, init, media) = structural_segment();
    let stop = Stop::new();
    let input = SegmentInput::new(&init, &stop).unwrap();
    assert!(input.movie().fragmented);
    let evidence = input
        .evidence(&media, &InputKeys::default(), &stop)
        .unwrap();
    assert_eq!(evidence.tracks.len(), 2);
    assert_eq!(evidence.tracks[0].sample_count, 2);
    assert_eq!(evidence.tracks[0].first_dts, 0);
    assert_eq!(evidence.tracks[0].end_dts, 200);
    assert_eq!(evidence.tracks[1].sample_count, 1);
    assert_eq!(evidence.tracks[1].first_dts, 0);
    assert_eq!(evidence.tracks[1].end_dts, 100);
}

#[test]
fn cancelled_resource_scan_does_not_yield_plaintext() {
    let (_root, init, media) = structural_segment();
    let stop = Stop::new();
    let input = SegmentInput::new(&init, &stop).unwrap();
    stop.set();
    let mut callbacks = 0;
    let result = input.scan(&media, &InputKeys::default(), &stop, &mut |_, _, _| {
        callbacks += 1;
        Ok(())
    });
    assert_eq!(result.unwrap_err(), ConversionError("cancelled"));
    assert_eq!(callbacks, 0);
}

#[test]
fn resource_boundary_clock_changes_and_payload_changes_have_distinct_evidence() {
    let (_root, init, media) = structural_segment();
    let stop = Stop::new();
    let input = SegmentInput::new(&init, &stop).unwrap();
    let original = std::fs::read(&media).unwrap();
    let expected = input
        .evidence(&media, &InputKeys::default(), &stop)
        .unwrap();
    let nodes = crate::mp4::parse_boxes(&original).unwrap();
    let mdats = crate::mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"mdat")
        .unwrap();
    let mut payload = original.clone();
    payload[mdats.header.offset + mdats.header.header_size] ^= 1;
    std::fs::write(&media, payload).unwrap();
    let changed = input
        .evidence(&media, &InputKeys::default(), &stop)
        .unwrap();
    assert_ne!(expected.tracks[0].payload, changed.tracks[0].payload);
    assert_eq!(expected.tracks[0].samples, changed.tracks[0].samples);
    std::fs::write(&media, original).unwrap();
    assert_eq!(
        expected,
        input
            .evidence(&media, &InputKeys::default(), &stop)
            .unwrap()
    );
}

#[test]
fn changed_initialization_and_invalid_resource_refuse_before_plaintext_callbacks() {
    let (_root, init, media) = structural_segment();
    let stop = Stop::new();
    let input = SegmentInput::new(&init, &stop).unwrap();
    let original_init = std::fs::read(&init).unwrap();
    let mut changed = original_init.clone();
    changed[16] ^= 1;
    std::fs::write(&init, &changed).unwrap();
    let mut callbacks = 0;
    let result = input.scan(&media, &InputKeys::default(), &stop, &mut |_, _, _| {
        callbacks += 1;
        Ok(())
    });
    assert_eq!(
        result.unwrap_err(),
        ConversionError("native-source-changed")
    );
    assert_eq!(callbacks, 0);
    std::fs::write(&init, original_init).unwrap();
    let original = std::fs::read(&media).unwrap();
    std::fs::write(&media, &original[..original.len() - 1]).unwrap();
    assert!(
        input
            .scan(&media, &InputKeys::default(), &stop, &mut |_, _, _| {
                callbacks += 1;
                Ok(())
            })
            .is_err()
    );
    assert_eq!(callbacks, 0);
    std::fs::write(&media, original).unwrap();
    assert!(input.evidence(&media, &InputKeys::default(), &stop).is_ok());
}

#[test]
fn clear_resource_rewrite_preserves_original_fragment_sample_membership_and_controls() {
    let (root, init, media) = structural_segment();
    let stop = Stop::new();
    let original = SegmentInput::new(&init, &stop).unwrap();
    let before = original
        .evidence(&media, &InputKeys::default(), &stop)
        .unwrap();
    let output_init = root.path().join("clear-init.mp4");
    let output_media = root.path().join("clear-segment.m4s");
    let transformer = SegmentTransformer::new(
        &init,
        &output_init,
        crate::inspection::TargetProtection::Clear,
        None,
        &stop,
    )
    .unwrap();
    let receipt = transformer
        .rewrite_segment(&media, &output_media, &InputKeys::default(), &stop)
        .unwrap();
    let result = SegmentInput::new(&output_init, &stop).unwrap();
    assert_eq!(before, receipt);
    assert_eq!(
        before,
        result
            .evidence(&output_media, &InputKeys::default(), &stop)
            .unwrap()
    );
    assert_eq!(original.movie(), result.movie());
}

struct MatrixSource<'a> {
    context: &'a SegmentInput,
    init: &'a Path,
    resources: &'a [std::path::PathBuf],
    keys: &'a InputKeys,
    expected: &'a BTreeMap<u64, Vec<serde_json::Value>>,
    decoded: &'a [u8],
}
fn assert_target_initialization(context: &SegmentInput, path: &Path, target: TargetProtection) {
    for track in &context.movie().tracks {
        assert_eq!(
            track.protection.scheme,
            match target {
                TargetProtection::Clear => Scheme::Clear,
                TargetProtection::Cenc => Scheme::Cenc,
                _ => Scheme::Cbcs,
            }
        );
        assert_eq!(
            track.protection.key_id,
            (target != TargetProtection::Clear)
                .then(|| hex::decode(OUTPUT_ID).unwrap().try_into().unwrap())
        );
    }
    let bytes = std::fs::read(path).unwrap();
    let boxes = crate::mp4::parse_boxes(&bytes).unwrap();
    let pssh = crate::mp4::walk(&boxes)
        .into_iter()
        .filter(|node| node.kind() == *b"pssh")
        .collect::<Vec<_>>();
    assert_eq!(pssh.len(), usize::from(target != TargetProtection::Clear));
    if target != TargetProtection::Clear {
        let payload = pssh[0].header.payload(&bytes);
        assert_eq!(read::integer(payload, 0).unwrap(), 0x0100_0000);
        assert_eq!(
            &payload[4..20],
            hex::decode("1077efecc0b24d02ace33c1e52e2fb4b").unwrap()
        );
        assert_eq!(read::integer(payload, 20).unwrap(), 1);
        assert_eq!(&payload[24..40], hex::decode(OUTPUT_ID).unwrap());
        assert_eq!(read::integer(payload, 40).unwrap(), 0);
    }
}
fn assert_matrix_target(source: &MatrixSource<'_>, directory: &Path, target: TargetProtection) {
    let stop = Stop::new();
    let key = super::super::OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap();
    std::fs::create_dir(directory).unwrap();
    let output_init = directory.join("init.mp4");
    let writer =
        SegmentTransformer::new(source.init, &output_init, target, Some(&key), &stop).unwrap();
    assert_eq!(writer.source().movie(), writer.movie());
    let result_keys = if target == TargetProtection::Clear {
        InputKeys::default()
    } else {
        InputKeys::new(vec![
            super::super::ContentKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap(),
        ])
        .unwrap()
    };
    let result = SegmentInput::new(&output_init, &stop).unwrap();
    let mut resources = Vec::new();
    for (index, resource) in source.resources.iter().enumerate() {
        let path = directory.join(format!("segment-{index}.m4s"));
        let before = source
            .context
            .evidence(resource, source.keys, &stop)
            .unwrap();
        let during = writer
            .rewrite_segment(resource, &path, source.keys, &stop)
            .unwrap();
        let after = result.evidence(&path, &result_keys, &stop).unwrap();
        assert_eq!(before, during);
        assert_eq!(before, after);
        assert_output_auxiliary(&result, &path, target);
        if index == 0 {
            assert_eq!(after.fragment_count, 2);
        }
        resources.push(path);
    }
    let full = directory.join("full.mp4");
    combined(&output_init, &resources, &full);
    assert_packets(
        oracle(
            &full,
            (target != TargetProtection::Clear).then_some((OUTPUT_ID, OUTPUT_KEY)),
        ),
        source.expected,
        &format!("target {target:?}"),
    );
    assert_target_initialization(&result, &output_init, target);
    assert!(
        decoded_frames(
            &full,
            (target != TargetProtection::Clear).then_some((OUTPUT_ID, OUTPUT_KEY))
        ) == source.decoded,
        "complete decoded frame, clock and sample records differ for {target:?}"
    );
}

#[test]
fn muxed_avc_aac_all_encryption_pairs_preserve_uneven_resources_and_independent_packets() {
    let root = crate::native::private_tempdir().unwrap();
    let stop = Stop::new();
    let (original_init, original_resources, expected) = muxed_fixture(root.path());
    let decoded = decoded_frames(&root.path().join("muxed.mp4"), None);
    let original_hashes = original_resources
        .iter()
        .map(|path| std::fs::read(path).unwrap())
        .collect::<Vec<_>>();
    let source_key = super::super::OutputKey::from_hex(SOURCE_ID, SOURCE_KEY).unwrap();
    for source_scheme in [
        TargetProtection::Clear,
        TargetProtection::Cenc,
        TargetProtection::Cbcs,
    ] {
        let source_directory = root.path().join(format!("source-{source_scheme:?}"));
        std::fs::create_dir(&source_directory).unwrap();
        let source_init = source_directory.join("init.mp4");
        let source_writer = SegmentTransformer::new(
            &original_init,
            &source_init,
            source_scheme,
            Some(&source_key),
            &stop,
        )
        .unwrap();
        let mut source_resources = Vec::new();
        for (index, resource) in original_resources.iter().enumerate() {
            let path = source_directory.join(format!("segment-{index}.m4s"));
            source_writer
                .rewrite_segment(resource, &path, &InputKeys::default(), &stop)
                .unwrap();
            source_resources.push(path);
        }
        let source_keys = if source_scheme == TargetProtection::Clear {
            InputKeys::default()
        } else {
            InputKeys::new(vec![
                super::super::ContentKey::from_hex(SOURCE_ID, SOURCE_KEY).unwrap(),
            ])
            .unwrap()
        };
        let source_context = SegmentInput::new(&source_init, &stop).unwrap();
        let source_full = source_directory.join("full.mp4");
        combined(&source_init, &source_resources, &source_full);
        assert_packets(
            oracle(
                &source_full,
                (source_scheme != TargetProtection::Clear).then_some((SOURCE_ID, SOURCE_KEY)),
            ),
            &expected,
            &format!("source {source_scheme:?}"),
        );
        for target in [
            TargetProtection::Clear,
            TargetProtection::Cenc,
            TargetProtection::Cbcs,
        ] {
            let source = MatrixSource {
                context: &source_context,
                init: &source_init,
                resources: &source_resources,
                keys: &source_keys,
                expected: &expected,
                decoded: &decoded,
            };
            let directory = source_directory.join(format!("target-{target:?}"));
            assert_matrix_target(&source, &directory, target);
        }
    }
    for (path, before) in original_resources.iter().zip(original_hashes) {
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

fn shaka_source(fixture: &Path, output: &Path, scheme: &str, stream: &str) {
    run_tool(
        Command::new("packager")
            .arg(format!(
                "input={},stream={stream},output={},drm_label=MEDIA",
                crate::manifest_support::path::fspath(fixture).unwrap(),
                crate::manifest_support::path::fspath(output).unwrap(),
            ))
            .args([
                "--enable_raw_key_encryption",
                "--keys",
                &format!("label=MEDIA:key_id={SOURCE_ID}:key={SOURCE_KEY}"),
                "--protection_scheme",
                scheme,
                "--clear_lead",
                "0",
                "--segment_duration",
                "2",
                "--mp4_reset_initial_composition_offset_to_zero=false",
            ]),
    );
}

#[test]
fn independent_shaka_encrypted_resources_transform_to_every_supported_scheme() {
    let root = crate::native::private_tempdir().unwrap();
    let stop = Stop::new();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let output_key = super::super::OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap();
    let source_keys = InputKeys::new(vec![
        super::super::ContentKey::from_hex(SOURCE_ID, SOURCE_KEY).unwrap(),
    ])
    .unwrap();
    for scheme in ["cenc", "cbcs"] {
        for stream in ["video", "audio"] {
            let directory = root.path().join(format!("{scheme}-{stream}"));
            std::fs::create_dir(&directory).unwrap();
            let input_full = directory.join("shaka.mp4");
            shaka_source(&fixture, &input_full, scheme, stream);
            let expected = oracle(&input_full, Some((SOURCE_ID, SOURCE_KEY)));
            let (init_bytes, resources) = split_resources(&input_full);
            let init = directory.join("init.mp4");
            std::fs::write(&init, init_bytes).unwrap();
            let resources = resources
                .into_iter()
                .enumerate()
                .map(|(index, bytes)| {
                    let path = directory.join(format!("source-{index}.m4s"));
                    std::fs::write(&path, bytes).unwrap();
                    path
                })
                .collect::<Vec<_>>();
            let source = SegmentInput::new(&init, &stop).unwrap();
            for target in [
                TargetProtection::Clear,
                TargetProtection::Cenc,
                TargetProtection::Cbcs,
            ] {
                let target_dir = directory.join(format!("target-{target:?}"));
                std::fs::create_dir(&target_dir).unwrap();
                let target_init = target_dir.join("init.mp4");
                let writer =
                    SegmentTransformer::new(&init, &target_init, target, Some(&output_key), &stop)
                        .unwrap();
                let output_keys = if target == TargetProtection::Clear {
                    InputKeys::default()
                } else {
                    InputKeys::new(vec![
                        super::super::ContentKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap(),
                    ])
                    .unwrap()
                };
                let output_context = SegmentInput::new(&target_init, &stop).unwrap();
                let mut outputs = Vec::new();
                for (index, resource) in resources.iter().enumerate() {
                    let path = target_dir.join(format!("segment-{index}.m4s"));
                    let before = source.evidence(resource, &source_keys, &stop).unwrap();
                    assert_eq!(
                        before,
                        writer
                            .rewrite_segment(resource, &path, &source_keys, &stop)
                            .unwrap()
                    );
                    assert_eq!(
                        before,
                        output_context.evidence(&path, &output_keys, &stop).unwrap()
                    );
                    if (scheme == "cenc" && target == TargetProtection::Cenc)
                        || (scheme == "cbcs" && target == TargetProtection::Cbcs)
                    {
                        assert_eq!(
                            source
                                .protection_layout(resource, &source_keys, &stop)
                                .unwrap(),
                            output_context
                                .protection_layout(&path, &output_keys, &stop)
                                .unwrap(),
                            "subsample boundaries must match the independent Shaka reference"
                        );
                    }
                    outputs.push(path);
                }
                let full = target_dir.join("full.mp4");
                combined(&target_init, &outputs, &full);
                assert_packets(
                    oracle(
                        &full,
                        (target != TargetProtection::Clear).then_some((OUTPUT_ID, OUTPUT_KEY)),
                    ),
                    &expected,
                    &format!("Shaka {scheme}/{stream} -> {target:?}"),
                );
            }
        }
    }
}

#[test]
fn resource_change_during_callbacks_is_not_published_as_complete_evidence() {
    let (_root, init, media) = structural_segment();
    let original = std::fs::read(&media).unwrap();
    let input = SegmentInput::new(&init, &Stop::new()).unwrap();
    let mut changed = false;
    let result = input.scan(
        &media,
        &InputKeys::default(),
        &Stop::new(),
        &mut |_, _, _| {
            if !changed {
                let mut bytes = original.clone();
                *bytes.last_mut().unwrap() ^= 1;
                let rewrite = std::fs::write(&media, bytes);
                #[cfg(windows)]
                assert!(
                    rewrite.is_err(),
                    "the held native reader must deny concurrent writes"
                );
                #[cfg(not(windows))]
                rewrite.unwrap();
                changed = true;
            }
            Ok(())
        },
    );
    #[cfg(not(windows))]
    assert_eq!(
        result.unwrap_err(),
        ConversionError("native-source-changed")
    );
    #[cfg(windows)]
    {
        assert!(result.is_ok());
        assert_eq!(std::fs::read(&media).unwrap(), original);
    }
    std::fs::write(&media, original).unwrap();
    assert!(
        input
            .evidence(&media, &InputKeys::default(), &Stop::new())
            .is_ok()
    );
}

#[test]
fn invalid_codec_payload_and_missing_keys_remove_only_the_owned_incomplete_resource() {
    let root = crate::native::private_tempdir().unwrap();
    let stop = Stop::new();
    let (init, resources, _) = muxed_fixture(root.path());
    let input = SegmentInput::new(&init, &stop).unwrap();
    let mut video_byte = None;
    input
        .scan(
            &resources[0],
            &InputKeys::default(),
            &stop,
            &mut |track, info, _| {
                if track.kind == crate::inspection::TrackKind::Video && video_byte.is_none() {
                    video_byte = Some(
                        usize::try_from(info.range.start).unwrap()
                            + usize::from((track.decoder_config[4] & 3) + 1),
                    );
                }
                Ok(())
            },
        )
        .unwrap();
    let original = std::fs::read(&resources[0]).unwrap();
    let mut corrupted = original.clone();
    corrupted[video_byte.unwrap()] |= 0x80;
    std::fs::write(&resources[0], corrupted).unwrap();
    let output_key = super::super::OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap();
    let output_init = root.path().join("encrypted-init.mp4");
    let output_media = root.path().join("encrypted.m4s");
    let writer = SegmentTransformer::new(
        &init,
        &output_init,
        TargetProtection::Cbcs,
        Some(&output_key),
        &stop,
    )
    .unwrap();
    assert_eq!(
        writer
            .rewrite_segment(&resources[0], &output_media, &InputKeys::default(), &stop)
            .unwrap_err(),
        ConversionError("invalid-avc-syntax")
    );
    assert!(!output_media.exists());
    std::fs::write(&resources[0], &original).unwrap();
    writer
        .rewrite_segment(&resources[0], &output_media, &InputKeys::default(), &stop)
        .unwrap();
    let clear_init = root.path().join("clear-again-init.mp4");
    let clear_output = root.path().join("clear-again.m4s");
    let clear = SegmentTransformer::new(
        &output_init,
        &clear_init,
        TargetProtection::Clear,
        None,
        &stop,
    )
    .unwrap();
    assert_eq!(
        clear
            .rewrite_segment(&output_media, &clear_output, &InputKeys::default(), &stop)
            .unwrap_err(),
        ConversionError("input-key-required")
    );
    assert!(!clear_output.exists());
    let keys = InputKeys::new(vec![
        super::super::ContentKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap(),
    ])
    .unwrap();
    clear
        .rewrite_segment(&output_media, &clear_output, &keys, &stop)
        .unwrap();
    assert_eq!(std::fs::read(&resources[0]).unwrap(), original);
}

#[test]
fn one_transformer_rewrites_independent_resources_concurrently_without_changing_order() {
    let root = crate::native::private_tempdir().unwrap();
    let stop = Stop::new();
    let (init, resources, expected) = muxed_fixture(root.path());
    let key = super::super::OutputKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap();
    let result_init = root.path().join("parallel-init.mp4");
    let writer = SegmentTransformer::new(
        &init,
        &result_init,
        TargetProtection::Cenc,
        Some(&key),
        &stop,
    )
    .unwrap();
    let outputs = std::thread::scope(|scope| {
        let tasks = resources
            .iter()
            .enumerate()
            .map(|(index, input)| {
                let output = root.path().join(format!("parallel-{index}.m4s"));
                let writer = &writer;
                let stop = &stop;
                scope.spawn(move || {
                    let evidence = writer
                        .rewrite_segment(input, &output, &InputKeys::default(), stop)
                        .unwrap();
                    (output, evidence)
                })
            })
            .collect::<Vec<_>>();
        tasks
            .into_iter()
            .map(|task| task.join().unwrap())
            .collect::<Vec<_>>()
    });
    let result = SegmentInput::new(&result_init, &stop).unwrap();
    let keys = InputKeys::new(vec![
        super::super::ContentKey::from_hex(OUTPUT_ID, OUTPUT_KEY).unwrap(),
    ])
    .unwrap();
    for ((output, evidence), input) in outputs.iter().zip(&resources) {
        assert_eq!(
            *evidence,
            writer
                .source()
                .evidence(input, &InputKeys::default(), &stop)
                .unwrap()
        );
        assert_eq!(*evidence, result.evidence(output, &keys, &stop).unwrap());
    }
    let paths = outputs
        .into_iter()
        .map(|(path, _)| path)
        .collect::<Vec<_>>();
    let full = root.path().join("parallel-full.mp4");
    combined(&result_init, &paths, &full);
    assert_packets(
        oracle(&full, Some((OUTPUT_ID, OUTPUT_KEY))),
        &expected,
        "parallel resource output",
    );
}

#[test]
fn callback_receipt_binds_the_same_guarded_resource_samples() {
    let (_root, init, media) = structural_segment();
    let stop = Stop::new();
    let input = SegmentInput::new(&init, &stop).unwrap();
    let expected = input
        .evidence(&media, &InputKeys::default(), &stop)
        .unwrap();
    let mut counts = [0_u64; 2];
    let (movie, actual) = input
        .scan_with_evidence(
            &media,
            &InputKeys::default(),
            &stop,
            &mut |track, info, _| {
                if info.is_last_chunk {
                    counts[track.index] += 1;
                }
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(actual.evidence, expected);
    assert_eq!(
        actual.fingerprint,
        <[u8; 32]>::from(sha2::Sha256::digest(std::fs::read(&media).unwrap()))
    );
    let (_, repeated) = input
        .scan_with_evidence(&media, &InputKeys::default(), &stop, &mut |_, _, _| Ok(()))
        .unwrap();
    assert_eq!(actual, repeated);
    assert_eq!(counts, [2, 1]);
    assert_eq!(movie.tracks[0].sample_count, counts[0]);
    assert_eq!(movie.tracks[1].sample_count, counts[1]);
}
