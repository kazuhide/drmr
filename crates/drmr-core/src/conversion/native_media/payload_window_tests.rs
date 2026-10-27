use super::*;
use zeroize::Zeroizing;

const KID: [u8; 16] = [0x31; 16];
const KEY: [u8; 16] = [0x42; 16];

fn descriptor() -> init::Track {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
    let stop = Stop::new();
    let mut reader = read::Reader::open(&source, &stop, false).unwrap();
    let layout = reader.layout().unwrap();
    init::parse(&layout).unwrap().tracks.remove(0)
}

struct Case {
    scheme: Scheme,
    iv: Vec<u8>,
    crypt: u8,
    skip: u8,
    spans: Vec<(u32, u32)>,
}
impl Case {
    fn size(&self) -> u32 {
        self.spans
            .iter()
            .map(|(clear, encrypted)| clear + encrypted)
            .sum()
    }
    fn encryption(&self) -> crypto::SampleEncryption {
        crypto::SampleEncryption {
            iv: Zeroizing::new(self.iv.clone()),
            subsamples: self
                .spans
                .iter()
                .map(|&(clear, encrypted)| crypto::Subsample { clear, encrypted })
                .collect(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Evidence {
    track: TrackDescriptor,
    info: SampleInfo,
    bytes: Vec<u8>,
    chunks: Vec<(u32, bool, usize)>,
}
fn with_sample<T>(
    case: &Case,
    buffered: bool,
    action: impl FnOnce(&mut read::Reader<'_>, &InputKeys, Payload<'_>) -> T,
) -> T {
    let size = case.size();
    let owner = crate::native::private_tempdir().unwrap();
    let path = owner.path().join("sample");
    let raw = (0_u8..=255).cycle().take(size as usize).collect::<Vec<_>>();
    std::fs::write(&path, &raw).unwrap();
    let stop = Stop::new();
    let mut reader = read::Reader::open(&path, &stop, false).unwrap();
    if !buffered {
        // Disable only the qualified-window gate; retain the original streaming path.
        reader.length = 0;
    }
    let mut track = descriptor();
    track.encryption = Some(crypto::TrackEncryption {
        scheme: case.scheme,
        kid: KID,
        iv_size: u8::try_from(case.iv.len()).unwrap(),
        constant_iv: None,
        crypt: case.crypt,
        skip: case.skip,
    });
    let info = SampleInfo {
        ordinal: 17,
        dts: -3072,
        pts: -2048,
        duration: 1024,
        size,
        range: 0..u64::from(size),
        flags: 0x0102_0304,
        is_sync: false,
        roll_distance: Some(-1),
        preroll_distance: Some(-2),
        protection: SampleProtection::Clear,
        chunk_offset: 0,
        is_last_chunk: false,
    };
    let result = action(
        &mut reader,
        &InputKeys::new(vec![super::super::ContentKey {
            id: KID,
            bytes: Zeroizing::new(KEY),
        }])
        .unwrap(),
        Payload {
            track: &track,
            info,
            encryption: Some(case.encryption()),
        },
    );
    assert_eq!(std::fs::read(path).unwrap(), raw);
    result
}

fn visit(case: &Case, buffered: bool) -> Evidence {
    let mut evidence = None;
    with_sample(case, buffered, |reader, keys, sample| {
        payload(reader, keys, sample, &mut |track, info, bytes| {
            let output = evidence.get_or_insert_with(|| Evidence {
                track: track.clone(),
                info: info.clone(),
                bytes: Vec::new(),
                chunks: Vec::new(),
            });
            output.bytes.extend(bytes);
            output
                .chunks
                .push((info.chunk_offset, info.is_last_chunk, bytes.len()));
            output.info = info.clone();
            Ok(())
        })
    })
    .unwrap();
    let mut evidence = evidence.unwrap();
    evidence.info.chunk_offset = 0;
    evidence
}

#[test]
fn complete_encrypted_windows_visit_once_and_match_streaming_content_and_metadata() {
    for (scheme, iv_size, crypt, skip) in [
        (Scheme::Cenc, 8, 0, 0),
        (Scheme::Cenc, 16, 0, 0),
        (Scheme::Cbcs, 16, 0, 0),
        (Scheme::Cbcs, 16, 1, 9),
        (Scheme::Cbcs, 16, 5, 2),
    ] {
        for spans in [
            vec![(5, 47), (0, 0), (11, 7), (3, 333), (2, 0)],
            vec![(5, 0), (3, 0)],
            vec![(5, 47), (11, u32::try_from(CHUNK_BYTES - 63).unwrap())],
        ] {
            let case = Case {
                scheme,
                iv: vec![0x23; iv_size],
                crypt,
                skip,
                spans,
            };
            let mut expected = visit(&case, false);
            let actual = visit(&case, true);
            assert_eq!(
                actual.chunks,
                [(0, true, case.size() as usize)],
                "{scheme:?}"
            );
            assert!(expected.chunks.len() > 1);
            expected.chunks = actual.chunks.clone();
            assert_eq!(actual, expected, "{scheme:?}");
        }
    }
}

#[test]
fn direct_large_and_single_span_samples_keep_their_streaming_partition() {
    for spans in [
        vec![(0, 37)],
        vec![(5, u32::try_from(CHUNK_BYTES).unwrap())],
    ] {
        let case = Case {
            scheme: Scheme::Cenc,
            iv: vec![0x23; 8],
            crypt: 0,
            skip: 0,
            spans,
        };
        assert_eq!(visit(&case, true), visit(&case, false));
    }
}

#[test]
fn exact_counter_capacity_remains_eligible_across_partial_subsamples() {
    for (counter, encrypted) in [(u128::MAX, 16), (u128::MAX - 1, 32)] {
        let case = Case {
            scheme: Scheme::Cenc,
            iv: counter.to_be_bytes().to_vec(),
            crypt: 0,
            skip: 0,
            spans: vec![(5, 1), (3, encrypted - 1)],
        };
        let mut expected = visit(&case, false);
        let actual = visit(&case, true);
        assert_eq!(actual.chunks, [(0, true, case.size() as usize)]);
        expected.chunks = actual.chunks.clone();
        assert_eq!(actual, expected);
    }
}

#[test]
fn later_counter_overflow_keeps_the_original_clear_callback_failure_precedence() {
    for (counter, encrypted) in [(u128::MAX, 17), (u128::MAX - 1, 33)] {
        let case = Case {
            scheme: Scheme::Cenc,
            iv: counter.to_be_bytes().to_vec(),
            crypt: 0,
            skip: 0,
            spans: vec![(5, encrypted - 1), (3, 1)],
        };
        for refuse in [false, true] {
            let run = |buffered| {
                let mut calls = Vec::new();
                let result = with_sample(&case, buffered, |reader, keys, sample| {
                    payload(reader, keys, sample, &mut |_, info, bytes| {
                        calls.push((info.clone(), bytes.to_vec()));
                        if refuse {
                            Err(ConversionError("callback-refused"))
                        } else {
                            Ok(())
                        }
                    })
                });
                (result, calls)
            };
            let direct = run(false);
            let buffered = run(true);
            assert_eq!(buffered, direct);
            assert_eq!(buffered.1[0].0.chunk_offset, 0);
            assert_eq!(buffered.1[0].1.len(), 5);
            assert_eq!(
                buffered.0,
                Err(ConversionError(if refuse {
                    "callback-refused"
                } else {
                    "native-cipher-counter-overflow"
                }))
            );
        }
    }
}

#[test]
fn invalid_subsample_totals_use_streaming_and_keep_the_first_callback_error() {
    let case = Case {
        scheme: Scheme::Cenc,
        iv: vec![0x23; 8],
        crypt: 0,
        skip: 0,
        spans: vec![(5, 16), (3, 17)],
    };
    for size in [case.size() - 1, case.size() + 1] {
        for buffered in [false, true] {
            let mut calls = 0;
            let result = with_sample(&case, buffered, |reader, keys, mut sample| {
                sample.info.size = size;
                sample.info.range.end = u64::from(size);
                payload(reader, keys, sample, &mut |_, info, bytes| {
                    calls += 1;
                    assert_eq!(info.chunk_offset, 0);
                    assert_eq!(bytes.len(), 5);
                    Err(ConversionError("callback-refused"))
                })
            });
            assert_eq!(result, Err(ConversionError("callback-refused")));
            assert_eq!(calls, 1);
        }
    }
}
