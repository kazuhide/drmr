use super::*;
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, process::Command};

const KID_VIDEO: &str = "000102030405060708090a0b0c0d0e0f";
const KEY_VIDEO: &str = "101112131415161718191a1b1c1d1e1f";
const KID_AUDIO: &str = "303132333435363738393a3b3c3d3e3f";
const KEY_AUDIO: &str = "202122232425262728292a2b2c2d2e2f";
fn fixture() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4")
}
fn keys() -> InputKeys {
    InputKeys::new(vec![
        super::super::ContentKey::from_hex(KID_VIDEO, KEY_VIDEO).unwrap(),
        super::super::ContentKey::from_hex(KID_AUDIO, KEY_AUDIO).unwrap(),
    ])
    .unwrap()
}
fn run(command: &mut Command) -> Vec<u8> {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "independent fixture/oracle tool failed"
    );
    output.stdout
}
#[derive(Debug, PartialEq, Eq)]
struct Packet {
    pts: i64,
    dts: i64,
    duration: u32,
    size: u32,
    digest: String,
}
fn oracle(input: &Path, encrypted: bool) -> BTreeMap<usize, Vec<Packet>> {
    let mut command = Command::new("ffprobe");
    command.args(["-v", "error", "-ignore_editlist", "1"]);
    if encrypted {
        command
            .arg("-decryption_keys")
            .arg(format!("{KID_VIDEO}={KEY_VIDEO}:{KID_AUDIO}={KEY_AUDIO}"));
    }
    let output = run(command
        .args([
            "-show_packets",
            "-show_data_hash",
            "sha256",
            "-show_entries",
            "packet=stream_index,pts,dts,duration,size,data_hash:packet_side_data=",
            "-of",
            "json",
        ])
        .arg(input));
    let data: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let mut records = BTreeMap::<usize, Vec<Packet>>::new();
    for packet in data["packets"].as_array().unwrap() {
        let index = usize::try_from(packet["stream_index"].as_u64().unwrap()).unwrap();
        records.entry(index).or_default().push(Packet {
            pts: packet["pts"].as_i64().unwrap(),
            dts: packet["dts"].as_i64().unwrap(),
            duration: u32::try_from(packet["duration"].as_u64().unwrap()).unwrap(),
            size: packet["size"].as_str().unwrap().parse().unwrap(),
            digest: packet["data_hash"]
                .as_str()
                .unwrap()
                .strip_prefix("SHA256:")
                .unwrap()
                .into(),
        });
    }
    records
}
fn native(input: &Path, keys: &InputKeys) -> (Movie, BTreeMap<usize, Vec<Packet>>) {
    let mut records = BTreeMap::<usize, Vec<Packet>>::new();
    let mut digest = None;
    let mut next = 0_u32;
    let movie = scan(input, keys, &Stop::new(), &mut |track, info, bytes| {
        assert!(!bytes.is_empty() && bytes.len() <= CHUNK_BYTES);
        if info.chunk_offset == 0 {
            assert!(digest.is_none());
            digest = Some(Sha256::new());
            next = 0;
        }
        assert_eq!(info.chunk_offset, next);
        next += u32::try_from(bytes.len()).unwrap();
        digest.as_mut().unwrap().update(bytes);
        if info.is_last_chunk {
            assert_eq!(next, info.size);
            let packets = records.entry(track.index).or_default();
            assert_eq!(info.ordinal, packets.len() as u64);
            packets.push(Packet {
                pts: info.pts,
                dts: info.dts,
                duration: info.duration,
                size: info.size,
                digest: hex::encode(digest.take().unwrap().finalize()),
            });
        }
        Ok(())
    })
    .unwrap();
    assert!(digest.is_none());
    (movie, records)
}
fn compare(input: &Path, encrypted: bool) {
    let (movie, actual) = native(
        input,
        &if encrypted {
            keys()
        } else {
            InputKeys::default()
        },
    );
    let expected = oracle(input, encrypted);
    assert_eq!(
        actual.keys().collect::<Vec<_>>(),
        expected.keys().collect::<Vec<_>>()
    );
    for (index, packets) in &actual {
        assert_eq!(packets.len(), expected[index].len());
        assert_eq!(packets.len() as u64, movie.tracks[*index].sample_count);
        for (ordinal, (actual, expected)) in packets.iter().zip(&expected[index]).enumerate() {
            assert_eq!(actual, expected, "track {index}, sample {ordinal}");
        }
    }
}
#[test]
fn original_progressive_samples_and_raw_clocks_match_independent_ffprobe() {
    compare(&fixture(), false);
}

#[test]
fn existing_progressive_cenc_iv8_iv16_and_cbcs_joint_audio_match_independent_decryption() {
    for name in [
        "cenc-iv8.mp4",
        "cenc-iv16.mp4",
        "cbcs-joint.mp4",
        "cbcs-audio.mp4",
    ] {
        compare(&fixture().with_file_name(name), true);
    }
}

#[test]
fn buffered_encrypted_samples_preserve_callback_failure_and_cancellation() {
    let source = fixture().with_file_name("cenc-iv8.mp4");
    let before = std::fs::read(&source).unwrap();
    for cancel in [false, true] {
        let stop = Stop::new();
        let mut calls = 0;
        let result = scan(&source, &keys(), &stop, &mut |_, info, bytes| {
            calls += 1;
            assert_eq!(info.chunk_offset, 0);
            assert!(bytes.len() < usize::try_from(info.size).unwrap());
            if cancel {
                stop.set();
                Ok(())
            } else {
                Err(ConversionError("callback-refused"))
            }
        });
        assert_eq!(calls, 1);
        let expected = if cancel {
            "cancelled"
        } else {
            "callback-refused"
        };
        assert!(matches!(result, Err(error) if error.0 == expected));
    }
    assert_eq!(std::fs::read(source).unwrap(), before);
}

#[test]
fn shaka_cenc_and_cbcs_video_audio_with_distinct_keys_match_independent_decryption() {
    let root = crate::native::private_tempdir().unwrap();
    for scheme in ["cenc", "cbcs"] {
        let video = root.path().join(format!("{scheme}-video.mp4"));
        let audio = root.path().join(format!("{scheme}-audio.mp4"));
        let input = crate::manifest_support::path::fspath(&fixture()).unwrap();
        run(Command::new("packager").arg(format!("input={input},stream=video,output={},drm_label=V",crate::manifest_support::path::fspath(&video).unwrap()))
            .arg(format!("input={input},stream=audio,output={},drm_label=A",crate::manifest_support::path::fspath(&audio).unwrap()))
            .args(["--enable_raw_key_encryption","--keys"]).arg(format!("label=V:key_id={KID_VIDEO}:key={KEY_VIDEO},label=A:key_id={KID_AUDIO}:key={KEY_AUDIO}"))
            .args(["--protection_scheme",scheme,"--clear_lead","0","--segment_duration","2","--mp4_reset_initial_composition_offset_to_zero=false"]));
        for input in [&video, &audio] {
            let stop = Stop::new();
            let mut reader = read::Reader::open(input, &stop, false).unwrap();
            let layout = reader.layout().unwrap();
            let parsed = init::parse(&layout);
            if parsed.is_err() {
                let nodes = crate::mp4::parse_boxes(&layout.movie).unwrap();
                println!(
                    "INIT_BOX_CENSUS {:?}",
                    crate::mp4::walk(&nodes)
                        .iter()
                        .map(|node| crate::mp4::fourcc_text(&node.kind()))
                        .collect::<Vec<_>>()
                );
            }
            let mut parsed =
                parsed.unwrap_or_else(|error| panic!("{scheme} initialization: {error}"));
            let catalogue = catalogue(&mut reader, &layout, &mut parsed);
            if catalogue.is_err() {
                for range in layout.moofs.iter().take(1) {
                    let data = reader.read(range.clone(), MAX_METADATA_BYTES).unwrap();
                    let nodes = crate::mp4::parse_boxes(&data).unwrap();
                    println!(
                        "FRAGMENT_BOX_CENSUS {:?}",
                        crate::mp4::walk(&nodes)
                            .iter()
                            .map(|node| (
                                crate::mp4::fourcc_text(&node.kind()),
                                if matches!(&node.kind(), b"sgpd" | b"sbgp") {
                                    String::from_utf8_lossy(&node.header.payload(&data)[4..8])
                                        .to_string()
                                } else {
                                    String::new()
                                }
                            ))
                            .collect::<Vec<_>>()
                    );
                }
            }
            catalogue.unwrap_or_else(|error| panic!("{scheme} sample catalogue: {error}"));
        }
        compare(&video, true);
        compare(&audio, true);
        for input in [&video, &audio] {
            let mut protected = 0;
            scan(input,&keys(),&Stop::new(),&mut |_,info,_|{
            assert!(matches!(&info.protection,SampleProtection::Encrypted{scheme:actual,..} if *actual==if scheme=="cenc"{Scheme::Cenc}else{Scheme::Cbcs}));
            if info.chunk_offset==0{protected+=1;}Ok(())}).unwrap();
            assert!(protected > 0);
        }
    }
}
#[test]
fn malformed_sample_offsets_refuse_before_payload_visitation() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("offset.mp4");
    std::fs::copy(fixture(), &input).unwrap();
    let stop = Stop::new();
    let mut reader = read::Reader::open(&input, &stop, false).unwrap();
    let layout = reader.layout().unwrap();
    let nodes = crate::mp4::parse_boxes(&layout.movie).unwrap();
    let offset = crate::mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"stco")
        .unwrap();
    let at =
        layout.movie_offset + offset.header.offset as u64 + offset.header.header_size as u64 + 8;
    drop(reader);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(&input)
        .unwrap();
    file.seek(SeekFrom::Start(at)).unwrap();
    std::io::Write::write_all(&mut file, &0_u32.to_be_bytes()).unwrap();
    drop(file);
    let mut callbacks = 0;
    assert!(
        scan(
            &input,
            &InputKeys::default(),
            &Stop::new(),
            &mut |_, _, _| {
                callbacks += 1;
                Ok(())
            }
        )
        .is_err()
    );
    assert_eq!(callbacks, 0);
}
#[test]
fn cancellation_during_payload_never_reports_a_complete_scan() {
    let stop = Stop::new();
    let mut callbacks = 0;
    let result = scan(&fixture(), &InputKeys::default(), &stop, &mut |_, _, _| {
        callbacks += 1;
        stop.set();
        Ok(())
    });
    assert_eq!(result.unwrap_err(), ConversionError("cancelled"));
    assert_eq!(callbacks, 1);
}
#[cfg(unix)]
#[test]
fn same_inode_same_length_rewrites_cannot_reuse_sample_evidence() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("changed.mp4");
    std::fs::copy(fixture(), &input).unwrap();
    let mut changed = false;
    let result = scan(
        &input,
        &InputKeys::default(),
        &Stop::new(),
        &mut |_, _, _| {
            if !changed {
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .open(&input)
                    .unwrap();
                file.seek(SeekFrom::End(-1)).unwrap();
                std::io::Write::write_all(&mut file, &[0xa5]).unwrap();
                file.sync_all().unwrap();
                changed = true;
            }
            Ok(())
        },
    );
    assert_eq!(
        result.unwrap_err(),
        ConversionError("native-source-changed")
    );
}

#[cfg(unix)]
#[test]
fn path_replacement_cannot_substitute_a_new_source_after_payload_visitation() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("original.mp4");
    let replacement = root.path().join("replacement.mp4");
    std::fs::copy(fixture(), &input).unwrap();
    std::fs::copy(fixture(), &replacement).unwrap();
    let mut replaced = false;
    let result = scan(
        &input,
        &InputKeys::default(),
        &Stop::new(),
        &mut |_, _, _| {
            if !replaced {
                std::fs::rename(&replacement, &input).unwrap();
                replaced = true;
            }
            Ok(())
        },
    );
    assert_eq!(
        result.unwrap_err(),
        ConversionError("native-source-changed")
    );
}

fn original_layout() -> read::Layout {
    let stop = Stop::new();
    let mut reader = read::Reader::open(&fixture(), &stop, false).unwrap();
    reader.layout().unwrap()
}

#[test]
fn misplaced_protection_and_recovery_groups_are_not_admitted() {
    let mut layout = original_layout();
    let original = layout.movie.clone();
    let nodes = crate::mp4::parse_boxes(&original).unwrap();
    let moov = &nodes[0];
    let mvhd = moov
        .children
        .iter()
        .find(|node| node.kind() == *b"mvhd")
        .unwrap();
    for kind in [*b"senc", *b"tenc", *b"sbgp", *b"sgpd", *b"uuid"] {
        layout.movie.clone_from(&original);
        layout.movie[mvhd.header.offset + 4..mvhd.header.offset + 8].copy_from_slice(&kind);
        assert!(init::parse(&layout).is_err());
    }
}

#[test]
fn incomplete_table_counts_and_overlapping_chunks_refuse_before_any_payload() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("malformed.mp4");
    let original = std::fs::read(fixture()).unwrap();
    let nodes = crate::mp4::parse_boxes(&original).unwrap();
    for kind in [*b"stts", *b"stsc", *b"stsz", *b"stco"] {
        let node = crate::mp4::walk(&nodes)
            .into_iter()
            .find(|node| node.kind() == kind)
            .unwrap();
        let mut bytes = original.clone();
        let at =
            node.header.offset + node.header.header_size + if kind == *b"stsz" { 8 } else { 4 };
        bytes[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        std::fs::write(&input, bytes).unwrap();
        let mut callbacks = 0;
        assert!(
            scan(
                &input,
                &InputKeys::default(),
                &Stop::new(),
                &mut |_, _, _| {
                    callbacks += 1;
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(callbacks, 0);
    }
    let offsets = crate::mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"stco")
        .unwrap();
    assert!(offsets.header.payload(&original).len() >= 16);
    let mut bytes = original.clone();
    let at = offsets.header.offset + offsets.header.header_size + 8;
    bytes[at + 4..at + 8].copy_from_slice(&original[at..at + 4]);
    std::fs::write(&input, bytes).unwrap();
    let mut callbacks = 0;
    assert!(
        scan(
            &input,
            &InputKeys::default(),
            &Stop::new(),
            &mut |_, _, _| {
                callbacks += 1;
                Ok(())
            }
        )
        .is_err()
    );
    assert_eq!(callbacks, 0);
}

#[test]
fn truncated_top_level_box_and_declared_payload_never_reach_a_visitor() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("truncated.mp4");
    let bytes = std::fs::read(fixture()).unwrap();
    for length in [0, 1, 7, 8, bytes.len() - 1] {
        std::fs::write(&input, &bytes[..length]).unwrap();
        let mut callbacks = 0;
        assert!(
            scan(
                &input,
                &InputKeys::default(),
                &Stop::new(),
                &mut |_, _, _| {
                    callbacks += 1;
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(callbacks, 0);
    }
}

#[test]
fn co64_sample_larger_than_the_avc_canonical_buffer_is_streamed_in_bounded_chunks() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("large-sample.mp4");
    let size = 8 * 1024 * 1024 + 113;
    std::fs::write(
        &input,
        super::test_files::large_sample(&original_layout(), size),
    )
    .unwrap();
    let mut total = 0_u32;
    let mut callbacks = 0;
    let movie = scan(
        &input,
        &InputKeys::default(),
        &Stop::new(),
        &mut |_, info, bytes| {
            assert_eq!(info.chunk_offset, total);
            assert_eq!(info.size, size);
            assert!(bytes.len() <= CHUNK_BYTES && bytes.iter().all(|byte| *byte == 0x5a));
            callbacks += 1;
            total += u32::try_from(bytes.len()).unwrap();
            assert_eq!(info.is_last_chunk, total == size);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(movie.tracks[0].sample_count, 1);
    assert_eq!(total, size);
    assert!(callbacks > 128);
}

#[test]
fn repeated_and_interleaved_fragment_tracks_have_complete_per_track_sample_order() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("interleaved.mp4");
    std::fs::write(
        &input,
        super::test_files::interleaved_fragments(&original_layout(), false),
    )
    .unwrap();
    let mut records = BTreeMap::<usize, Vec<Vec<u8>>>::new();
    let movie = scan(
        &input,
        &InputKeys::default(),
        &Stop::new(),
        &mut |track, info, bytes| {
            let packets = records.entry(track.index).or_default();
            assert_eq!(info.ordinal, packets.len() as u64);
            assert_eq!(info.dts, i64::try_from(info.ordinal).unwrap() * 100);
            assert_eq!(info.pts, info.dts);
            packets.push(bytes.to_vec());
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(movie.tracks[0].sample_count, 2);
    assert_eq!(movie.tracks[1].sample_count, 1);
    assert_eq!(records[&0], [b"v01".to_vec(), b"v02".to_vec()]);
    assert_eq!(records[&1], [b"a01".to_vec()]);
}

#[test]
fn fragment_header_duplicates_clock_gaps_and_sample_counts_refuse_before_payload() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("bad-fragments.mp4");
    let layout = original_layout();
    let original = super::test_files::interleaved_fragments(&layout, false);
    let nodes = crate::mp4::parse_boxes(&original).unwrap();
    let trun = crate::mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"trun")
        .unwrap();
    let tfdt = crate::mp4::walk(&nodes)
        .into_iter()
        .filter(|node| node.kind() == *b"tfdt")
        .nth(2)
        .unwrap();
    let mut count = original.clone();
    let at = trun.header.offset + trun.header.header_size + 4;
    count[at..at + 4].copy_from_slice(&2_u32.to_be_bytes());
    let mut gap = original.clone();
    let at = tfdt.header.offset + tfdt.header.header_size + 4;
    gap[at..at + 4].copy_from_slice(&101_u32.to_be_bytes());
    for bytes in [
        super::test_files::interleaved_fragments(&layout, true),
        count,
        gap,
    ] {
        std::fs::write(&input, bytes).unwrap();
        let mut callbacks = 0;
        assert!(
            scan(
                &input,
                &InputKeys::default(),
                &Stop::new(),
                &mut |_, _, _| {
                    callbacks += 1;
                    Ok(())
                }
            )
            .is_err()
        );
        assert_eq!(callbacks, 0);
    }
}

#[test]
fn missing_key_for_a_later_track_refuses_before_any_plaintext_is_visited() {
    let input = fixture().with_file_name("cenc-iv8.mp4");
    let movie = inspect(&input, &Stop::new()).unwrap();
    assert!(movie.tracks.iter().any(|track| track.protection.key_id
        == Some(hex::decode(KID_AUDIO).unwrap().try_into().unwrap())));
    let keys = InputKeys::new(vec![
        super::super::ContentKey::from_hex(KID_VIDEO, KEY_VIDEO).unwrap(),
    ])
    .unwrap();
    let mut callbacks = 0;
    let result = scan(&input, &keys, &Stop::new(), &mut |_, _, _| {
        callbacks += 1;
        Ok(())
    });
    assert_eq!(result.unwrap_err(), ConversionError("input-key-required"));
    assert_eq!(callbacks, 0);
}

#[test]
fn signed_track_layer_is_retained_separately_from_alternate_group_and_volume() {
    let mut layout = original_layout();
    let nodes = crate::mp4::parse_boxes(&layout.movie).unwrap();
    let tkhd = crate::mp4::walk(&nodes)
        .into_iter()
        .find(|node| node.kind() == *b"tkhd")
        .unwrap();
    assert_eq!(tkhd.header.payload(&layout.movie)[0], 0);
    let at = tkhd.header.offset + tkhd.header.header_size + 32;
    layout.movie[at..at + 2].copy_from_slice(&(-2_i16).to_be_bytes());
    let init = init::parse(&layout).unwrap();
    assert_eq!(init.movie.tracks[0].sample_entry_metadata.layer, -2);
    assert_eq!(
        init.movie.tracks[0].sample_entry_metadata.alternate_group,
        0
    );
    assert_eq!(init.movie.tracks[0].sample_entry_metadata.volume, 0);
}

#[test]
fn unsupported_track_controls_are_refused_before_reader_payload_visitation() {
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("controls.mp4");
    let original = std::fs::read(fixture()).unwrap();
    for changed in [
        super::test_files::with_track_aperture(&original),
        super::test_files::with_extended_language(&original),
    ] {
        std::fs::write(&input, changed).unwrap();
        let mut callbacks = 0;
        let result = scan(
            &input,
            &InputKeys::default(),
            &Stop::new(),
            &mut |_, _, _| {
                callbacks += 1;
                Ok(())
            },
        );
        let Err(error) = result else {
            panic!("unsupported controls reached a complete reader scan");
        };
        assert_eq!(error, UNSUPPORTED);
        assert_eq!(callbacks, 0);
    }
}

#[test]
fn unsupported_track_controls_cannot_pass_full_native_source_admission() {
    use crate::{
        events::Collector,
        toolrun::{Secrets, ToolRunner},
    };
    use std::sync::Arc;
    let events = Arc::new(Collector::default());
    let runner = Arc::new(ToolRunner::new(Arc::new(Secrets::new()), events.clone()));
    let converter =
        super::super::Converter::new(runner, "must-not-run-packager", "must-not-run-ffmpeg");
    let root = crate::native::private_tempdir().unwrap();
    let input = root.path().join("controls.mp4");
    let original = std::fs::read(fixture()).unwrap();
    let empty = InputKeys::default();
    let hls = super::super::HlsInputKeys::default();
    let original_input = fixture();
    let admitted = converter
        .validate_source(
            super::super::VerificationSource {
                input: &original_input,
                input_keys: &empty,
                hls_keys: &hls,
            },
            super::super::SourceValidationPurpose::Conversion,
        )
        .unwrap();
    assert!(admitted.verification.passed);
    for changed in [
        super::test_files::with_track_aperture(&original),
        super::test_files::with_extended_language(&original),
    ] {
        std::fs::write(&input, changed).unwrap();
        let result = converter.validate_source(
            super::super::VerificationSource {
                input: &input,
                input_keys: &empty,
                hls_keys: &hls,
            },
            super::super::SourceValidationPurpose::Conversion,
        );
        let Err(error) = result else {
            panic!("unsupported controls passed full native admission");
        };
        assert_eq!(error, UNSUPPORTED);
    }
    assert!(
        events
            .records()
            .iter()
            .all(|record| record.get("stage").and_then(serde_json::Value::as_str) != Some("tool"))
    );
}
