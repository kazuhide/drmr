//! CENC counter and CBCS pattern processing over exact sample subsample ranges.
use super::{BAD, ConversionError, Scheme, UNSUPPORTED};
use aes::cipher::{BlockCipherDecrypt as _, BlockCipherEncrypt as _, KeyInit as _};
use zeroize::Zeroizing;

const BATCH_BLOCKS: usize = 32;

pub(super) struct TrackEncryption {
    pub scheme: Scheme,
    pub kid: [u8; 16],
    pub iv_size: u8,
    pub constant_iv: Option<Zeroizing<Vec<u8>>>,
    pub crypt: u8,
    pub skip: u8,
}
pub(super) struct Subsample {
    pub clear: u32,
    pub encrypted: u32,
}
pub(super) struct SampleEncryption {
    pub iv: Zeroizing<Vec<u8>>,
    pub subsamples: Vec<Subsample>,
}
pub(super) struct Cipher {
    aes: aes::Aes128,
    scheme: Scheme,
    counter: Zeroizing<[u8; 16]>,
    previous: Zeroizing<[u8; 16]>,
    stream: Zeroizing<[u8; 16]>,
    stream_at: usize,
    generated: bool,
    counter_start: usize,
    crypt: usize,
    skip: usize,
    block: usize,
}
impl Cipher {
    pub fn new(
        key: &[u8; 16],
        iv: &[u8],
        scheme: Scheme,
        crypt: u8,
        skip: u8,
    ) -> Result<Self, ConversionError> {
        if !matches!(iv.len(), 8 | 16)
            || scheme == Scheme::Clear
            || (scheme == Scheme::Cbcs && iv.len() != 16)
            || (scheme == Scheme::Cenc && (crypt != 0 || skip != 0))
            || (crypt == 0 && skip != 0)
        {
            return Err(UNSUPPORTED);
        }
        let mut counter = Zeroizing::new([0; 16]);
        counter[..iv.len()].copy_from_slice(iv);
        let previous = Zeroizing::new(*counter);
        Ok(Self {
            aes: aes::Aes128::new(key.into()),
            scheme,
            counter,
            previous,
            stream: Zeroizing::new([0; 16]),
            stream_at: 16,
            generated: false,
            counter_start: if iv.len() == 8 { 8 } else { 0 },
            crypt: usize::from(if crypt == 0 { 1 } else { crypt }),
            skip: usize::from(skip),
            block: 0,
        })
    }
    pub fn begin_subsample(&mut self) {
        self.block = 0;
        // CBCS restarts the IV and pattern for each subsample, while CENC CTR
        // retains its partial keystream across clear/encrypted subsample ranges.
        if self.scheme == Scheme::Cbcs {
            self.previous.copy_from_slice(self.counter.as_ref());
        }
    }
    fn next_counter(&mut self) -> Result<(), ConversionError> {
        if self.generated {
            for position in (self.counter_start..16).rev() {
                let (value, overflow) = self.counter[position].overflowing_add(1);
                self.counter[position] = value;
                if !overflow {
                    return Ok(());
                }
            }
            return Err(ConversionError("native-cipher-counter-overflow"));
        }
        self.generated = true;
        Ok(())
    }
    /// Consume only complete blocks, keeping the partial stream in the caller.
    fn transform_ctr_blocks(&mut self, bytes: &mut [u8]) -> Result<usize, ConversionError> {
        debug_assert_eq!(self.stream_at, 16);
        if bytes.len() < 16 {
            return Ok(0);
        }
        let mut streams = Zeroizing::new([aes::Block::default(); BATCH_BLOCKS]);
        let mut processed = 0;
        while bytes.len() - processed >= 16 {
            let count = ((bytes.len() - processed) / 16).min(BATCH_BLOCKS);
            let mut generated = 0;
            let mut failure = None;
            for stream in &mut streams[..count] {
                if let Err(error) = self.next_counter() {
                    failure = Some(error);
                    break;
                }
                stream.copy_from_slice(self.counter.as_ref());
                generated += 1;
            }
            if generated != 0 {
                self.aes.encrypt_blocks(&mut streams[..generated]);
                for (block, stream) in bytes[processed..processed + generated * 16]
                    .chunks_exact_mut(16)
                    .zip(&streams[..generated])
                {
                    for (byte, mask) in block.iter_mut().zip(stream) {
                        *byte ^= mask;
                    }
                }
                self.stream.copy_from_slice(&streams[generated - 1]);
                processed += generated * 16;
            }
            if let Some(error) = failure {
                return Err(error);
            }
        }
        Ok(processed)
    }
    fn transform_ctr(&mut self, bytes: &mut [u8]) -> Result<(), ConversionError> {
        let head = bytes.len().min(16 - self.stream_at);
        for (byte, mask) in bytes[..head].iter_mut().zip(&self.stream[self.stream_at..]) {
            *byte ^= mask;
        }
        self.stream_at += head;
        if head == bytes.len() {
            return Ok(());
        }
        let complete = self.transform_ctr_blocks(&mut bytes[head..])?;
        let tail = &mut bytes[head + complete..];
        if !tail.is_empty() {
            self.next_counter()?;
            self.stream.copy_from_slice(self.counter.as_ref());
            self.aes.encrypt_block((&mut *self.stream).into());
            for (byte, mask) in tail.iter_mut().zip(self.stream.iter()) {
                *byte ^= mask;
            }
            self.stream_at = tail.len();
        }
        Ok(())
    }
    pub fn transform(
        &mut self,
        bytes: &mut [u8],
        final_chunk: bool,
    ) -> Result<(), ConversionError> {
        match self.scheme {
            Scheme::Cenc => self.transform_ctr(bytes)?,
            Scheme::Cbcs => {
                if !final_chunk && !bytes.len().is_multiple_of(16) {
                    return Err(BAD);
                }
                for block in bytes.as_chunks_mut::<16>().0 {
                    if self.block % (self.crypt + self.skip) < self.crypt {
                        let mut original = Zeroizing::new([0; 16]);
                        original.copy_from_slice(block);
                        self.aes.decrypt_block((&mut *block).into());
                        for (value, previous) in block.iter_mut().zip(self.previous.iter()) {
                            *value ^= previous;
                        }
                        self.previous.copy_from_slice(original.as_ref());
                    }
                    self.block = self.block.checked_add(1).ok_or(BAD)?;
                }
                // The final incomplete cipher block is clear and never changes CBC state.
            }
            Scheme::Clear => return Err(BAD),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aes_known_answer_and_context_wiping_are_retained() {
        fn require_wiping<T: zeroize::ZeroizeOnDrop>() {}
        require_wiping::<aes::Aes128>();
        require_wiping::<cbc::Encryptor<aes::Aes128>>();
        require_wiping::<cbc::Decryptor<aes::Aes128>>();

        let key: [u8; 16] = bytes("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        let plain: [u8; 16] = bytes("00112233445566778899aabbccddeeff")
            .try_into()
            .unwrap();
        let mut block = Zeroizing::new(aes::Block::from(plain));
        let cipher = aes::Aes128::new((&key).into());
        cipher.encrypt_block(&mut block);
        assert_eq!(
            hex::encode(block.as_slice()),
            "69c4e0d86a7b0430d8cdb78070b4c55a"
        );
        cipher.decrypt_block(&mut block);
        assert_eq!(block.as_slice(), plain);
        eprintln!("AES hardware acceleration: {}", aes::hardware_accelerated());
    }

    fn bytes(text: &str) -> Vec<u8> {
        hex::decode(text).unwrap()
    }
    fn key() -> [u8; 16] {
        bytes("2b7e151628aed2a6abf7158809cf4f3c")
            .try_into()
            .unwrap()
    }
    fn plaintext() -> Vec<u8> {
        bytes(concat!(
            "6bc1bee22e409f96e93d7e117393172a",
            "ae2d8a571e03ac9c9eb76fac45af8e51",
            "30c81c46a35ce411e5fbc1191a0a52ef",
            "f69f2445df4f9b17ad2b417be66c3710"
        ))
    }
    #[test]
    fn contiguous_ctr_kernel_keeps_the_nist_tail_for_the_streaming_path() {
        let mut encrypted = bytes(concat!(
            "874d6191b620e3261bef6864990db6ce",
            "9806f66b7970fdff8617187bb9fffdff",
            "5ae4df3edbd5d35e5b4f09020db03eab",
            "1e031dda2fbe03d1792170a0f3009cee"
        ));
        let iv = bytes("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff");
        let mut cipher = Cipher::new(&key(), &iv, Scheme::Cenc, 0, 0).unwrap();
        let untouched = encrypted[48];
        assert_eq!(
            cipher.transform_ctr_blocks(&mut encrypted[..49]).unwrap(),
            48
        );
        assert_eq!(&encrypted[..48], &plaintext()[..48]);
        assert_eq!(encrypted[48], untouched);
        cipher.begin_subsample();
        cipher.transform(&mut encrypted[48..], true).unwrap();
        assert_eq!(encrypted, plaintext());
    }
    struct ScalarCtr {
        aes: aes::Aes128,
        counter: u128,
        narrow: bool,
        generated: bool,
        stream: aes::Block,
        at: usize,
    }
    impl ScalarCtr {
        fn new(counter: [u8; 16], narrow: bool) -> Self {
            Self {
                aes: aes::Aes128::new((&key()).into()),
                counter: u128::from_be_bytes(counter),
                narrow,
                generated: false,
                stream: aes::Block::default(),
                at: 16,
            }
        }
        fn apply(&mut self, bytes: &mut [u8]) -> Result<(), ConversionError> {
            for byte in bytes {
                if self.at == 16 {
                    if self.generated {
                        let next = self
                            .counter
                            .checked_add(1)
                            .filter(|next| !self.narrow || next >> 64 == self.counter >> 64);
                        self.counter =
                            next.ok_or(ConversionError("native-cipher-counter-overflow"))?;
                    }
                    self.stream = self.counter.to_be_bytes().into();
                    self.aes.encrypt_block(&mut self.stream);
                    self.generated = true;
                    self.at = 0;
                }
                *byte ^= self.stream[self.at];
                self.at += 1;
            }
            Ok(())
        }
    }
    #[test]
    fn ctr_batches_equal_a_scalar_oracle_across_chunks_and_clear_subsample_gaps() {
        for width in [8, 16] {
            let iv = bytes("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff");
            let mut counter = [0; 16];
            counter[..width].copy_from_slice(&iv[..width]);
            for chunk in [1, 7, 15, 16, 17, 63, 511, 512, 513, 65_536] {
                let original = (0..65_569)
                    .map(|i| u8::try_from((i * 73 + 19) & 255).unwrap())
                    .collect::<Vec<_>>();
                let mut actual = original.clone();
                let mut expected = original.clone();
                let mut cipher = Cipher::new(&key(), &iv[..width], Scheme::Cenc, 0, 0).unwrap();
                let mut scalar = ScalarCtr::new(counter, width == 8);
                let mut at = 0;
                for (clear, protected) in [(3, 17), (5, 1), (7, 1537), (0, 64_999)] {
                    at += clear;
                    let end = (at + protected).min(actual.len());
                    cipher.begin_subsample();
                    for part in actual[at..end].chunks_mut(chunk) {
                        cipher.transform(part, true).unwrap();
                    }
                    scalar.apply(&mut expected[at..end]).unwrap();
                    at = end;
                }
                assert_eq!(actual, expected, "IV bytes={width}, chunk={chunk}");
                assert_eq!(&actual[..3], &original[..3]);
            }
        }
    }
    #[test]
    fn ctr_batch_overflow_keeps_the_scalar_prefix_and_never_carries_into_an_iv8_prefix() {
        for narrow in [false, true] {
            for head in [0, 1, 15, 16, 17] {
                let initial = if narrow {
                    (0x1234_u128 << 64) | u128::from(u64::MAX - 2)
                } else {
                    u128::MAX - 2
                };
                let counter = initial.to_be_bytes();
                let mut cipher = Cipher::new(
                    &key(),
                    &counter[..if narrow { 8 } else { 16 }],
                    Scheme::Cenc,
                    0,
                    0,
                )
                .unwrap();
                cipher.counter.copy_from_slice(&counter);
                let mut scalar = ScalarCtr::new(counter, narrow);
                let mut actual = [0x5a; 65];
                let mut expected = actual;
                cipher.transform(&mut actual[..head], true).unwrap();
                scalar.apply(&mut expected[..head]).unwrap();
                assert_eq!(
                    cipher.transform(&mut actual[head..], true),
                    scalar.apply(&mut expected[head..]),
                );
                assert_eq!(actual, expected, "IV8={narrow}, head={head}");
                assert_eq!(&actual[48..], &[0x5a; 17]);
            }
        }
    }
    #[test]
    fn nist_sp800_38a_f5_ctr_retains_partial_blocks_across_chunks_and_clear_subsamples() {
        let mut encrypted = bytes(concat!(
            "874d6191b620e3261bef6864990db6ce",
            "9806f66b7970fdff8617187bb9fffdff",
            "5ae4df3edbd5d35e5b4f09020db03eab",
            "1e031dda2fbe03d1792170a0f3009cee"
        ));
        let iv = bytes("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff");
        let mut cipher = Cipher::new(&key(), &iv, Scheme::Cenc, 0, 0).unwrap();
        for chunk in encrypted.chunks_mut(7) {
            cipher.begin_subsample();
            cipher.transform(chunk, true).unwrap();
        }
        assert_eq!(encrypted, plaintext());
    }
    #[test]
    fn nist_sp800_38a_f2_cbc_and_cbcs_partial_tail_keep_exact_plaintext() {
        let mut encrypted = bytes(concat!(
            "7649abac8119b246cee98e9b12e9197d",
            "5086cb9b507219ee95db113a917678b2",
            "73bed6b8e3c1743b7116e69e22229516",
            "3ff1caa1681fac09120eca307586e1a7"
        ));
        let iv = bytes("000102030405060708090a0b0c0d0e0f");
        encrypted.extend_from_slice(b"clear tail");
        let mut cipher = Cipher::new(&key(), &iv, Scheme::Cbcs, 0, 0).unwrap();
        cipher.transform(&mut encrypted[..32], false).unwrap();
        cipher.transform(&mut encrypted[32..], true).unwrap();
        let mut expected = plaintext();
        expected.extend_from_slice(b"clear tail");
        assert_eq!(encrypted, expected);
    }
    #[test]
    fn cbcs_pattern_skips_blocks_without_changing_cbc_chain_inside_one_subsample() {
        let first = bytes("7649abac8119b246cee98e9b12e9197d");
        let second = bytes("5086cb9b507219ee95db113a917678b2");
        let skipped = vec![0x5a; 9 * 16];
        let mut encrypted = [first, skipped.clone(), second].concat();
        let mut cipher = Cipher::new(
            &key(),
            &bytes("000102030405060708090a0b0c0d0e0f"),
            Scheme::Cbcs,
            1,
            9,
        )
        .unwrap();
        cipher.transform(&mut encrypted[..80], false).unwrap();
        cipher.transform(&mut encrypted[80..], true).unwrap();
        assert_eq!(&encrypted[..16], &plaintext()[..16]);
        assert_eq!(&encrypted[16..160], skipped);
        assert_eq!(&encrypted[160..], &plaintext()[16..32]);
        cipher.begin_subsample();
        let mut repeated = bytes("7649abac8119b246cee98e9b12e9197d");
        cipher.transform(&mut repeated, true).unwrap();
        assert_eq!(&repeated, &plaintext()[..16]);
    }
    #[test]
    fn counter_exhaustion_and_partial_nonterminal_cbcs_blocks_refuse() {
        let mut ctr = Cipher::new(&key(), &[255; 16], Scheme::Cenc, 0, 0).unwrap();
        ctr.transform(&mut [0; 16], false).unwrap();
        assert!(ctr.transform(&mut [0; 1], true).is_err());
        let mut cbc = Cipher::new(&key(), &[0; 16], Scheme::Cbcs, 1, 9).unwrap();
        assert!(cbc.transform(&mut [0; 15], false).is_err());
    }
}
