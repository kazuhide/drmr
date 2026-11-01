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

fn hrd_stream(temporal: u8, cpb_count: u8, configuration_sei: bool) -> Result<(), ConversionError> {
    let (config, data) =
        hevc::Configuration::test_hrd_stream(temporal, cpb_count, configuration_sei);
    complete_hrd_stream(&config, &data)
}

fn complete_hrd_stream(config: &[u8], data: &[Vec<u8>]) -> Result<(), ConversionError> {
    let (config, width) = hevc::Configuration::from_hvcc(config)?;
    let mut samples = Samples::new(24, u64::try_from(data.len()).unwrap(), None)?.with_hevc(
        config.clone(),
        width,
        *b"hvc1",
    )?;
    for (ordinal, bytes) in data.iter().enumerate() {
        let info = SampleInfo {
            ordinal: u64::try_from(ordinal).unwrap(),
            dts: i64::try_from(ordinal).unwrap(),
            pts: i64::try_from(ordinal).unwrap(),
            duration: 1,
            size: u32::try_from(bytes.len()).unwrap(),
            range: 0..u64::try_from(bytes.len()).unwrap(),
            flags: 0,
            is_sync: config
                .length_prefixed_access_unit(bytes, width, *b"hvc1")?
                .is_sync,
            roll_distance: None,
            preroll_distance: None,
            protection: native_media::SampleProtection::Clear,
            chunk_offset: 0,
            is_last_chunk: true,
        };
        samples.push(&info, bytes)?;
    }
    samples.finish()?;
    Ok(())
}

#[test]
fn hevc_r2_buffering_period_uses_the_complete_stream_operation_point() {
    for configuration_sei in [false, true] {
        let matrix = [
            hrd_stream(0, 1, configuration_sei).is_ok(),
            hrd_stream(1, 2, configuration_sei).is_ok(),
            hrd_stream(1, 1, configuration_sei).is_ok(),
            hrd_stream(0, 2, configuration_sei).is_ok(),
        ];
        assert_eq!(
            matrix,
            [true, true, false, false],
            "configuration SEI {configuration_sei}"
        );
        for (temporal, cpb_count) in [(1, 1), (0, 2)] {
            assert_eq!(
                hrd_stream(temporal, cpb_count, configuration_sei)
                    .unwrap_err()
                    .0,
                "native-hevc-hrd-operation-point-mismatch"
            );
        }
    }
}

#[test]
fn hevc_hrd_candidates_intersect_across_complete_samples() {
    let (config, mut data) = hevc::Configuration::test_hrd_stream(1, 2, false);
    let (_, low) = hevc::Configuration::test_hrd_stream(0, 1, false);
    let length = usize::try_from(u32::from_be_bytes(low[0][..4].try_into().unwrap())).unwrap();
    let mut middle = low[0][..4 + length].to_vec();
    let mut trail = data[1].clone();
    trail[5] = 1; // The middle AU is temporal 0; the final AU still establishes actual max 1.
    middle.extend(trail);
    data.insert(1, middle);
    assert_eq!(
        complete_hrd_stream(&config, &data).unwrap_err().0,
        "native-hevc-hrd-operation-point-mismatch"
    );
}

#[test]
fn hevc_terminal_boundaries_allow_an_irap_after_end_of_bitstream() {
    let (track, info, bytes) = first_sample("hevc-main.mp4");
    let mut terminal = bytes.clone();
    terminal.extend(2_u32.to_be_bytes());
    terminal.extend([37 << 1, 1]);
    let mut end_info = info.clone();
    end_info.size = u32::try_from(terminal.len()).unwrap();
    end_info.range.end = end_info.range.start + u64::from(end_info.size);
    let mut complete = accumulator(&track);
    complete.push(&end_info, &terminal).unwrap();
    assert!(complete.finish().is_ok());

    let (config, width) = hevc::Configuration::from_hvcc(&track.decoder_config).unwrap();
    let mut additional = Samples::new(track.timescale, 2, None)
        .unwrap()
        .with_hevc(config, width, track.codec)
        .unwrap();
    additional.push(&end_info, &terminal).unwrap();
    let mut next = info;
    next.ordinal = 1;
    next.dts += i64::from(next.duration);
    next.pts += i64::from(next.duration);
    additional.push(&next, &bytes).unwrap();
    assert!(additional.finish().is_ok());
}

fn joined_hrd_streams(
    terminal: u8,
    first: (u8, u8),
    second: (u8, u8),
    configuration_sei: bool,
) -> Result<(), ConversionError> {
    let (config, mut data) =
        hevc::Configuration::test_hrd_stream(first.0, first.1, configuration_sei);
    data.last_mut().unwrap().extend(2_u32.to_be_bytes());
    data.last_mut().unwrap().extend([terminal << 1, 1]);
    let (_, next) = hevc::Configuration::test_hrd_stream(second.0, second.1, false);
    data.extend(next);
    complete_hrd_stream(&config, &data)
}

#[test]
fn hevc_terminal_boundaries_keep_or_reset_the_bitstream_operation_point() {
    for configuration_sei in [false, true] {
        for (terminal, first, second, valid) in [
            (37, (0, 1), (1, 2), true),
            (37, (1, 2), (0, 1), true),
            (36, (0, 2), (1, 2), true),
            (36, (1, 2), (0, 2), true),
            (36, (1, 2), (0, 1), false),
            (36, (0, 1), (1, 2), false),
        ] {
            let result = joined_hrd_streams(terminal, first, second, configuration_sei);
            assert_eq!(
                result.is_ok(),
                valid,
                "terminal {terminal}, {first:?} -> {second:?}, config SEI {configuration_sei}"
            );
            if !valid {
                assert_eq!(
                    result.unwrap_err().0,
                    "native-hevc-hrd-operation-point-mismatch"
                );
            }
        }
    }
}

#[test]
fn hevc_terminal_boundaries_require_the_next_access_unit_to_be_irap() {
    for terminal in [36, 37] {
        let (config, mut data) = hevc::Configuration::test_hrd_stream(0, 1, false);
        data[0].extend(2_u32.to_be_bytes());
        data[0].extend([terminal << 1, 1]);
        let (_, mut next) = hevc::Configuration::test_hrd_stream(1, 2, false);
        data.push(next.pop().unwrap());
        assert_eq!(
            complete_hrd_stream(&config, &data).unwrap_err().0,
            "native-hevc-terminal-followed-by-non-irap"
        );
    }
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
