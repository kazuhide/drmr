use super::*;
use zeroize::{Zeroize as _, ZeroizeOnDrop};

fn scalar_rbsp(nal: &[u8], kind: u8) -> Result<Vec<u8>, ConversionError> {
    if nal.len() > 8 * 1024 * 1024 {
        return Err(ConversionError("ts-access-unit-size-limit"));
    }
    if nal.len() < 2 || nal[0] & 0x9f != kind || (matches!(kind, 5 | 7 | 8) && nal[0] & 0x60 == 0) {
        return Err(BAD);
    }
    let mut bytes = Vec::new();
    let mut zeros = 0;
    let mut at = 1;
    while at < nal.len() {
        let byte = nal[at];
        if zeros >= 2 {
            if byte == 3 {
                if nal.get(at + 1).is_none_or(|next| *next > 3) {
                    return Err(BAD);
                }
                zeros = 0;
                at += 1;
                continue;
            }
            if byte <= 2 {
                return Err(BAD);
            }
        }
        bytes.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        at += 1;
    }
    Ok(bytes)
}

#[test]
fn complete_unescaped_nals_borrow_the_original_bytes() {
    let mut nal = vec![0x55; 65_537];
    nal[0] = 0x65;
    nal[65_535..].fill(0);
    let Rbsp::Borrowed(body) = rbsp(&nal, 5).unwrap() else {
        panic!("an unescaped NAL must retain a borrowed body");
    };
    assert_eq!(body.as_ptr(), nal[1..].as_ptr());
    assert_eq!(body.len(), nal.len() - 1);
    assert_eq!(body, &nal[1..]);
}

#[test]
fn escaped_nals_own_a_wiping_buffer_and_leave_the_original_unchanged() {
    fn require_wiping<T: ZeroizeOnDrop>(_: &T) {}
    let nal = [0x65, 0, 0, 3, 0, 0x55, 0, 0, 3, 3, 0x80];
    let original = nal;
    let Rbsp::Owned(mut body) = rbsp(&nal, 5).unwrap() else {
        panic!("removing an escape requires an owned body");
    };
    require_wiping(&body);
    assert_eq!(&*body, &[0, 0, 0, 0x55, 0, 0, 3, 0x80]);
    assert_ne!(body.as_ptr(), nal[1..].as_ptr());
    body.as_mut_slice().zeroize();
    assert!(body.iter().all(|byte| *byte == 0));
    drop(body);
    assert_eq!(nal, original);
}

#[test]
fn complete_rbsp_matches_scalar_for_all_short_escape_combinations() {
    const ALPHABET: [u8; 5] = [0, 1, 2, 3, 4];
    for length in 0..=7 {
        for mut pattern in 0..5_usize.pow(length) {
            let mut nal = vec![0x65];
            for _ in 0..length {
                nal.push(ALPHABET[pattern % 5]);
                pattern /= 5;
            }
            assert_eq!(
                rbsp(&nal, 5).map(|body| body.to_vec()),
                scalar_rbsp(&nal, 5),
                "{nal:?}"
            );
        }
    }
}

#[test]
fn header_reference_bits_and_kinds_keep_the_scalar_refusals() {
    for header in 0..=u8::MAX {
        for kind in [0, 1, 5, 6, 7, 8, 9, 12, 31] {
            let nal = [header, 0x80];
            assert_eq!(
                rbsp(&nal, kind).map(|body| body.to_vec()),
                scalar_rbsp(&nal, kind)
            );
        }
    }
}

#[test]
fn valid_escapes_at_every_search_alignment_remove_exactly_one_layer() {
    for boundary in (1..=132).chain([65_535, 65_536, 65_537]) {
        for next in 0..=3 {
            let mut nal = vec![0x55; boundary];
            nal[0] = 0x65;
            nal.extend_from_slice(&[0, 0, 3, next, 0x55, 0, 0, 3, 3]);
            assert_eq!(
                rbsp(&nal, 5).map(|body| body.to_vec()),
                scalar_rbsp(&nal, 5)
            );
        }
    }
}

#[test]
fn dense_escapes_reset_input_zeros_and_still_validate_the_tail() {
    for next in 0..=3 {
        let mut nal = vec![0x65];
        let mut expected = Vec::new();
        for _ in 0..16_384 {
            nal.extend_from_slice(&[0, 0, 3]);
            expected.extend_from_slice(&[0, 0]);
        }
        nal.extend_from_slice(&[next, 0x55, 0, 0]);
        expected.extend_from_slice(&[next, 0x55, 0, 0]);
        assert_eq!(&*rbsp(&nal, 5).unwrap(), expected);
        assert_eq!(scalar_rbsp(&nal, 5).unwrap(), expected);

        for tail in [&[0][..], &[1], &[2], &[3], &[3, 4]] {
            let mut invalid = nal.clone();
            invalid.extend_from_slice(tail);
            assert_eq!(rbsp(&invalid, 5).err(), Some(BAD));
            assert_eq!(scalar_rbsp(&invalid, 5).err(), Some(BAD));
        }
    }
}

#[test]
fn sparse_and_adjacent_escape_runs_match_the_scalar_reference() {
    for gap in 0..=40 {
        let mut nal = vec![0x65, 0, 0, 3, 0, 0x55];
        nal.resize(nal.len() + gap, 0x55);
        nal.extend_from_slice(&[0, 0, 3, 1, 0x55, 0, 0, 3, 0, 0, 3, 3, 0x55, 0, 0]);
        assert_eq!(
            rbsp(&nal, 5).map(|body| body.to_vec()),
            scalar_rbsp(&nal, 5),
            "gap: {gap}"
        );
    }
}

#[test]
fn dense_prefixes_match_scalar_for_every_short_suffix() {
    const ALPHABET: [u8; 5] = [0, 1, 2, 3, 4];
    for escapes in 3..=5 {
        let mut prefix = vec![0x65];
        for _ in 0..escapes {
            prefix.extend_from_slice(&[0, 0, 3]);
        }
        for length in 0..=6 {
            for mut pattern in 0..5_usize.pow(length) {
                let mut nal = prefix.clone();
                for _ in 0..length {
                    nal.push(ALPHABET[pattern % 5]);
                    pattern /= 5;
                }
                assert_eq!(
                    rbsp(&nal, 5).map(|body| body.to_vec()),
                    scalar_rbsp(&nal, 5),
                    "{nal:?}"
                );
            }
        }
    }
}

#[test]
fn late_invalid_escapes_fail_even_after_an_owned_prefix_was_created() {
    for escaped in [false, true] {
        for boundary in [15, 16, 17, 31, 32, 33, 63, 64, 65, 65_535, 65_536, 65_537] {
            for tail in [
                &[0, 0, 0][..],
                &[0, 0, 1],
                &[0, 0, 2],
                &[0, 0, 3],
                &[0, 0, 3, 4],
            ] {
                let mut nal = vec![0x55; boundary];
                nal[0] = 0x65;
                if escaped {
                    nal[1..5].copy_from_slice(&[0, 0, 3, 0]);
                }
                nal.extend_from_slice(tail);
                let original = nal.clone();
                assert_eq!(rbsp(&nal, 5).err(), Some(BAD));
                assert_eq!(scalar_rbsp(&nal, 5).err(), Some(BAD));
                assert_eq!(nal, original);
            }
        }
    }
}

#[test]
fn the_full_nal_size_limit_is_checked_before_borrowing_or_copying() {
    let mut nal = vec![0x55; 8 * 1024 * 1024];
    nal[0] = 0x65;
    assert!(matches!(rbsp(&nal, 5).unwrap(), Rbsp::Borrowed(_)));
    let end = nal.len();
    nal[end - 4..].copy_from_slice(&[0, 0, 3, 4]);
    assert_eq!(rbsp(&nal, 5).err(), Some(BAD));
    nal[1..5].copy_from_slice(&[0, 0, 3, 0]);
    assert_eq!(rbsp(&nal, 5).err(), Some(BAD));
    nal[1..14].copy_from_slice(&[0, 0, 3, 0, 0, 3, 0, 0, 3, 0, 0, 3, 0]);
    assert_eq!(rbsp(&nal, 5).err(), Some(BAD));
    nal.push(0x80);
    assert_eq!(
        rbsp(&nal, 5).err(),
        Some(ConversionError("ts-access-unit-size-limit"))
    );
    assert!(rbsp(&[], 5).is_err());
    assert!(rbsp(&[0x65], 5).is_err());
}
