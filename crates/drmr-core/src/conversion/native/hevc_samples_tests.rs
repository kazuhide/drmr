//! HEVC validation must be independent of the reader's callback chunk boundaries.
use super::*;
use crate::conversion::{InputKeys, native_media};

fn first_sample(name: &str) -> (native_media::TrackDescriptor, SampleInfo, Vec<u8>) {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../drmr/tests/fixtures/bbb")
        .join(name);
    let mut selected = None;
    let mut bytes = Vec::new();
    native_media::scan(
        &path,
        &InputKeys::default(),
        &crate::concurrency::Stop::new(),
        &mut |track, info, chunk| {
            if track.kind == crate::inspection::TrackKind::Video && info.ordinal == 0 {
                if selected.is_none() {
                    selected = Some((track.clone(), info.clone()));
                }
                bytes.extend_from_slice(chunk);
            }
            Ok(())
        },
    )
    .unwrap();
    let (track, mut info) = selected.unwrap();
    assert_eq!(bytes.len(), info.size as usize);
    info.chunk_offset = 0;
    info.is_last_chunk = true;
    (track, info, bytes)
}

fn accumulator(track: &native_media::TrackDescriptor) -> Samples {
    let (config, width) = hevc::Configuration::from_hvcc(&track.decoder_config).unwrap();
    Samples::new(track.timescale, 1, None)
        .unwrap()
        .with_hevc(config, width, track.codec)
        .unwrap()
}

#[test]
fn hevc_chunks_retain_the_complete_access_unit_and_exact_clock_evidence() {
    for name in ["hevc-main.mp4", "hevc-main10.mp4"] {
        let (track, info, bytes) = first_sample(name);
        let mut complete = accumulator(&track);
        complete.push(&info, &bytes).unwrap();
        let complete = complete.finish().unwrap();
        for width in [1, 13, 127] {
            let mut chunked = accumulator(&track);
            let mut offset = 0;
            for part in bytes.chunks(width) {
                let mut next = info.clone();
                next.chunk_offset = u32::try_from(offset).unwrap();
                offset += part.len();
                next.is_last_chunk = offset == bytes.len();
                chunked.push(&next, part).unwrap();
            }
            let chunked = chunked.finish().unwrap();
            assert_eq!(complete.raw_payload, chunked.raw_payload, "{name}");
            assert!(complete.canonical_payload.is_some());
            assert_eq!(
                complete.canonical_payload, chunked.canonical_payload,
                "{name}"
            );
            assert!(complete.clocks_equal(&chunked));
            assert_eq!(complete.sync_samples, chunked.sync_samples);
        }
    }
}

#[test]
fn hevc_oversized_access_units_never_fall_back_to_unchecked_payloads() {
    let (track, mut info, _) = first_sample("hevc-main.mp4");
    info.size = u32::try_from(MAX_CANONICAL_BYTES + 1).unwrap();
    info.range.end = info.range.start + u64::from(info.size);
    info.is_last_chunk = false;
    let mut samples = accumulator(&track);
    assert_eq!(
        samples.push(&info, &[0]).unwrap_err().0,
        "native-hevc-access-unit-size-limit"
    );
    assert!(samples.finish().is_err());
}

#[test]
fn hevc_sync_declarations_must_match_the_actual_picture() {
    for name in ["hevc-main.mp4", "hevc-main10.mp4"] {
        let (track, mut info, bytes) = first_sample(name);
        assert!(info.is_sync);
        info.is_sync = false;
        let mut samples = accumulator(&track);
        assert_eq!(
            samples.push(&info, &bytes).unwrap_err().0,
            "native-hevc-sync-sample-mismatch"
        );
        assert!(samples.finish().is_err());
    }
}

#[test]
fn hevc_syntax_failure_stays_failed_when_valid_bytes_are_replayed() {
    let (track, info, bytes) = first_sample("hevc-main.mp4");
    let mut invalid = bytes.clone();
    invalid[..4].fill(0);
    let mut samples = accumulator(&track);
    assert!(samples.push(&info, &invalid).is_err());
    assert!(samples.push(&info, &bytes).is_err());
    assert!(samples.finish().is_err());
}

#[test]
fn hevc_progressive_bitstream_never_ignores_an_interlaced_container_declaration() {
    let (mut track, _, _) = first_sample("hevc-main.mp4");
    track
        .sample_entry_metadata
        .boxes
        .retain(|node| node.kind != *b"fiel");
    track
        .sample_entry_metadata
        .boxes
        .push(native_media::SemanticBox {
            kind: *b"fiel",
            payload: vec![2, 1],
        });
    assert_eq!(
        super::super::media::accumulator_with_clock(&track, None)
            .err()
            .unwrap()
            .0,
        "native-hevc-field-order-not-supported"
    );
}
