//! CENC/CBCS encryption over explicit subsamples; encoded payload is never re-encoded.
use super::{BAD, ConversionError, Scheme, TrackDescriptor, crypto};
use crate::{concurrency::Stop, inspection::TrackKind};
use aes::cipher::{Block, BlockEncrypt as _, KeyInit as _};
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};
use zeroize::Zeroizing;

static PREFIX: OnceLock<Result<[u8; 8], ConversionError>> = OnceLock::new();
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn reserve_sequence(allocator: &AtomicU64) -> Result<u32, ConversionError> {
    let mut current = allocator.load(Ordering::Relaxed);
    loop {
        if current >= u64::from(u32::MAX) {
            return Err(ConversionError("native-output-iv-exhausted"));
        }
        match allocator.compare_exchange_weak(
            current,
            current + 1,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => return u32::try_from(current).map_err(|_| BAD),
            Err(observed) => current = observed,
        }
    }
}

pub(super) fn constant_iv() -> Result<Zeroizing<[u8; 16]>, ConversionError> {
    let mut bytes = Zeroizing::new([0; 16]);
    getrandom::fill(bytes.as_mut()).map_err(|_| ConversionError("secure-random-unavailable"))?;
    Ok(bytes)
}

/// A process-wide namespace covers concurrent resources, tracks and transformer contexts.
/// The low 32 bits are reserved for at least 64 GiB of CENC counter blocks per sample.
pub(super) fn iv() -> Result<Zeroizing<[u8; 16]>, ConversionError> {
    let prefix = PREFIX
        .get_or_init(|| {
            let mut bytes = [0; 8];
            getrandom::fill(&mut bytes)
                .map_err(|_| ConversionError("secure-random-unavailable"))?;
            Ok(bytes)
        })
        .as_ref()
        .map_err(|error| *error)?;
    let sequence = reserve_sequence(&SEQUENCE)?;
    let mut bytes = Zeroizing::new([0; 16]);
    bytes[..8].copy_from_slice(prefix);
    bytes[8..12].copy_from_slice(&sequence.to_be_bytes());
    Ok(bytes)
}

pub(super) struct Encryptor {
    aes: aes::Aes128,
    ctr: Option<crypto::Cipher>,
    iv: Zeroizing<[u8; 16]>,
    previous: Zeroizing<[u8; 16]>,
    crypt: usize,
    skip: usize,
    block: usize,
}
impl Encryptor {
    pub(super) fn new(
        key: &[u8; 16],
        iv: &[u8; 16],
        scheme: Scheme,
        video: bool,
    ) -> Result<Self, ConversionError> {
        if scheme == Scheme::Clear {
            return Err(BAD);
        }
        Ok(Self {
            aes: aes::Aes128::new_from_slice(key).map_err(|_| BAD)?,
            ctr: if scheme == Scheme::Cenc {
                Some(crypto::Cipher::new(key, iv, scheme, 0, 0)?)
            } else {
                None
            },
            iv: Zeroizing::new(*iv),
            previous: Zeroizing::new(*iv),
            crypt: 1,
            skip: if video { 9 } else { 0 },
            block: 0,
        })
    }
    pub(super) fn begin_subsample(&mut self) {
        self.block = 0;
        self.previous.copy_from_slice(self.iv.as_ref());
        if let Some(ctr) = &mut self.ctr {
            ctr.begin_subsample();
        }
    }
    pub(super) fn transform(&mut self, bytes: &mut [u8]) -> Result<(), ConversionError> {
        if let Some(ctr) = &mut self.ctr {
            return ctr.transform(bytes, false);
        }
        if !bytes.len().is_multiple_of(16) {
            return Err(BAD);
        }
        for block in bytes.as_chunks_mut::<16>().0 {
            if self.block % (self.crypt + self.skip) < self.crypt {
                for (byte, previous) in block.iter_mut().zip(self.previous.iter()) {
                    *byte ^= previous;
                }
                self.aes
                    .encrypt_block(Block::<aes::Aes128>::from_mut_slice(block));
                self.previous.copy_from_slice(block);
            }
            self.block = self.block.checked_add(1).ok_or(BAD)?;
        }
        Ok(())
    }
    pub(super) fn block_aligned(&self) -> bool {
        self.ctr.is_none()
    }
}

pub(super) fn subsamples(
    track: &TrackDescriptor,
    bytes: &[u8],
    scheme: Scheme,
    stop: &Stop,
) -> Result<Vec<crypto::Subsample>, ConversionError> {
    if track.kind != TrackKind::Video {
        return Ok(vec![crypto::Subsample {
            clear: 0,
            encrypted: u32::try_from(bytes.len()).map_err(|_| BAD)?,
        }]);
    }
    let (configuration, width) =
        super::super::ts::avc::Configuration::from_avcc(&track.decoder_config)?;
    configuration.length_prefixed_access_unit(bytes, width)?;
    let width = usize::from(width);
    let mut at = 0_usize;
    let mut clear = 0_u64;
    let mut spans = Vec::new();
    while at < bytes.len() {
        read::check_stop(stop)?;
        let begin = at.checked_add(width).ok_or(BAD)?;
        let size = bytes
            .get(at..begin)
            .ok_or(BAD)?
            .iter()
            .fold(0_usize, |value, byte| (value << 8) | usize::from(*byte));
        let end = begin.checked_add(size).ok_or(BAD)?;
        let nal = bytes.get(begin..end).filter(|n| !n.is_empty()).ok_or(BAD)?;
        let header = if matches!(nal[0] & 31, 1 | 5) {
            configuration.slice_header_bytes(nal)?
        } else {
            nal.len()
        };
        let mut protected = nal.len().checked_sub(header).ok_or(BAD)?;
        let tail = if scheme == Scheme::Cenc {
            protected % 16
        } else {
            0
        };
        protected -= tail;
        clear = clear
            .checked_add(u64::try_from(width + header).map_err(|_| BAD)?)
            .ok_or(BAD)?;
        if protected != 0 {
            append_span(
                &mut spans,
                clear,
                u32::try_from(protected).map_err(|_| BAD)?,
            )?;
            clear = tail as u64;
        } else {
            clear = clear.checked_add(tail as u64).ok_or(BAD)?;
        }
        at = end;
    }
    if clear != 0 {
        append_span(&mut spans, clear, 0)?;
    }
    if spans.is_empty() {
        return Err(BAD);
    }
    Ok(spans)
}
use super::read;
fn append_span(
    spans: &mut Vec<crypto::Subsample>,
    mut clear: u64,
    encrypted: u32,
) -> Result<(), ConversionError> {
    while clear > u64::from(u16::MAX) {
        spans.push(crypto::Subsample {
            clear: u32::from(u16::MAX),
            encrypted: 0,
        });
        clear -= u64::from(u16::MAX);
    }
    spans.push(crypto::Subsample {
        clear: u32::try_from(clear).map_err(|_| BAD)?,
        encrypted,
    });
    if spans.len() > usize::from(u16::MAX) {
        return Err(ConversionError("resource-budget"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exhausted_iv_sequence_never_wraps_or_reuses_a_reserved_value() {
        let sequence = AtomicU64::new(u64::from(u32::MAX) - 2);
        let mut values = std::thread::scope(|scope| {
            let tasks = (0..8)
                .map(|_| scope.spawn(|| reserve_sequence(&sequence)))
                .collect::<Vec<_>>();
            tasks
                .into_iter()
                .filter_map(|task| task.join().unwrap().ok())
                .collect::<Vec<_>>()
        });
        values.sort_unstable();
        assert_eq!(values, [u32::MAX - 2, u32::MAX - 1]);
        assert_eq!(sequence.load(Ordering::Relaxed), u64::from(u32::MAX));
        assert_eq!(
            reserve_sequence(&sequence).unwrap_err().0,
            "native-output-iv-exhausted"
        );
        let overflow = AtomicU64::new(u64::MAX);
        assert_eq!(
            reserve_sequence(&overflow).unwrap_err().0,
            "native-output-iv-exhausted"
        );
        assert_eq!(overflow.load(Ordering::Relaxed), u64::MAX);
    }
    #[test]
    fn output_cbc_matches_nist_sp800_38a_f2_and_resets_each_subsample() {
        let key: [u8; 16] = hex::decode("2b7e151628aed2a6abf7158809cf4f3c")
            .unwrap()
            .try_into()
            .unwrap();
        let iv: [u8; 16] = hex::decode("000102030405060708090a0b0c0d0e0f")
            .unwrap()
            .try_into()
            .unwrap();
        let plain = hex::decode(concat!(
            "6bc1bee22e409f96e93d7e117393172a",
            "ae2d8a571e03ac9c9eb76fac45af8e51"
        ))
        .unwrap();
        let mut bytes = plain.clone();
        let mut cipher = Encryptor::new(&key, &iv, Scheme::Cbcs, false).unwrap();
        cipher.transform(&mut bytes).unwrap();
        assert_eq!(
            hex::encode(bytes),
            "7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2"
        );
        cipher.begin_subsample();
        let mut repeated = plain[..16].to_vec();
        cipher.transform(&mut repeated).unwrap();
        assert_eq!(hex::encode(repeated), "7649abac8119b246cee98e9b12e9197d");
    }
    #[test]
    fn concurrent_iv_allocations_keep_sample_and_counter_namespaces_disjoint() {
        let mut values = std::thread::scope(|scope| {
            let tasks = (0..8)
                .map(|_| scope.spawn(|| (0..128).map(|_| *iv().unwrap()).collect::<Vec<_>>()))
                .collect::<Vec<_>>();
            tasks
                .into_iter()
                .flat_map(|task| task.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert!(values.iter().all(|value| value[12..] == [0; 4]));
        values.sort_unstable();
        values.dedup();
        assert_eq!(values.len(), 1024);
    }
}
