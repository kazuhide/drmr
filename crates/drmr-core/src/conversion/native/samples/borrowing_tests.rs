use super::super::super::{
    InputKeys,
    native_media::{self, SampleProtection, Scheme},
};
use super::*;
use crate::{concurrency::Stop, inspection::TrackKind};
use std::path::Path;

fn audio() -> (CanonicalConfig, u8) {
    (
        CanonicalConfig::Audio {
            channels: 2,
            sample_rate: 44_100,
        },
        4,
    )
}

fn info(size: usize) -> SampleInfo {
    let size = u32::try_from(size).unwrap();
    SampleInfo {
        ordinal: 0,
        dts: 0,
        pts: 0,
        duration: 1024,
        size,
        range: 0..u64::from(size),
        flags: 0,
        is_sync: true,
        roll_distance: None,
        preroll_distance: None,
        protection: SampleProtection::Clear,
        chunk_offset: 0,
        is_last_chunk: true,
    }
}

fn collect(
    info: &SampleInfo,
    bytes: &[u8],
    configuration: Option<(CanonicalConfig, u8)>,
    chunk: usize,
) -> Result<SampleEvidence, ConversionError> {
    let mut samples = Samples::new(44_100, 1, configuration)?;
    for (ordinal, bytes) in bytes.chunks(chunk).enumerate() {
        let mut info = info.clone();
        info.chunk_offset = u32::try_from(ordinal * chunk).unwrap();
        info.is_last_chunk = usize::try_from(info.chunk_offset).unwrap() + bytes.len()
            == usize::try_from(info.size).unwrap();
        samples.push(&info, bytes)?;
    }
    samples.finish()
}

fn equal(first: &SampleEvidence, second: &SampleEvidence) {
    macro_rules! fields {
        ($($name:ident),+ $(,)?) => {$(assert_eq!(first.$name, second.$name, stringify!($name));)+};
    }
    fields!(
        count,
        raw_payload,
        canonical_payload,
        clocks,
        presentation_clocks,
        presentation_offset,
        transport_clocks,
        durations,
        coded_duration,
        sync_samples,
        recovery,
        transport_recovery_compatible,
        initial_composition_offset,
        first_pts,
        end_pts,
        presented_first_pts,
        presented_end_pts,
        continuous_decode,
        protected_samples,
    );
}

#[test]
fn audio_complete_and_fragmented_samples_keep_every_digest_and_exact_clock() {
    let bytes = (0..37).collect::<Vec<u8>>();
    let sample = info(bytes.len());
    let complete = collect(&sample, &bytes, Some(audio()), bytes.len()).unwrap();
    assert!(complete.canonical_payload.is_some());
    for chunk in 1..bytes.len() {
        equal(
            &complete,
            &collect(&sample, &bytes, Some(audio()), chunk).unwrap(),
        );
    }
    let raw = collect(&sample, &bytes, None, bytes.len()).unwrap();
    assert!(raw.canonical_payload.is_none());
    for chunk in [1, 2, 7, 13] {
        equal(&raw, &collect(&sample, &bytes, None, chunk).unwrap());
    }
}

#[test]
fn chunk_limit_is_unchanged_and_larger_samples_keep_the_fragmented_path() {
    let bytes = vec![0xa5; native_media::CHUNK_BYTES + 17];
    let limit = native_media::CHUNK_BYTES;
    let sample = info(limit);
    let complete = collect(&sample, &bytes[..limit], Some(audio()), limit).unwrap();
    for chunk in [1, 257, limit - 1] {
        equal(
            &complete,
            &collect(&sample, &bytes[..limit], Some(audio()), chunk).unwrap(),
        );
    }
    let sample = info(bytes.len());
    assert!(collect(&sample, &bytes, Some(audio()), bytes.len()).is_err());
    let fragmented = collect(&sample, &bytes, Some(audio()), limit).unwrap();
    equal(
        &fragmented,
        &collect(&sample, &bytes, Some(audio()), 4093).unwrap(),
    );
}

fn rejected(mut sample: SampleInfo, bytes: &[u8]) {
    let mut samples = Samples::new(44_100, 1, Some(audio())).unwrap();
    assert!(samples.push(&sample, bytes).is_err());
    sample = info(1);
    assert_eq!(samples.push(&sample, &[0]), Err(BAD));
    assert!(samples.finish().is_err());
}

#[test]
fn complete_callbacks_do_not_bypass_header_extent_or_last_chunk_validation() {
    let valid = info(4);
    for case in 0..8 {
        let mut sample = valid.clone();
        match case {
            0 => sample.ordinal = 1,
            1 => sample.chunk_offset = 1,
            2 => sample.size = 0,
            3 => sample.duration = 0,
            4 => sample.range.end = 3,
            5 => sample.range.start = 1,
            6 => sample.is_last_chunk = false,
            7 => sample.size = 3,
            _ => unreachable!(),
        }
        rejected(sample, &[1, 2, 3, 4]);
    }
    rejected(valid.clone(), &[]);
    rejected(valid, &[1, 2, 3]);
}

#[test]
fn continuation_identity_failures_remain_sticky_after_partial_canonical_storage() {
    for case in 0..14 {
        let mut samples = Samples::new(44_100, 1, Some(audio())).unwrap();
        let mut sample = info(4);
        sample.is_last_chunk = false;
        samples.push(&sample, &[1, 2]).unwrap();
        assert_eq!(
            samples.pending.as_ref().unwrap().canonical_bytes.as_slice(),
            &[1, 2]
        );
        sample.chunk_offset = 2;
        sample.is_last_chunk = true;
        match case {
            0 => sample.ordinal = 1,
            1 => sample.dts = 1,
            2 => sample.pts = 1,
            3 => sample.duration = 1,
            4 => sample.size = 5,
            5 => sample.range = 1..5,
            6 => sample.flags = 1,
            7 => sample.is_sync = false,
            8 => sample.roll_distance = Some(1),
            9 => sample.preroll_distance = Some(-1),
            10 => {
                sample.protection = SampleProtection::Encrypted {
                    scheme: Scheme::Cenc,
                    key_id: [7; 16],
                    crypt_byte_block: 0,
                    skip_byte_block: 0,
                    encrypted_bytes: 4,
                    vp9_ranges: None,
                }
            }
            11 => sample.chunk_offset = 3,
            12 => sample.is_last_chunk = false,
            13 => {}
            _ => unreachable!(),
        }
        let bytes: &[u8] = if case == 13 { &[3, 4, 5] } else { &[3, 4] };
        assert!(samples.push(&sample, bytes).is_err(), "case {case}");
        assert_eq!(samples.push(&info(1), &[0]), Err(BAD));
        assert!(samples.finish().is_err());
    }
}

fn avc() -> (SampleInfo, Vec<u8>, (CanonicalConfig, u8)) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let mut first = None;
    native_media::scan(
        &path,
        &InputKeys::default(),
        &Stop::new(),
        &mut |track, info, bytes| {
            if first.is_none()
                && track.kind == TrackKind::Video
                && info.ordinal == 0
                && info.chunk_offset == 0
                && info.is_last_chunk
            {
                let (_, width) =
                    super::super::super::ts::avc::Configuration::from_avcc(&track.decoder_config)?;
                first = Some((
                    info.clone(),
                    bytes.to_vec(),
                    (CanonicalConfig::video(&track.decoder_config, false)?, width),
                ));
            }
            Ok(())
        },
    )
    .unwrap();
    first.expect("fixture contains a bounded complete AVC sample")
}

#[test]
fn avc_complete_and_fragmented_units_keep_sync_canonical_payload_and_refusals() {
    let (sample, bytes, config) = avc();
    let complete = collect(&sample, &bytes, Some(config.clone()), bytes.len()).unwrap();
    assert!(complete.canonical_payload.is_some());
    for chunk in [1, 2, 3, 31, 257] {
        equal(
            &complete,
            &collect(&sample, &bytes, Some(config.clone()), chunk).unwrap(),
        );
    }
    let mut false_sync = sample.clone();
    false_sync.is_sync = !false_sync.is_sync;
    for chunk in [1, bytes.len()] {
        assert_eq!(
            collect(&false_sync, &bytes, Some(config.clone()), chunk).err(),
            Some(ConversionError("native-avc-sync-sample-mismatch"))
        );
    }
    let mut invalid = bytes;
    invalid[0] = 0xff;
    let complete_error = collect(&sample, &invalid, Some(config.clone()), invalid.len()).err();
    assert!(complete_error.is_some());
    assert_eq!(
        collect(&sample, &invalid, Some(config), 3).err(),
        complete_error
    );
}

#[test]
fn oversized_canonical_audio_stays_bounded_and_avc_is_still_refused() {
    let size = MAX_CANONICAL_BYTES + 1;
    let sample = info(size);
    let chunk = vec![0x5a; native_media::CHUNK_BYTES];
    let mut samples = Samples::new(44_100, 1, Some(audio())).unwrap();
    for offset in (0..size).step_by(chunk.len()) {
        let length = (size - offset).min(chunk.len());
        let mut sample = sample.clone();
        sample.chunk_offset = u32::try_from(offset).unwrap();
        sample.is_last_chunk = offset + length == size;
        samples.push(&sample, &chunk[..length]).unwrap();
        assert!(samples.canonical.is_none());
        if let Some(pending) = &samples.pending {
            assert_eq!(pending.canonical_bytes.len(), 0);
        }
    }
    assert!(samples.finish().unwrap().canonical_payload.is_none());
    let (_, _, config) = avc();
    let mut samples = Samples::new(44_100, 1, Some(config)).unwrap();
    let mut sample = sample;
    sample.is_last_chunk = false;
    assert_eq!(
        samples.push(&sample, &[0]),
        Err(ConversionError("native-avc-access-unit-size-limit"))
    );
    assert_eq!(samples.push(&info(1), &[0]), Err(BAD));
    assert!(samples.finish().is_err());
}

#[test]
fn complete_avc_canonical_failures_cannot_be_repaired_by_a_later_valid_callback() {
    let (sample, bytes, config) = avc();
    for invalid_syntax in [false, true] {
        let mut changed = sample.clone();
        let mut payload = bytes.clone();
        if invalid_syntax {
            payload[0] = 0xff;
        } else {
            changed.is_sync = !changed.is_sync;
        }
        let mut samples = Samples::new(44_100, 1, Some(config.clone())).unwrap();
        assert!(samples.push(&changed, &payload).is_err());
        assert_eq!(samples.push(&sample, &bytes), Err(BAD));
        assert!(samples.finish().is_err());
    }
}

fn repeated(
    mut sample: SampleInfo,
    bytes: &[u8],
    config: (CanonicalConfig, u8),
    count: u64,
    chunk: usize,
) -> SampleEvidence {
    let audio_clock = matches!(&config.0, CanonicalConfig::Audio { .. });
    let mut samples = Samples::new(44_100, count, Some(config)).unwrap();
    if audio_clock {
        samples = samples.with_audio_clock(1024, 44_100).unwrap();
    }
    let pts = sample.pts;
    let dts = sample.dts;
    for ordinal in 0..count {
        sample.ordinal = ordinal;
        let offset = i64::try_from(ordinal * u64::from(sample.duration)).unwrap();
        sample.pts = pts + offset;
        sample.dts = dts + offset;
        for (part, bytes) in std::hint::black_box(bytes).chunks(chunk).enumerate() {
            sample.chunk_offset = u32::try_from(part * chunk).unwrap();
            sample.is_last_chunk = usize::try_from(sample.chunk_offset).unwrap() + bytes.len()
                == usize::try_from(sample.size).unwrap();
            samples.push(&sample, bytes).unwrap();
        }
    }
    samples.finish().unwrap()
}

fn fingerprint(evidence: &SampleEvidence) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(evidence.count.to_be_bytes());
    hash.update(evidence.raw_payload);
    for value in [evidence.canonical_payload, evidence.transport_clocks] {
        hash.update([u8::from(value.is_some())]);
        hash.update(value.unwrap_or_default());
    }
    for value in [
        evidence.clocks,
        evidence.presentation_clocks,
        evidence.durations,
        evidence.sync_samples,
        evidence.recovery,
    ] {
        hash.update(value);
    }
    for value in [
        evidence.presentation_offset,
        evidence.coded_duration,
        evidence.initial_composition_offset,
        evidence.first_pts,
        evidence.end_pts,
        evidence.presented_first_pts,
        evidence.presented_end_pts,
    ] {
        value.hash(&mut hash);
    }
    hash.update([
        u8::from(evidence.transport_recovery_compatible),
        u8::from(evidence.continuous_decode),
    ]);
    hash.update(evidence.protected_samples.to_be_bytes());
    hash.finalize()
}

#[test]
fn repeated_samples_match_across_complete_and_fragmented_delivery() {
    // The private measurement harness repeats this same correctness workload.
    let count = std::env::var("DRMR_SAMPLE_BENCH_SAMPLES")
        .map_or(128, |value| value.parse::<u64>().unwrap());
    assert!((1..=1_000_000).contains(&count));
    let codec = std::env::var("DRMR_SAMPLE_BENCH_CODEC").unwrap_or_else(|_| "audio".into());
    let mode = std::env::var("DRMR_SAMPLE_BENCH_MODE").unwrap_or_else(|_| "both".into());
    assert!(matches!(mode.as_str(), "complete" | "fragmented" | "both"));
    let (sample, bytes, config) = match codec.as_str() {
        "audio" => (
            info(512),
            (0..512)
                .map(|index| u8::try_from(index % 251).unwrap())
                .collect(),
            audio(),
        ),
        "avc" => avc(),
        _ => panic!("unknown private benchmark codec"),
    };
    let mut first = None;
    for (delivery, chunk) in [
        ("complete", bytes.len()),
        ("fragmented", (bytes.len() / 2).max(1)),
    ] {
        if mode != "both" && mode != delivery {
            continue;
        }
        let started = std::time::Instant::now();
        let evidence = repeated(sample.clone(), &bytes, config.clone(), count, chunk);
        let elapsed = started.elapsed();
        assert_eq!(evidence.count, count);
        if let Some(first) = &first {
            equal(first, &evidence);
        }
        println!(
            "{}",
            serde_json::json!({"sample_benchmark":1,"codec":codec,"delivery":delivery,
            "samples":count,"sample_bytes":bytes.len(),"elapsed_ns":elapsed.as_nanos(),
            "evidence_sha256":hex::encode(fingerprint(&evidence))})
        );
        first = Some(evidence);
    }
}
