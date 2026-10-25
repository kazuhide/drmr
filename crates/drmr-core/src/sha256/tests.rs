use super::Sha256;
use sha2::{Digest as _, Sha256 as Oracle};

fn hash(bytes: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(bytes);
    digest.finalize()
}

#[test]
fn nist_vectors_match_the_complete_standard_digest() {
    for (input, expected) in [
        (
            b"".as_slice(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        ),
        (
            b"abc".as_slice(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        ),
        (
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq".as_slice(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        ),
    ] {
        assert_eq!(hex::encode(hash(input)), expected);
    }
    let mut million_a = Sha256::new();
    for _ in 0..1_000 {
        million_a.update([b'a'; 1_000]);
    }
    assert_eq!(
        hex::encode(million_a.finalize()),
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
}

#[test]
fn padding_and_chunk_boundaries_match_the_independent_rustcrypto_oracle() {
    let bytes = (0_u8..=u8::MAX)
        .cycle()
        .map(|byte| byte.wrapping_mul(37).wrapping_add(11))
        .take(1_048_593)
        .collect::<Vec<_>>();
    for length in [
        0, 1, 7, 15, 16, 31, 32, 55, 56, 63, 64, 65, 119, 120, 127, 128, 129, 255, 256, 4095, 4096,
        65535, 65536, 65537, 1_048_593,
    ] {
        let input = &bytes[..length];
        let expected: [u8; 32] = Oracle::digest(input).into();
        for chunk_size in [1, 7, 63, 64, 65, 1021, 65_536] {
            let mut actual = Sha256::new();
            actual.update([]);
            for chunk in input.chunks(chunk_size) {
                actual.update(chunk);
                actual.update([]);
            }
            assert_eq!(actual.finalize(), expected, "{length}/{chunk_size}");
        }
    }
}

#[test]
fn cloned_partial_states_can_finish_and_branch_independently() {
    let prefix = (0_u8..=u8::MAX)
        .cycle()
        .map(|byte| byte.wrapping_mul(73).wrapping_add(19))
        .take(65_537)
        .collect::<Vec<_>>();
    for length in [0, 1, 55, 56, 63, 64, 65, 127, 128, 4095, 65_537] {
        let prefix = &prefix[..length];
        let mut original = Sha256::new();
        original.update(prefix);
        let mut left = original.clone();
        let mut right = original.clone();
        left.update(b"left");
        right.update(b"right");
        assert_eq!(original.finalize(), Oracle::digest(prefix).as_slice());
        for (actual, suffix) in [(left, b"left".as_slice()), (right, b"right".as_slice())] {
            let mut expected = Oracle::new();
            expected.update(prefix);
            expected.update(suffix);
            assert_eq!(actual.finalize(), expected.finalize().as_slice());
        }
    }
}

#[test]
fn large_streams_keep_only_the_context_and_caller_buffer() {
    let block = vec![0x5a; 65_536];
    let mut actual = Sha256::new();
    let mut expected = Oracle::new();
    for _ in 0..1_024 {
        actual.update(block.as_slice());
        expected.update(block.as_slice());
    }
    assert_eq!(actual.finalize(), expected.finalize().as_slice());
    assert!(std::mem::size_of::<Sha256>() < 1_024);
}
