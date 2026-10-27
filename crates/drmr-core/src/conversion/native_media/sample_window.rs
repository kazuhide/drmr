//! Reuse an already bounded sample window only when decryption cannot fail late.
use super::{BAD, CHUNK_BYTES, ConversionError, Payload, Scheme, crypto, read};
use crate::concurrency::Stop;
use zeroize::Zeroizing;

pub(super) fn eligible(sample: &Payload<'_>) -> bool {
    sample
        .track
        .encryption
        .as_ref()
        .zip(sample.encryption.as_ref())
        .is_some_and(|(track, encryption)| bounded(sample.info.size, track.scheme, encryption))
}

fn bounded(size: u32, scheme: Scheme, encryption: &crypto::SampleEncryption) -> bool {
    if !(1..=CHUNK_BYTES).contains(&(size as usize)) {
        return false;
    }
    let Some((covered, encrypted)) =
        encryption
            .subsamples
            .iter()
            .try_fold((0_u64, 0_u64), |(covered, encrypted), span| {
                Some((
                    covered.checked_add(u64::from(span.clear) + u64::from(span.encrypted))?,
                    encrypted.checked_add(u64::from(span.encrypted))?,
                ))
            })
    else {
        return false;
    };
    if covered != u64::from(size) {
        return false;
    }
    if scheme == Scheme::Cenc && encryption.iv.len() == 16 {
        let Ok(iv) = <&[u8; 16]>::try_from(encryption.iv.as_slice()) else {
            return false;
        };
        // Preserve clear-prefix callback precedence when a later CTR block would overflow.
        // The first block consumes the IV itself; clear spans consume no keystream.
        let increments = encrypted.div_ceil(16).saturating_sub(1);
        let counter = Zeroizing::new(u128::from_be_bytes(*iv));
        return counter.checked_add(u128::from(increments)).is_some();
    }
    matches!(
        (scheme, encryption.iv.len()),
        (Scheme::Cenc, 8) | (Scheme::Cbcs, 16)
    )
}

pub(super) fn decrypt(
    bytes: &mut [u8],
    subsamples: &[crypto::Subsample],
    cipher: &mut crypto::Cipher,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let mut at = 0_usize;
    for span in subsamples {
        cipher.begin_subsample();
        for (encrypted, size) in [(false, span.clear), (true, span.encrypted)] {
            let end = at
                .checked_add(usize::try_from(size).map_err(|_| BAD)?)
                .ok_or(BAD)?;
            let range = bytes.get_mut(at..end).ok_or(BAD)?;
            if !range.is_empty() {
                read::check_stop(stop)?;
                if encrypted {
                    cipher.transform(range, true)?;
                }
            }
            at = end;
        }
    }
    if at != bytes.len() {
        return Err(BAD);
    }
    read::check_stop(stop)
}
