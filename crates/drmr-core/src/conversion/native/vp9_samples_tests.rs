//! VP9 retains reference state and raw superframe bytes across bounded callbacks.
use super::*;
use crate::conversion::{InputKeys, native_media};

const SEEDS: [&str; 6] = [
    "vp9-p0-8.mp4",
    "vp9-p1-8.mp4",
    "vp9-p2-10.mp4",
    "vp9-p2-12.mp4",
    "vp9-p3-10.mp4",
    "vp9-p3-12.mp4",
];

fn packets(name: &str) -> (native_media::TrackDescriptor, Vec<(SampleInfo, Vec<u8>)>) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name);
    let mut selected = None;
    let mut packets: Vec<(SampleInfo, Vec<u8>)> = Vec::new();
    native_media::scan(
        &path,
        &InputKeys::default(),
        &crate::concurrency::Stop::new(),
        &mut |track, info, chunk| {
            if selected.is_none() {
                selected = Some(track.clone());
            }
            if info.chunk_offset == 0 {
                packets.push((info.clone(), Vec::new()));
            }
            packets.last_mut().unwrap().1.extend_from_slice(chunk);
            Ok(())
        },
    )
    .unwrap();
    for (info, bytes) in &mut packets {
        assert_eq!(u32::try_from(bytes.len()).unwrap(), info.size);
        info.chunk_offset = 0;
        info.is_last_chunk = true;
    }
    (selected.unwrap(), packets)
}

fn accumulator(track: &native_media::TrackDescriptor, expected: u64) -> Samples {
    Samples::new(track.timescale, expected, None)
        .unwrap()
        .with_vp9(
            vp9::Configuration::from_vpcc(&track.decoder_config).unwrap(),
            u32::from(track.sample_entry_metadata.width.unwrap()),
            u32::from(track.sample_entry_metadata.height.unwrap()),
        )
        .unwrap()
}

fn collect(
    track: &native_media::TrackDescriptor,
    packets: &[(SampleInfo, Vec<u8>)],
    width: usize,
) -> SampleEvidence {
    let mut samples = accumulator(track, u64::try_from(packets.len()).unwrap());
    for (info, bytes) in packets {
        let mut offset = 0;
        for part in bytes.chunks(width) {
            let mut next = info.clone();
            next.chunk_offset = u32::try_from(offset).unwrap();
            offset += part.len();
            next.is_last_chunk = offset == bytes.len();
            samples.push(&next, part).unwrap();
        }
    }
    samples.finish().unwrap()
}

#[test]
fn vp9_chunks_validate_every_profile_and_hidden_frame_in_one_reference_state() {
    for name in SEEDS {
        let (track, packets) = packets(name);
        assert_eq!(packets.len(), 240);
        let complete = collect(&track, &packets, native_media::CHUNK_BYTES);
        let chunked = collect(&track, &packets, 13);
        assert_eq!(complete.vp9_geometry, chunked.vp9_geometry);
        assert_eq!(
            complete.vp9_geometry,
            Some([
                u32::from(track.sample_entry_metadata.width.unwrap()),
                u32::from(track.sample_entry_metadata.height.unwrap()),
                u32::from(track.sample_entry_metadata.width.unwrap()),
                u32::from(track.sample_entry_metadata.height.unwrap()),
            ])
        );
        assert!(complete.canonical_payload.is_none());
        assert_eq!(complete.raw_payload, chunked.raw_payload, "{name}");
        assert!(complete.clocks_equal(&chunked), "{name}");
        assert_eq!(complete.sync_samples, chunked.sync_samples, "{name}");
        for width in [1, 127] {
            let single = collect(&track, &packets[..1], width);
            let borrowed = collect(&track, &packets[..1], native_media::CHUNK_BYTES);
            assert_eq!(single.raw_payload, borrowed.raw_payload, "{name}");
            assert!(single.clocks_equal(&borrowed), "{name}");
        }
    }
}

#[test]
fn vp9_raw_identity_includes_a_valid_one_frame_superframe_index() {
    let (_, packets) = packets(SEEDS[0]);
    let (frame, _) = vp9::test_key_frame();
    let indexed = vp9::test_superframe(&[&frame], 2);
    let collect = |bytes: &[u8]| {
        let mut info = packets[0].0.clone();
        info.size = u32::try_from(bytes.len()).unwrap();
        info.range = 0..u64::from(info.size);
        let mut samples = Samples::new(24, 1, None)
            .unwrap()
            .with_vp9(
                vp9::Configuration::from_vpcc(&vp9::test_configuration()).unwrap(),
                512,
                512,
            )
            .unwrap();
        samples.push(&info, bytes).unwrap();
        samples.finish().unwrap()
    };
    let raw = collect(&frame);
    let indexed = collect(&indexed);
    assert_ne!(raw.raw_payload, indexed.raw_payload);
    assert!(raw.clocks_equal(&indexed));
    assert_eq!(raw.sync_samples, indexed.sync_samples);
    assert!(raw.canonical_payload.is_none() && indexed.canonical_payload.is_none());
    assert_eq!(raw.vp9_geometry, Some([128, 128, 128, 128]));
    assert_eq!(raw.vp9_geometry, indexed.vp9_geometry);
}

#[test]
fn vp9_oversized_samples_never_fall_back_to_unchecked_payloads() {
    let (track, packets) = packets(SEEDS[0]);
    let mut info = packets[0].0.clone();
    info.size = u32::try_from(MAX_CANONICAL_BYTES + 1).unwrap();
    info.range.end = info.range.start + u64::from(info.size);
    info.is_last_chunk = false;
    let mut samples = accumulator(&track, 1);
    assert_eq!(
        samples.push(&info, &[0]).unwrap_err().0,
        "native-vp9-sample-size-limit"
    );
    assert!(samples.finish().is_err());
}

#[test]
fn vp9_sync_declarations_must_match_key_and_dependent_samples() {
    for name in SEEDS {
        let (track, packets) = packets(name);
        for ordinal in [0, 1] {
            let mut samples = accumulator(&track, 2);
            if ordinal == 1 {
                samples.push(&packets[0].0, &packets[0].1).unwrap();
            }
            let (mut info, bytes) = packets[ordinal].clone();
            info.is_sync = !info.is_sync;
            assert_eq!(
                samples.push(&info, &bytes).unwrap_err().0,
                "native-vp9-sync-sample-mismatch",
                "{name} sample {ordinal}"
            );
            assert!(samples.finish().is_err());
        }
    }
}

#[test]
fn vp9_late_syntax_failure_stays_failed_when_valid_bytes_are_replayed() {
    let (track, packets) = packets(SEEDS[0]);
    let mut samples = accumulator(&track, 2);
    samples.push(&packets[0].0, &packets[0].1).unwrap();
    let (info, bytes) = &packets[1];
    let mut invalid = bytes.clone();
    invalid[0] &= 0x3f;
    assert!(samples.push(info, &invalid).is_err());
    assert!(samples.push(info, bytes).is_err());
    assert!(samples.finish().is_err());
}

#[test]
fn vp9_progressive_frames_never_ignore_a_conflicting_container_field_order() {
    let (mut track, _) = packets(SEEDS[0]);
    track
        .sample_entry_metadata
        .boxes
        .retain(|node| node.kind != *b"fiel");
    for payload in [vec![2, 1], vec![1, 1]] {
        let mut invalid = track.clone();
        invalid
            .sample_entry_metadata
            .boxes
            .push(native_media::SemanticBox {
                kind: *b"fiel",
                payload,
            });
        assert_eq!(
            super::super::media::accumulator_with_clock(&invalid, None)
                .err()
                .unwrap()
                .0,
            "native-vp9-field-order-not-supported"
        );
    }
}

fn golden_protection(range: Option<&std::ops::Range<u32>>) -> native_media::SampleProtection {
    // The independently measured p0 first sample is 19121 bytes with an 18-byte
    // header: canonical protected bytes are 33..19121; Shaka uses 18..19106.
    let mut digest = crate::sha256::Sha256::new();
    digest.update(b"vp9-protected-byte-ranges-v1");
    digest.update(19121_u32.to_be_bytes());
    let encrypted = range.map_or(0, |range| range.end - range.start);
    if let Some(range) = range {
        digest.update(range.start.to_be_bytes());
        digest.update(range.end.to_be_bytes());
    }
    digest.update(u32::from(range.is_some()).to_be_bytes());
    native_media::SampleProtection::Encrypted {
        scheme: native_media::Scheme::Cenc,
        key_id: [1; 16],
        crypt_byte_block: 0,
        skip_byte_block: 0,
        encrypted_bytes: encrypted,
        vp9_ranges: Some(digest.finalize()),
    }
}

#[test]
fn vp9_explicit_clear_samples_are_neutral_to_canonical_or_modeled_tail_track_facts() {
    let (track, packets) = packets(SEEDS[0]);
    let (first, bytes) = &packets[0];
    assert_eq!(first.size, 19121);
    for (range, fact) in [
        (33..19121, native_media::Vp9Protection::CanonicalCenc),
        (18..19106, native_media::Vp9Protection::ShakaTailCenc),
    ] {
        for clear_first in [false, true] {
            let mut samples = accumulator(&track, 2);
            for ordinal in 0..2 {
                let mut info = first.clone();
                info.ordinal = ordinal;
                info.dts += i64::try_from(ordinal).unwrap() * i64::from(info.duration);
                info.pts = info.dts;
                info.protection = if (ordinal == 0) == clear_first {
                    native_media::SampleProtection::Clear
                } else {
                    golden_protection(Some(&range))
                };
                samples.push(&info, bytes).unwrap();
            }
            assert_eq!(samples.finish().unwrap().vp9_protection, Some(fact));
        }
    }
}

#[test]
fn vp9_invalid_encrypted_maps_never_become_clear_when_mixed_with_clear_samples() {
    let (track, packets) = packets(SEEDS[0]);
    let (first, bytes) = &packets[0];
    for range in [None, Some(0..19121)] {
        for clear_first in [false, true] {
            let mut samples = accumulator(&track, 2);
            if clear_first {
                samples.push(first, bytes).unwrap();
            }
            let mut invalid = first.clone();
            invalid.ordinal = u64::from(clear_first);
            invalid.dts += i64::from(clear_first) * i64::from(invalid.duration);
            invalid.pts = invalid.dts;
            invalid.protection = golden_protection(range.as_ref());
            assert_eq!(
                samples.push(&invalid, bytes).unwrap_err(),
                native_media::VP9_LAYOUT
            );
            invalid.protection = native_media::SampleProtection::Clear;
            assert!(samples.push(&invalid, bytes).is_err());
            assert!(samples.finish().is_err());
        }
    }
}
