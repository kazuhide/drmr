use super::*;
use sha2::{Digest as _, Sha256 as ReferenceHash};

fn scalar_annex(data: &[u8]) -> Result<Vec<&[u8]>, ConversionError> {
    let mut marks = Vec::new();
    let mut at = 0;
    while at + 3 <= data.len() {
        let width = if data.get(at..at + 4) == Some(&[0, 0, 0, 1]) {
            4
        } else if data.get(at..at + 3) == Some(&[0, 0, 1]) {
            3
        } else {
            at += 1;
            continue;
        };
        marks.push((at, at + width));
        at += width;
        if marks.len() > 4096 {
            return Err(BAD);
        }
    }
    if marks.first().is_none_or(|mark| mark.0 != 0) {
        return Err(BAD);
    }
    let mut units = Vec::new();
    for (index, &(_, begin)) in marks.iter().enumerate() {
        let mut end = marks.get(index + 1).map_or(data.len(), |mark| mark.0);
        while end > begin && data[end - 1] == 0 {
            end -= 1;
        }
        if begin == end {
            return Err(BAD);
        }
        units.push(&data[begin..end]);
    }
    Ok(units)
}

#[test]
fn annex_boundaries_match_the_scalar_reference_for_every_short_byte_combination() {
    const ALPHABET: [u8; 4] = [0, 1, 3, 0x65];
    for length in 0..=8 {
        for mut pattern in 0..4_usize.pow(length) {
            let bytes: Vec<_> = (0..length)
                .map(|_| {
                    let byte = ALPHABET[pattern % 4];
                    pattern /= 4;
                    byte
                })
                .collect();
            assert_eq!(annex(&bytes), scalar_annex(&bytes), "{bytes:?}");
        }
    }
}

#[test]
fn annex_mixed_prefixes_trailing_zeros_and_search_boundaries_match_the_reference() {
    for width in [3, 4] {
        for position in (width + 2..=132).chain([255, 256, 257, 65_535, 65_536, 65_537]) {
            for trailing in 0..=8 {
                let mut bytes = vec![0x55; position];
                bytes[..width - 1].fill(0);
                bytes[width - 1] = 1;
                bytes[width] = 0x65;
                bytes.extend_from_slice(&[0, 0, 0, 1, 6, 0x80]);
                bytes.resize(bytes.len() + trailing, 0);
                assert_eq!(annex(&bytes), scalar_annex(&bytes));
            }
        }
    }
    for zeros in 0..=12 {
        let mut bytes = vec![0; zeros];
        bytes.extend_from_slice(&[0, 0, 1, 0x65, 0x80]);
        assert_eq!(annex(&bytes), scalar_annex(&bytes));
    }
}

#[test]
fn annex_nal_limit_and_empty_units_keep_the_scalar_refusals() {
    let mut bytes = [0, 0, 1, 0x65, 0x80].repeat(4096);
    assert_eq!(annex(&bytes).unwrap().len(), 4096);
    bytes.extend_from_slice(&[0, 0, 1, 0x65, 0x80]);
    assert_eq!(annex(&bytes), scalar_annex(&bytes));
    assert_eq!(annex(&bytes).unwrap_err(), BAD);
    for bytes in [
        &[0, 0, 1][..],
        &[0, 0, 0, 1][..],
        &[0, 0, 1, 0, 0, 1, 0x65][..],
        &[0, 0, 1, 0x65, 0, 0, 1][..],
    ] {
        assert_eq!(annex(bytes), scalar_annex(bytes));
        assert!(annex(bytes).is_err());
    }
}

#[test]
fn parsed_video_units_retain_the_independent_canonical_digest() {
    let config = Config::Video {
        sps: vec![0x67, 0x80],
        pps: vec![0x68, 0x80],
    };
    let bytes = [
        0, 0, 0, 1, 9, 0xf0, 0, 0, 1, 0x67, 0x80, 0, 0, 0, 1, 0x68, 0x80, 0, 0, 1, 6, 0x80, 0, 0,
        1, 0x65, 0x80, 0, 0,
    ];
    let units = annex(&bytes).unwrap();
    let mut parsed = Sha256::new();
    config.video_units(&units, &mut parsed).unwrap();
    let mut framed = Sha256::new();
    config.units(&bytes, true, &mut framed).unwrap();
    let mut reference = ReferenceHash::new();
    for unit in [[6, 0x80], [0x65, 0x80]] {
        reference.update(2_u64.to_be_bytes());
        reference.update(unit);
    }
    reference.update(2_u64.to_be_bytes());
    let expected: [u8; 32] = reference.finalize().into();
    assert_eq!(parsed.finalize(), expected);
    assert_eq!(framed.finalize(), expected);
}
