//! Bounded Apple HLS sample encryption and decryption.
//!
//! The cipher boundaries follow Apple's MPEG-2 Stream Encryption Format, sections 2.2
//! and 2.3.1.1. Transport headers, timestamps, key selection, and descriptors belong
//! to the caller. The specification is available at
//! <https://developer.apple.com/library/archive/documentation/AudioVideo/Conceptual/HLS_Sample_Encryption/Encryption/Encryption.html>.
//! Each call processes at most eight MiB; callers check cancellation
//! and hold their resource permit before and after this bounded operation.
use super::super::{Aes128Key, ConversionError, SegmentIv};
use aes::cipher::{BlockModeDecrypt as _, BlockModeEncrypt as _, KeyIvInit as _};
use zeroize::Zeroizing;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const BATCH_BLOCKS: usize = 32;
const INVALID: ConversionError = ConversionError("ts-sample-encryption-invalid");
const LIMIT: ConversionError = ConversionError("ts-sample-size-limit");

fn output(bytes: &[u8]) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    if bytes.len() > MAX_BYTES {
        return Err(LIMIT);
    }
    if bytes.is_empty() {
        return Err(INVALID);
    }
    // Reserve the full result before any plaintext is present. H.264 only shrinks.
    let mut result = Zeroizing::new(Vec::new());
    result.try_reserve_exact(bytes.len()).map_err(|_| LIMIT)?;
    Ok(result)
}
fn start_code(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes.get(at..at.checked_add(4)?) == Some(&[0, 0, 0, 1]) {
        Some(4)
    } else if bytes.get(at..at.checked_add(3)?) == Some(&[0, 0, 1]) {
        Some(3)
    } else {
        None
    }
}
fn next_start(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() {
        if start_code(bytes, at).is_some() {
            break;
        }
        at += 1;
    }
    at
}
fn append_nal(bytes: &[u8], strip_outer: bool, out: &mut Vec<u8>) -> Result<(), ConversionError> {
    let mut at = 0;
    let mut zeros = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if zeros == 2 {
            if byte == 3 {
                if bytes.get(at + 1).is_none_or(|next| *next > 3) {
                    return Err(INVALID);
                }
                if strip_outer {
                    // Reset input-layer state, preserving an original inner 00 00 03.
                    zeros = 0;
                    at += 1;
                    continue;
                }
            } else if byte < 3 {
                return Err(INVALID);
            }
        }
        out.push(byte);
        zeros = if byte == 0 { zeros + 1 } else { 0 };
        at += 1;
    }
    Ok(())
}
fn decrypt_blocks(bytes: &mut [u8], key: &Aes128Key, iv: &SegmentIv, skip: usize) {
    let mut cipher = cbc::Decryptor::<aes::Aes128>::new((&*key.0).into(), (&*iv.0).into());
    if skip == 0 {
        let (blocks, _) = aes::Block::slice_as_chunks_mut(bytes);
        for batch in blocks.chunks_mut(BATCH_BLOCKS) {
            cipher.decrypt_blocks(batch);
        }
        return;
    }
    let mut blocks = Zeroizing::new([aes::Block::default(); BATCH_BLOCKS]);
    let mut offsets = [0; BATCH_BLOCKS];
    let mut at = 0;
    while bytes.len() - at > 16 {
        let mut count = 0;
        while count < BATCH_BLOCKS && bytes.len() - at > 16 {
            offsets[count] = at;
            blocks[count].copy_from_slice(&bytes[at..at + 16]);
            count += 1;
            at += 16;
            at += skip.min(bytes.len() - at);
        }
        cipher.decrypt_blocks(&mut blocks[..count]);
        for (offset, block) in offsets[..count].iter().zip(&blocks[..count]) {
            bytes[*offset..*offset + 16].copy_from_slice(block);
        }
    }
}
fn encrypt_blocks(
    bytes: &mut [u8],
    key: &Aes128Key,
    iv: &SegmentIv,
    skip: usize,
) -> Result<(), ConversionError> {
    let mut cipher = cbc::Encryptor::<aes::Aes128>::new((&*key.0).into(), (&*iv.0).into());
    let mut at = 0;
    while bytes.len() - at >= 16 {
        if skip != 0 && bytes.len() - at == 16 {
            break;
        }
        cipher.encrypt_block((&mut bytes[at..at + 16]).try_into().map_err(|_| INVALID)?);
        at += 16;
        at += skip.min(bytes.len() - at);
    }
    Ok(())
}

/// Protect already escaped Annex-B NAL units, adding the required outer escapes.
pub(super) fn encrypt_h264_annex_b(
    bytes: &[u8],
    key: &Aes128Key,
    iv: &SegmentIv,
) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    if bytes.len() > MAX_BYTES {
        return Err(LIMIT);
    }
    if bytes.is_empty() {
        return Err(INVALID);
    }
    // A second escape layer can grow by at most one byte per two input bytes.
    // Reserve its complete bounded capacity before retaining any plaintext.
    let capacity = bytes
        .len()
        .saturating_add(bytes.len() / 2 + 1)
        .min(MAX_BYTES);
    let mut out = Zeroizing::new(Vec::new());
    out.try_reserve_exact(capacity).map_err(|_| LIMIT)?;
    let mut at = 0;
    while at < bytes.len() {
        let width = start_code(bytes, at).ok_or(INVALID)?;
        let begin = at.checked_add(width).ok_or(INVALID)?;
        let next = next_start(bytes, begin);
        let mut end = next;
        while end > begin && bytes[end - 1] == 0 {
            end -= 1;
        }
        let nal = bytes
            .get(begin..end)
            .filter(|n| !n.is_empty())
            .ok_or(INVALID)?;
        let kind = nal[0] & 31;
        if nal[0] & 0x80 != 0 || !(1..=23).contains(&kind) {
            return Err(INVALID);
        }
        extend_bounded(&mut out, &bytes[at..begin])?;
        let mut protected = output(nal)?;
        append_nal(nal, false, &mut protected)?;
        if matches!(kind, 1 | 5) && nal.len() > 48 {
            encrypt_blocks(&mut protected[32..], key, iv, 144)?;
            let mut zeros = 0;
            for &byte in protected.iter() {
                if zeros == 2 && byte <= 3 {
                    extend_bounded(&mut out, &[3])?;
                    zeros = 0;
                }
                extend_bounded(&mut out, &[byte])?;
                zeros = if byte == 0 { zeros + 1 } else { 0 };
            }
        } else {
            extend_bounded(&mut out, &protected)?;
        }
        extend_bounded(&mut out, &bytes[end..next])?;
        at = next;
    }
    Ok(out)
}

fn extend_bounded(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), ConversionError> {
    let needed = out.len().checked_add(bytes.len()).ok_or(LIMIT)?;
    if needed > out.capacity() || needed > MAX_BYTES {
        return Err(LIMIT);
    }
    out.extend_from_slice(bytes);
    Ok(())
}

/// Protect complete AAC-LC ADTS frames, resetting CBC at every frame boundary.
pub(super) fn encrypt_aac_adts(
    bytes: &[u8],
    key: &Aes128Key,
    iv: &SegmentIv,
) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    let mut out = output(bytes)?;
    out.extend_from_slice(bytes);
    let mut at = 0;
    let mut configuration = None;
    while at < out.len() {
        let h = out
            .get(at..at.checked_add(7).ok_or(INVALID)?)
            .ok_or(INVALID)?;
        let current = super::canonical::Config::audio_adts(h).map_err(|_| INVALID)?;
        if configuration.as_ref().is_some_and(|old| old != &current) {
            return Err(INVALID);
        }
        configuration = Some(current);
        let length =
            (usize::from(h[3] & 3) << 11) | (usize::from(h[4]) << 3) | usize::from(h[5] >> 5);
        if length <= 7 {
            return Err(INVALID);
        }
        let end = at
            .checked_add(length)
            .filter(|n| *n <= out.len())
            .ok_or(INVALID)?;
        if length > 23 {
            encrypt_blocks(&mut out[at + 23..end], key, iv, 0)?;
        }
        at = end;
    }
    Ok(out)
}
/// Decrypt complete Annex-B bytes, preserving start codes, non-VCL units, and inner escapes.
pub(super) fn decrypt_h264_annex_b(
    bytes: &[u8],
    key: &Aes128Key,
    iv: &SegmentIv,
) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    let mut out = output(bytes)?;
    let mut at = 0;
    while at < bytes.len() {
        let width = start_code(bytes, at).ok_or(INVALID)?;
        let begin = at.checked_add(width).ok_or(INVALID)?;
        let next = next_start(bytes, begin);
        let mut end = next;
        // Annex-B trailing_zero_8bits are outside the protected NAL and stay unchanged.
        while end > begin && bytes[end - 1] == 0 {
            end -= 1;
        }
        let nal = bytes
            .get(begin..end)
            .filter(|nal| !nal.is_empty())
            .ok_or(INVALID)?;
        let kind = nal[0] & 31;
        if nal[0] & 0x80 != 0 || !(1..=23).contains(&kind) {
            return Err(INVALID);
        }
        out.extend_from_slice(&bytes[at..begin]);
        let output_begin = out.len();
        let protected = matches!(kind, 1 | 5) && nal.len() > 48;
        append_nal(nal, protected, &mut out)?;
        if protected {
            if out.len() - output_begin <= 48 {
                return Err(INVALID);
            }
            let leader = output_begin.checked_add(32).ok_or(INVALID)?;
            let encrypted = out.get_mut(leader..).ok_or(INVALID)?;
            // The last complete 16-byte block is clear when no byte follows it.
            decrypt_blocks(encrypted, key, iv, 144);
        }
        out.extend_from_slice(&bytes[end..next]);
        at = next;
    }
    Ok(out)
}
/// Decrypt complete seven-byte, no-CRC AAC-LC ADTS frames, resetting the IV each time.
pub(super) fn decrypt_aac_adts(
    bytes: &[u8],
    key: &Aes128Key,
    iv: &SegmentIv,
) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    let mut out = output(bytes)?;
    out.extend_from_slice(bytes);
    let mut at = 0;
    let mut configuration = None;
    while at < out.len() {
        let h = out
            .get(at..at.checked_add(7).ok_or(INVALID)?)
            .ok_or(INVALID)?;
        let current = super::canonical::Config::audio_adts(h).map_err(|_| INVALID)?;
        if configuration
            .as_ref()
            .is_some_and(|previous| previous != &current)
        {
            return Err(INVALID);
        }
        configuration = Some(current);
        let length =
            (usize::from(h[3] & 3) << 11) | (usize::from(h[4]) << 3) | usize::from(h[5] >> 5);
        if length <= 7 {
            return Err(INVALID);
        }
        let end = at
            .checked_add(length)
            .filter(|end| *end <= out.len())
            .ok_or(INVALID)?;
        if length > 23 {
            decrypt_blocks(&mut out[at + 23..end], key, iv, 0);
        }
        at = end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    // NIST SP 800-38A, F.2.1: independent AES-128 CBC known-answer data.
    // https://nvlpubs.nist.gov/nistpubs/Legacy/SP/nistspecialpublication800-38a.pdf
    const KEY: &str = "2b7e151628aed2a6abf7158809cf4f3c";
    const IV: &str = "000102030405060708090a0b0c0d0e0f";
    const PLAIN: &str = concat!(
        "6bc1bee22e409f96e93d7e117393172a",
        "ae2d8a571e03ac9c9eb76fac45af8e51",
        "30c81c46a35ce411e5fbc1191a0a52ef",
        "f69f2445df4f9b17ad2b417be66c3710"
    );
    const CIPHER: &str = concat!(
        "7649abac8119b246cee98e9b12e9197d",
        "5086cb9b507219ee95db113a917678b2",
        "73bed6b8e3c1743b7116e69e22229516",
        "3ff1caa1681fac09120eca307586e1a7"
    );
    fn parameters() -> (Aes128Key, SegmentIv) {
        (
            Aes128Key::from_hex(KEY).unwrap(),
            SegmentIv::from_hex(IV).unwrap(),
        )
    }
    #[test]
    fn decrypt_batches_match_a_scalar_cbc_oracle_across_batch_skip_and_tail_boundaries() {
        use aes::cipher::{BlockCipherDecrypt as _, KeyInit as _};
        let (key, iv) = parameters();
        let aes = aes::Aes128::new((&*key.0).into());
        for skip in [0, 144] {
            for length in (0..=33).chain([511, 512, 513, 5119, 5120, 5121, 8191, 100_001]) {
                let mut actual = (0..length)
                    .map(|i| u8::try_from(i % 251).unwrap())
                    .collect::<Vec<_>>();
                let mut expected = actual.clone();
                let mut previous = *iv.0;
                for at in (0..length).step_by(16 + skip) {
                    let remaining = length - at;
                    if remaining < 16 || (skip != 0 && remaining == 16) {
                        break;
                    }
                    let block = &mut expected[at..at + 16];
                    let original = <[u8; 16]>::try_from(&*block).unwrap();
                    aes.decrypt_block(block.try_into().unwrap());
                    for (byte, mask) in block.iter_mut().zip(previous) {
                        *byte ^= mask;
                    }
                    previous = original;
                }
                decrypt_blocks(&mut actual, &key, &iv, skip);
                assert_eq!(actual, expected, "length={length}, skip={skip}");
            }
        }
    }
    #[test]
    fn encryption_matches_independent_nist_aac_vectors_at_both_rates() {
        let (key, iv) = parameters();
        let mut plain = vec![0x55; 16];
        plain.extend(hex::decode(PLAIN).unwrap());
        plain.extend([0x66; 9]);
        let mut cipher = vec![0x55; 16];
        cipher.extend(hex::decode(CIPHER).unwrap());
        cipher.extend([0x66; 9]);
        for rate_index in [3, 4] {
            let mut original = adts(&plain);
            let mut expected = adts(&cipher);
            original[2] = (original[2] & !0x3c) | (rate_index << 2);
            expected[2] = (expected[2] & !0x3c) | (rate_index << 2);
            let original = [original.clone(), original].concat();
            let expected = [expected.clone(), expected].concat();
            assert_eq!(
                encrypt_aac_adts(&original, &key, &iv).unwrap().as_slice(),
                expected
            );
        }
    }
    #[test]
    fn encryption_matches_independent_nist_avc_vectors_across_skips() {
        let (key, iv) = parameters();
        let plain = hex::decode(PLAIN).unwrap();
        let cipher = hex::decode(CIPHER).unwrap();
        let mut original = vec![0x55; 32 + 3 * 160 + 17];
        original[0] = 0x65;
        let mut expected = original.clone();
        for (index, block) in cipher.as_chunks::<16>().0.iter().enumerate() {
            let at = 32 + index * 160;
            original[at..at + 16].copy_from_slice(&plain[index * 16..index * 16 + 16]);
            expected[at..at + 16].copy_from_slice(block);
        }
        let original = [[0, 0, 1].as_slice(), &original].concat();
        let expected = [[0, 0, 1].as_slice(), &expected].concat();
        assert_eq!(
            encrypt_h264_annex_b(&original, &key, &iv)
                .unwrap()
                .as_slice(),
            expected
        );
    }
    #[test]
    fn encryption_inserts_outer_escapes_without_losing_original_bytes() {
        let (key, iv) = parameters();
        let mut block = [0; 16];
        decrypt_blocks(&mut block, &key, &iv, 0);
        let mut original = vec![0, 0, 1, 0x65];
        original.extend([0x55; 31]);
        original.extend(block);
        original.push(0x55);
        let cipher = encrypt_h264_annex_b(&original, &key, &iv).unwrap();
        assert!(cipher.len() > original.len());
        assert!(cipher.windows(3).any(|x| x == [0, 0, 3]));
        assert_eq!(
            decrypt_h264_annex_b(&cipher, &key, &iv).unwrap().as_slice(),
            original
        );
    }
    fn adts(payload: &[u8]) -> Vec<u8> {
        let length = payload.len() + 7;
        assert!(length <= 8191);
        let mut frame = vec![
            0xff,
            0xf1,
            0x4c,
            0x80 | u8::try_from(length >> 11).unwrap(),
            u8::try_from((length >> 3) & 255).unwrap(),
            u8::try_from((length & 7) << 5).unwrap() | 0x1f,
            0xfc,
        ];
        frame.extend_from_slice(payload);
        frame
    }
    #[test]
    fn aac_uses_independent_cbc_vector_and_resets_every_frame() {
        let (key, iv) = parameters();
        let mut plain = vec![0x55; 16];
        plain.extend(hex::decode(PLAIN).unwrap());
        plain.extend([0x66; 9]);
        let mut cipher = vec![0x55; 16];
        cipher.extend(hex::decode(CIPHER).unwrap());
        cipher.extend([0x66; 9]);
        for index in [3, 4] {
            let mut expected_frame = adts(&plain);
            let mut encrypted_frame = adts(&cipher);
            expected_frame[2] = (expected_frame[2] & !0x3c) | (index << 2);
            encrypted_frame[2] = (encrypted_frame[2] & !0x3c) | (index << 2);
            let expected = [expected_frame.clone(), expected_frame].concat();
            let encrypted = [encrypted_frame.clone(), encrypted_frame].concat();
            let actual = decrypt_aac_adts(&encrypted, &key, &iv).unwrap();
            assert_eq!(actual.as_slice(), expected);
        }
    }
    #[test]
    fn avc_uses_independent_cbc_vector_across_skips_and_resets_every_nal() {
        let (key, iv) = parameters();
        let plain = hex::decode(PLAIN).unwrap();
        let cipher = hex::decode(CIPHER).unwrap();
        let mut encrypted_nal = vec![0x55; 32 + 3 * 160 + 17];
        encrypted_nal[0] = 0x65;
        let mut expected_nal = encrypted_nal.clone();
        for (index, block) in cipher.as_chunks::<16>().0.iter().enumerate() {
            let at = 32 + index * 160;
            encrypted_nal[at..at + 16].copy_from_slice(block);
            expected_nal[at..at + 16].copy_from_slice(&plain[index * 16..index * 16 + 16]);
        }
        let prefix = [0, 0, 0, 1, 0x67, 0x11, 0, 0, 1, 0x68, 0x22];
        let mut input = prefix.to_vec();
        let mut expected = prefix.to_vec();
        for _ in 0..2 {
            input.extend([0, 0, 1]);
            input.extend_from_slice(&encrypted_nal);
            expected.extend([0, 0, 1]);
            expected.extend_from_slice(&expected_nal);
        }
        input.extend([0, 0]);
        expected.extend([0, 0]);
        let actual = decrypt_h264_annex_b(&input, &key, &iv).unwrap();
        assert_eq!(actual.as_slice(), expected);
    }
    #[test]
    fn avc_exact_boundary_retains_short_nals_and_decrypts_only_above_48_bytes() {
        let (key, iv) = parameters();
        for length in [32, 47, 48, 49, 64] {
            let mut nal = vec![0x55; length];
            nal[0] = 0x41;
            let mut expected = nal.clone();
            if length > 48 {
                nal[32..48].copy_from_slice(&hex::decode(CIPHER).unwrap()[..16]);
                expected[32..48].copy_from_slice(&hex::decode(PLAIN).unwrap()[..16]);
            }
            let input = [[0, 0, 0, 1].as_slice(), &nal].concat();
            let expected = [[0, 0, 0, 1].as_slice(), &expected].concat();
            assert_eq!(
                decrypt_h264_annex_b(&input, &key, &iv).unwrap().as_slice(),
                expected
            );
        }
    }
    fn outer_escape(data: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        let mut zeros = 0;
        for &byte in data {
            if zeros == 2 && byte <= 3 {
                output.push(3);
                zeros = 0;
            }
            output.push(byte);
            zeros = if byte == 0 { zeros + 1 } else { 0 };
        }
        output
    }
    #[test]
    fn avc_removes_one_outer_escape_layer_and_preserves_original_inner_bytes() {
        let (key, iv) = parameters();
        let mut plain = vec![0x55; 49];
        plain[0] = 0x65;
        plain[4..8].copy_from_slice(&[0, 0, 3, 0]);
        plain[32..48].copy_from_slice(&hex::decode(PLAIN).unwrap()[..16]);
        let mut encrypted = plain.clone();
        encrypted[32..48].copy_from_slice(&hex::decode(CIPHER).unwrap()[..16]);
        let escaped = outer_escape(&encrypted);
        assert!(escaped.windows(5).any(|w| w == [0, 0, 3, 3, 0]));
        let input = [[0, 0, 1].as_slice(), &escaped].concat();
        let expected = [[0, 0, 1].as_slice(), &plain].concat();
        assert_eq!(
            decrypt_h264_annex_b(&input, &key, &iv).unwrap().as_slice(),
            expected
        );
        let mut short = vec![0x55; 48];
        short[0] = 0x41;
        short[4..8].copy_from_slice(&[0, 0, 3, 0]);
        let short = [[0, 0, 1].as_slice(), &short].concat();
        assert_eq!(
            decrypt_h264_annex_b(&short, &key, &iv).unwrap().as_slice(),
            short
        );
    }
    #[test]
    fn malformed_or_oversized_elementary_streams_refuse_without_partial_plaintext() {
        let (key, iv) = parameters();
        assert_eq!(encrypt_h264_annex_b(&[], &key, &iv).unwrap_err(), INVALID);
        for data in [
            vec![],
            vec![0x65],
            vec![0, 0, 1],
            vec![0, 0, 1, 0xe5],
            [vec![0, 0, 1, 0x65], vec![0x55; 50], vec![0, 0, 3, 4]].concat(),
            [vec![0, 0, 1, 0x65], vec![0x55; 50], vec![0, 0, 3]].concat(),
        ] {
            assert!(decrypt_h264_annex_b(&data, &key, &iv).is_err());
            assert!(encrypt_h264_annex_b(&data, &key, &iv).is_err());
        }
        for data in [vec![], vec![0xff; 6], adts(&[]), {
            let mut frame = adts(&[0x55; 32]);
            frame.pop();
            frame
        }] {
            assert!(decrypt_aac_adts(&data, &key, &iv).is_err());
            assert!(encrypt_aac_adts(&data, &key, &iv).is_err());
        }
        for (at, mask, replacement) in [
            (1, 255, 0xf0),
            (2, 0x3c, 5 << 2),
            (2, 0xc0, 0x80),
            (6, 3, 1),
        ] {
            let mut frame = adts(&[0x55; 32]);
            frame[at] = (frame[at] & !mask) | replacement;
            assert_eq!(decrypt_aac_adts(&frame, &key, &iv).unwrap_err(), INVALID);
            assert_eq!(encrypt_aac_adts(&frame, &key, &iv).unwrap_err(), INVALID);
        }
        let first = adts(&[0x55; 32]);
        let mut other_rate = first.clone();
        other_rate[2] = (other_rate[2] & !0x3c) | (4 << 2);
        assert_eq!(
            decrypt_aac_adts(&[first.clone(), other_rate].concat(), &key, &iv).unwrap_err(),
            INVALID
        );
        let mut second = first.clone();
        second[3] ^= 0x40;
        assert_eq!(
            decrypt_aac_adts(&[first, second].concat(), &key, &iv).unwrap_err(),
            INVALID
        );
        let data = vec![0x55; 8 * 1024 * 1024 + 1];
        assert_eq!(
            decrypt_h264_annex_b(&data, &key, &iv).unwrap_err().0,
            "ts-sample-size-limit"
        );
        assert_eq!(
            encrypt_h264_annex_b(&data, &key, &iv).unwrap_err().0,
            "ts-sample-size-limit"
        );
        assert_eq!(
            decrypt_aac_adts(&data, &key, &iv).unwrap_err().0,
            "ts-sample-size-limit"
        );
        assert_eq!(
            encrypt_aac_adts(&data, &key, &iv).unwrap_err().0,
            "ts-sample-size-limit"
        );
    }

    #[test]
    fn avc_final_full_block_is_clear_at_208_bytes_and_protected_at_209() {
        let (key, iv) = parameters();
        let encrypted = hex::decode(CIPHER).unwrap();
        let plain = hex::decode(PLAIN).unwrap();
        for length in [208, 209] {
            let mut nal = vec![0x55; length];
            nal[0] = 0x41;
            nal[32..48].copy_from_slice(&encrypted[..16]);
            nal[192..208].copy_from_slice(&encrypted[16..32]);
            let mut expected = nal.clone();
            expected[32..48].copy_from_slice(&plain[..16]);
            if length == 209 {
                expected[192..208].copy_from_slice(&plain[16..32]);
            }
            assert_eq!(
                decrypt_h264_annex_b(&[[0, 0, 1].as_slice(), &nal].concat(), &key, &iv)
                    .unwrap()
                    .as_slice(),
                [[0, 0, 1].as_slice(), &expected].concat()
            );
        }
    }
    #[test]
    fn aac_preserves_every_short_frame_and_every_trailer_length_without_padding() {
        let (key, iv) = parameters();
        for count in 1..32 {
            let frame = adts(&vec![0x55; count]);
            assert_eq!(
                decrypt_aac_adts(&frame, &key, &iv).unwrap().as_slice(),
                frame
            );
        }
        for tail in 0..16 {
            let mut cipher = vec![0x55; 16];
            cipher.extend(hex::decode(CIPHER).unwrap());
            cipher.extend(vec![0x66; tail]);
            let mut expected = vec![0x55; 16];
            expected.extend(hex::decode(PLAIN).unwrap());
            expected.extend(vec![0x66; tail]);
            assert_eq!(
                decrypt_aac_adts(&adts(&cipher), &key, &iv)
                    .unwrap()
                    .as_slice(),
                adts(&expected)
            );
        }
    }
    #[test]
    fn an_outer_escape_layer_cannot_make_an_unencrypted_short_nal_protected() {
        let (key, iv) = parameters();
        let mut short = vec![0x55; 48];
        short[0] = 0x41;
        short[4..8].copy_from_slice(&[0, 0, 3, 0]);
        let malformed = [[0, 0, 1].as_slice(), &outer_escape(&short)].concat();
        assert_eq!(
            decrypt_h264_annex_b(&malformed, &key, &iv).unwrap_err(),
            INVALID
        );
    }
    fn real_tool(tool: &str, arguments: Vec<String>) -> crate::toolrun::Completed {
        use crate::{
            events::NoSink,
            toolrun::{RunOptions, Secrets, ToolRunner},
        };
        use std::{sync::Arc, time::Duration};
        let runner = ToolRunner::new(
            Arc::new(Secrets::with([
                "101112131415161718191a1b1c1d1e1f",
                "303132333435363738393a3b3c3d3e3f",
            ])),
            Arc::new(NoSink),
        );
        let mut argv = vec![tool.to_string()];
        argv.extend(arguments);
        let shown = [tool.to_string(), "[arguments redacted]".into()];
        let result = runner
            .run_tool(
                &argv,
                tool,
                &RunOptions {
                    timeout: Some(Duration::from_secs(30)),
                    argv_redacted: Some(&shown),
                    log_stdout: false,
                    max_output_chars: 0,
                    ..RunOptions::default()
                },
            )
            .unwrap();
        assert_eq!(
            result.returncode,
            Some(0),
            "required real media tool failed: {tool}"
        );
        result
    }
    fn fixture_pes(directory: &std::path::Path) -> Vec<Vec<u8>> {
        fixture_role_pes(directory, 5, false)
    }
    fn fixture_role_pes(directory: &std::path::Path, segments: usize, audio: bool) -> Vec<Vec<u8>> {
        let mut output = Vec::new();
        for number in 1..=segments {
            let file = directory.join(format!("seg_{number}.ts"));
            assert!(std::fs::metadata(&file).unwrap().len() <= MAX_BYTES as u64);
            let bytes = std::fs::read(file).unwrap();
            assert_eq!(bytes.len() % 188, 0);
            let mut pid = None;
            let mut pes = Vec::new();
            for packet in bytes.as_chunks::<188>().0 {
                assert_eq!(packet[0], 0x47);
                let mode = (packet[3] >> 4) & 3;
                if mode & 1 == 0 {
                    continue;
                }
                let at = if mode & 2 == 0 {
                    4
                } else {
                    5 + usize::from(packet[4])
                };
                let payload = &packet[at..];
                let id = (u16::from(packet[1] & 31) << 8) | u16::from(packet[2]);
                let start = packet[1] & 0x40 != 0;
                if start
                    && payload.len() >= 4
                    && payload[..3] == [0, 0, 1]
                    && if audio {
                        (0xc0..=0xdf).contains(&payload[3])
                    } else {
                        (0xe0..=0xef).contains(&payload[3])
                    }
                {
                    assert!(pid.is_none_or(|previous| previous == id));
                    pid = Some(id);
                    if !pes.is_empty() {
                        output.push(std::mem::take(&mut pes));
                    }
                }
                if pid == Some(id) {
                    pes.extend_from_slice(payload);
                }
            }
            assert_ne!(pes, [] as [u8; 0]);
            output.push(pes);
        }
        output
    }
    fn frame_rows(data: &[u8]) -> Vec<String> {
        std::str::from_utf8(data)
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(str::to_owned)
            .collect()
    }
    fn package_video_pair(source: &std::path::Path, work: &std::path::Path) {
        for encrypted in [false, true] {
            let dir = work.join(if encrypted { "cipher" } else { "clear" });
            crate::conversion::files::create_dir(&dir).unwrap();
            let source_path = crate::conversion::files::tool_path(source).unwrap();
            let template =
                crate::conversion::files::tool_path(&dir.join("seg_$Number$.ts")).unwrap();
            let mut args = vec![
                format!(
                    "input={source_path},stream=video,segment_template={template},playlist_name=media.m3u8"
                ),
                "--hls_master_playlist_output".into(),
                crate::conversion::files::tool_path(&dir.join("master.m3u8")).unwrap(),
                "--segment_duration".into(),
                "2".into(),
                "--fragment_duration".into(),
                "2".into(),
                "--transport_stream_timestamp_offset_ms".into(),
                "0".into(),
                "--mp4_reset_initial_composition_offset_to_zero=false".into(),
            ];
            if encrypted {
                args.extend([
                "--enable_raw_key_encryption".into(), "--keys".into(),
                "label=:key_id=000102030405060708090a0b0c0d0e0f:key=101112131415161718191a1b1c1d1e1f".into(),
                "--iv".into(), "303132333435363738393a3b3c3d3e3f".into(),
                "--protection_scheme".into(), "cbcs".into(), "--clear_lead".into(), "0".into(),
                "--hls_key_uri".into(), "https://operator.invalid/public-fixture".into()]);
            }
            real_tool("packager", args);
        }
    }
    #[test]
    fn real_shaka_bbb_all_240_pes_including_final_cipher_packet_match_and_decode() {
        use sha2::{Digest as _, Sha256};
        use std::io::Write as _;
        // The existing ten-second fixture is attributed under CC BY 3.0 in
        // crates/drmr/tests/fixtures/bbb/README.md. Shaka independently encrypts it;
        // production decryption is never used to generate the expected bytes.
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../drmr/tests/fixtures/bbb/clip.mp4");
        let before = Sha256::digest(std::fs::read(&source).unwrap());
        let work = crate::native::private_tempdir().unwrap();
        package_video_pair(&source, work.path());
        let clear = fixture_pes(&work.path().join("clear"));
        let cipher = fixture_pes(&work.path().join("cipher"));
        assert_eq!(clear.len(), 240);
        assert_eq!(cipher.len(), 240);
        let key = Aes128Key::from_hex("101112131415161718191a1b1c1d1e1f").unwrap();
        let iv = SegmentIv::from_hex("303132333435363738393a3b3c3d3e3f").unwrap();
        let mut expected = Zeroizing::new(Vec::new());
        let mut decrypted = Zeroizing::new(Vec::new());
        for (index, (plain, encrypted)) in clear.iter().zip(&cipher).enumerate() {
            let a = 9 + usize::from(plain[8]);
            let b = 9 + usize::from(encrypted[8]);
            assert_eq!(
                &plain[6..a],
                &encrypted[6..b],
                "original PES flags/PTS/DTS at {index}"
            );
            let actual = decrypt_h264_annex_b(&encrypted[b..], &key, &iv).unwrap();
            assert_eq!(
                encrypt_h264_annex_b(&plain[a..], &key, &iv)
                    .unwrap()
                    .as_slice(),
                &encrypted[b..],
                "independent Shaka encryption at {index}"
            );
            assert_eq!(
                actual.as_slice(),
                &plain[a..],
                "complete original ES at {index}"
            );
            expected.extend_from_slice(&plain[a..]);
            decrypted.extend_from_slice(&actual);
        }
        // Index 239 is intentionally included: FFmpeg 9's keyed HLS decoder misses
        // that final encrypted packet at EOF, so its output is not our oracle.
        let last = &cipher[239];
        let header = 9 + usize::from(last[8]);
        assert_ne!(
            &last[header..],
            &clear[239][9 + usize::from(clear[239][8])..]
        );
        let mut frames = Vec::new();
        for (name, data) in [("expected.h264", &expected), ("decrypted.h264", &decrypted)] {
            let path = work.path().join(name);
            crate::native::create_private_new(&path)
                .unwrap()
                .write_all(data)
                .unwrap();
            let result = real_tool(
                "ffmpeg",
                vec![
                    "-v".into(),
                    "error".into(),
                    "-xerror".into(),
                    "-nostdin".into(),
                    "-threads".into(),
                    "1".into(),
                    "-f".into(),
                    "h264".into(),
                    "-i".into(),
                    crate::conversion::files::tool_path(&path).unwrap(),
                    "-threads".into(),
                    "1".into(),
                    "-f".into(),
                    "framemd5".into(),
                    "-".into(),
                ],
            );
            let rows = frame_rows(&result.stdout);
            assert_eq!(rows.len(), 240);
            frames.push(rows);
        }
        assert_eq!(frames[0], frames[1]);
        assert_eq!(Sha256::digest(std::fs::read(source).unwrap()), before);
    }
    fn prepare_real_audio_fixture(
        work: &std::path::Path,
        source: &std::path::Path,
    ) -> (std::path::PathBuf, Vec<u8>) {
        use sha2::{Digest as _, Sha256};
        let baseline = work.join("baseline");
        crate::conversion::files::create_dir(&baseline).unwrap();
        let transport = baseline.join("source.ts");
        // The original MP4 has a finite audio edit and negative priming DTS.
        // A clear TS reference retains all coded frames at its explicitly observed
        // nonzero transport origin. No samples, offsets, or padding are trimmed.
        real_tool(
            "ffmpeg",
            vec![
                "-v".into(),
                "error".into(),
                "-nostdin".into(),
                "-threads".into(),
                "1".into(),
                "-i".into(),
                crate::conversion::files::tool_path(source).unwrap(),
                "-map".into(),
                "0:v:0".into(),
                "-map".into(),
                "0:a:0".into(),
                "-c".into(),
                "copy".into(),
                "-f".into(),
                "mpegts".into(),
                crate::conversion::files::tool_path(&transport).unwrap(),
            ],
        );
        let transport_before = Sha256::digest(std::fs::read(&transport).unwrap()).to_vec();
        for encrypted in [false, true] {
            let dir = work.join(if encrypted {
                "cipher-audio"
            } else {
                "clear-audio"
            });
            crate::conversion::files::create_dir(&dir).unwrap();
            let input = crate::conversion::files::tool_path(&transport).unwrap();
            let template =
                crate::conversion::files::tool_path(&dir.join("seg_$Number$.ts")).unwrap();
            let mut arguments = vec![
                format!(
                    "input={input},stream=audio,segment_template={template},playlist_name=media.m3u8"
                ),
                "--hls_master_playlist_output".into(),
                crate::conversion::files::tool_path(&dir.join("master.m3u8")).unwrap(),
                "--segment_duration".into(),
                "2".into(),
                "--fragment_duration".into(),
                "2".into(),
                "--transport_stream_timestamp_offset_ms".into(),
                "0".into(),
            ];
            if encrypted {
                arguments.extend([
                "--enable_raw_key_encryption".into(), "--keys".into(),
                "label=:key_id=000102030405060708090a0b0c0d0e0f:key=101112131415161718191a1b1c1d1e1f".into(),
                "--iv".into(), "303132333435363738393a3b3c3d3e3f".into(),
                "--protection_scheme".into(), "cbcs".into(), "--clear_lead".into(), "0".into(),
                "--hls_key_uri".into(), "https://operator.invalid/public-fixture".into()]);
            }
            real_tool("packager", arguments);
        }
        (transport, transport_before)
    }
    #[test]
    fn real_shaka_all_470_aac_frames_preserve_coded_bytes_clocks_and_complete_pcm() {
        use sha2::{Digest as _, Sha256};
        use std::io::Write as _;
        let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../drmr/tests/fixtures/bbb/clip.mp4");
        let before = Sha256::digest(std::fs::read(&source).unwrap());
        let work = crate::native::private_tempdir().unwrap();
        let (transport, transport_before) = prepare_real_audio_fixture(work.path(), &source);
        let clear = fixture_role_pes(&work.path().join("clear-audio"), 6, true);
        let cipher = fixture_role_pes(&work.path().join("cipher-audio"), 6, true);
        assert_eq!(clear.len(), 470);
        assert_eq!(cipher.len(), 470);
        let key = Aes128Key::from_hex("101112131415161718191a1b1c1d1e1f").unwrap();
        let iv = SegmentIv::from_hex("303132333435363738393a3b3c3d3e3f").unwrap();
        let mut expected = Zeroizing::new(Vec::new());
        let mut decrypted = Zeroizing::new(Vec::new());
        let mut changed = 0;
        for (index, (plain, encrypted)) in clear.iter().zip(&cipher).enumerate() {
            let a = 9 + usize::from(plain[8]);
            let b = 9 + usize::from(encrypted[8]);
            assert_eq!(
                &plain[..a],
                &encrypted[..b],
                "original AAC PES length/flags/PTS/DTS at {index}"
            );
            changed += usize::from(plain[a..] != encrypted[b..]);
            let actual = decrypt_aac_adts(&encrypted[b..], &key, &iv).unwrap();
            assert_eq!(
                encrypt_aac_adts(&plain[a..], &key, &iv).unwrap().as_slice(),
                &encrypted[b..],
                "independent Shaka AAC encryption at {index}"
            );
            assert_eq!(
                actual.as_slice(),
                &plain[a..],
                "complete original ADTS at {index}"
            );
            expected.extend_from_slice(&plain[a..]);
            decrypted.extend_from_slice(&actual);
        }
        assert_eq!(
            changed, 470,
            "every real audio frame must exercise encryption"
        );
        let mut pcm = Vec::new();
        for (name, data) in [("expected.aac", &expected), ("decrypted.aac", &decrypted)] {
            let path = work.path().join(name);
            crate::native::create_private_new(&path)
                .unwrap()
                .write_all(data)
                .unwrap();
            let result = real_tool(
                "ffmpeg",
                vec![
                    "-v".into(),
                    "error".into(),
                    "-xerror".into(),
                    "-nostdin".into(),
                    "-threads".into(),
                    "1".into(),
                    "-f".into(),
                    "aac".into(),
                    "-i".into(),
                    crate::conversion::files::tool_path(&path).unwrap(),
                    "-map".into(),
                    "0:a:0".into(),
                    "-threads".into(),
                    "1".into(),
                    "-c:a".into(),
                    "pcm_s16le".into(),
                    "-f".into(),
                    "s16le".into(),
                    "-".into(),
                ],
            );
            assert_eq!(result.stdout.len(), 470 * 1024 * 2 * 2);
            pcm.push(result.stdout);
        }
        assert_eq!(pcm[0], pcm[1]);
        assert_eq!(
            Sha256::digest(std::fs::read(&transport).unwrap()).to_vec(),
            transport_before
        );
        assert_eq!(Sha256::digest(std::fs::read(source).unwrap()), before);
    }
}
