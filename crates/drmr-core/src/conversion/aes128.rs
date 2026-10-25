//! AES-128 segment encryption with explicit keys and bounded streaming.
use super::ConversionError;
use std::{fmt, path::Path};
use zeroize::Zeroizing;
/// One operator-supplied segment key, independent of MP4 key identifiers.
pub struct Aes128Key(pub(super) Zeroizing<[u8; 16]>);
impl fmt::Debug for Aes128Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Aes128Key([redacted])")
    }
}
impl Aes128Key {
    pub fn from_hex(value: &str) -> Result<Self, ConversionError> {
        let mut bytes = Zeroizing::new([0; 16]);
        if value.len() != 32 || hex::decode_to_slice(value, bytes.as_mut()).is_err() {
            return Err(ConversionError("invalid-aes128-key"));
        }
        Ok(Self(bytes))
    }
}
/// An explicit initialization vector, or the big-endian HLS media sequence number.
pub struct SegmentIv(pub(super) Zeroizing<[u8; 16]>);
impl fmt::Debug for SegmentIv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SegmentIv([redacted])")
    }
}
impl SegmentIv {
    pub fn from_hex(value: &str) -> Result<Self, ConversionError> {
        let value = value.strip_prefix("0x").unwrap_or(value);
        if value.is_empty()
            || value.len() > 32
            || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ConversionError("invalid-segment-iv"));
        }
        let padded = Zeroizing::new(format!("{value:0>32}"));
        let mut bytes = Zeroizing::new([0; 16]);
        hex::decode_to_slice(&*padded, bytes.as_mut())
            .map_err(|_| ConversionError("invalid-segment-iv"))?;
        Ok(Self(bytes))
    }
    pub fn for_sequence(sequence: u64) -> Self {
        let mut bytes = Zeroizing::new([0; 16]);
        bytes[8..].copy_from_slice(&sequence.to_be_bytes());
        Self(bytes)
    }
    pub fn random() -> Result<Self, ConversionError> {
        let mut bytes = Zeroizing::new([0; 16]);
        getrandom::fill(bytes.as_mut())
            .map_err(|_| ConversionError("secure-random-unavailable"))?;
        Ok(Self(bytes))
    }
}
#[derive(Default)]
/// Exact playlist-key-URI mappings. Referenced URIs are never fetched.
pub struct HlsInputKeys(pub(super) Vec<HlsInputKey>);
pub(super) struct HlsInputKey {
    pub uri: Zeroizing<String>,
    pub key: Aes128Key,
    pub iv: Option<SegmentIv>,
}
impl fmt::Debug for HlsInputKeys {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HlsInputKeys")
            .field("count", &self.0.len())
            .finish()
    }
}
impl HlsInputKeys {
    pub fn new(keys: Vec<(String, Aes128Key)>) -> Result<Self, ConversionError> {
        Self::new_with_ivs(
            keys.into_iter()
                .map(|(uri, key)| (uri, key, None))
                .collect(),
        )
    }
    /// Match a sample-encryption key and its optional associated IV by exact URI.
    /// SAMPLE-AES never derives a missing IV from the media sequence number.
    pub fn new_with_ivs(
        keys: Vec<(String, Aes128Key, Option<SegmentIv>)>,
    ) -> Result<Self, ConversionError> {
        if keys.len() > 64 {
            return Err(ConversionError("key-count-limit"));
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut values = Vec::new();
        for (uri, key, iv) in keys {
            if uri.is_empty()
                || uri.len() > 4096
                || uri.chars().any(char::is_control)
                || !seen.insert(uri.clone())
            {
                return Err(ConversionError("invalid-or-duplicate-key-uri"));
            }
            values.push(HlsInputKey {
                uri: Zeroizing::new(uri),
                key,
                iv,
            });
        }
        Ok(Self(values))
    }
}
/// A segment key and the URI written into output playlists; no key file is published.
pub struct HlsOutputKey {
    pub(super) uri: Zeroizing<String>,
    pub(super) key: Aes128Key,
    pub(super) iv: Option<SegmentIv>,
}
impl fmt::Debug for HlsOutputKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HlsOutputKey([redacted])")
    }
}
impl HlsOutputKey {
    /// The operator-provided URI for an output playlist. Never include it in diagnostics.
    pub fn key_uri(&self) -> &str {
        &self.uri
    }
    /// The associated SAMPLE-AES IV. Never include it in diagnostics.
    pub fn associated_iv(&self) -> Option<&SegmentIv> {
        self.iv.as_ref()
    }
    pub fn encrypt(
        &self,
        source: &Path,
        destination: &Path,
        iv: &SegmentIv,
    ) -> Result<(), ConversionError> {
        encrypt_segment(source, destination, &self.key, iv)
    }
    pub fn new(uri: impl Into<String>, key: Aes128Key) -> Result<Self, ConversionError> {
        Self::with_optional_iv(uri.into(), key, None)
    }
    /// Supply an explicit IV for SAMPLE-AES output, independently of MP4 key identifiers.
    pub fn new_with_iv(
        uri: impl Into<String>,
        key: Aes128Key,
        iv: SegmentIv,
    ) -> Result<Self, ConversionError> {
        Self::with_optional_iv(uri.into(), key, Some(iv))
    }
    fn with_optional_iv(
        uri: String,
        key: Aes128Key,
        iv: Option<SegmentIv>,
    ) -> Result<Self, ConversionError> {
        if uri.is_empty()
            || uri.len() > 4096
            || uri.contains('"')
            || uri.chars().any(char::is_control)
        {
            return Err(ConversionError("invalid-output-key-uri"));
        }
        Ok(Self {
            uri: Zeroizing::new(uri),
            key,
            iv,
        })
    }
}

use aes::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyIvInit, block_padding::Pkcs7};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Write},
};
const IO_BUFFER: usize = 64 * 1024;

/// Encrypt one regular local resource and publish it without overwriting existing files.
pub fn encrypt_segment(
    source: &Path,
    destination: &Path,
    key: &Aes128Key,
    iv: &SegmentIv,
) -> Result<(), ConversionError> {
    file_transform(source, destination, key, iv, true, None)
}
/// Decrypt one resource; invalid length or PKCS#7 padding publishes no plaintext.
pub fn decrypt_segment(
    source: &Path,
    destination: &Path,
    key: &Aes128Key,
    iv: &SegmentIv,
) -> Result<(), ConversionError> {
    file_transform(source, destination, key, iv, false, None)
}
pub(super) fn encrypt_segment_with_stop(
    source: &Path,
    destination: &Path,
    key: &Aes128Key,
    iv: &SegmentIv,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    file_transform(source, destination, key, iv, true, Some(stop))
}
pub(super) fn decrypt_segment_with_stop(
    source: &Path,
    destination: &Path,
    key: &Aes128Key,
    iv: &SegmentIv,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    file_transform(source, destination, key, iv, false, Some(stop))
}

fn cancelled(stop: Option<&crate::concurrency::Stop>) -> Result<(), ConversionError> {
    if stop.is_some_and(crate::concurrency::Stop::is_set) {
        return Err(ConversionError("cancelled"));
    }
    Ok(())
}

struct StoppableReader<'a, R> {
    input: R,
    stop: Option<&'a crate::concurrency::Stop>,
    remaining: usize,
}
impl<R: Read> Read for StoppableReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            cancelled(self.stop).map_err(std::io::Error::other)?;
            self.remaining = IO_BUFFER;
        }
        let length = bytes.len().min(self.remaining);
        let count = self.input.read(&mut bytes[..length])?;
        self.remaining -= count;
        Ok(count)
    }
}
fn file_transform(
    source: &Path,
    destination: &Path,
    key: &Aes128Key,
    iv: &SegmentIv,
    encrypt: bool,
    stop: Option<&crate::concurrency::Stop>,
) -> Result<(), ConversionError> {
    cancelled(stop)?;
    let destination = super::destination(destination)?;
    let workspace = super::workspace()?;
    let snapshot = workspace.path().join("source");
    if stop.is_some() {
        let source = super::files::regular_input(source)?;
        super::validate_held_input(&source)?;
        let mut reader = StoppableReader {
            input: source.take(super::MAX_MEDIA_BYTES + 1),
            stop,
            remaining: 0,
        };
        let mut output = super::files::create_new(&snapshot)?;
        let copied = std::io::copy(&mut reader, &mut output);
        cancelled(stop)?;
        if copied.map_err(|_| ConversionError("media-copy-failed"))? > super::MAX_MEDIA_BYTES {
            return Err(ConversionError("media-size-limit"));
        }
    } else {
        super::copy_private(source, &snapshot)?;
    }
    let candidate = workspace.path().join("candidate");
    let file = super::files::create_new(&candidate)?;
    let mut output = BufWriter::with_capacity(IO_BUFFER, file);
    let mut input = BufReader::with_capacity(
        IO_BUFFER,
        File::open(snapshot).map_err(|_| ConversionError("segment-read-failed"))?,
    );
    let mut input = StoppableReader {
        input: &mut input,
        stop,
        remaining: 0,
    };
    let transformed = if encrypt {
        encrypt_stream(&mut input, &mut output, key, iv)
    } else {
        decrypt_stream(&mut input, &mut output, key, iv)
    };
    cancelled(stop)?;
    transformed?;
    output
        .flush()
        .map_err(|_| ConversionError("segment-write-failed"))?;
    output
        .get_ref()
        .sync_all()
        .map_err(|_| ConversionError("segment-write-failed"))?;
    drop(output);
    cancelled(stop)?;
    super::publish(&candidate, &destination)
}
fn read_block(input: &mut impl Read, block: &mut [u8; 16]) -> Result<usize, ConversionError> {
    let mut size = 0;
    while size < 16 {
        match input.read(&mut block[size..]) {
            Ok(0) => break,
            Ok(count) => size += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(ConversionError("segment-read-failed")),
        }
    }
    Ok(size)
}
fn encrypt_stream(
    input: &mut impl Read,
    output: &mut impl Write,
    key: &Aes128Key,
    iv: &SegmentIv,
) -> Result<(), ConversionError> {
    let mut cipher = cbc::Encryptor::<aes::Aes128>::new((&*key.0).into(), (&*iv.0).into());
    let mut buffer = Zeroizing::new([0u8; 16]);
    loop {
        let size = read_block(input, &mut buffer)?;
        if size == 16 {
            cipher.encrypt_block((&mut *buffer).into());
            output
                .write_all(buffer.as_ref())
                .map_err(|_| ConversionError("segment-write-failed"))?;
        } else {
            let bytes = cipher
                .encrypt_padded::<Pkcs7>(buffer.as_mut(), size)
                .map_err(|_| ConversionError("segment-encryption-failed"))?;
            output
                .write_all(bytes)
                .map_err(|_| ConversionError("segment-write-failed"))?;
            return Ok(());
        }
    }
}
fn decrypt_stream(
    input: &mut impl Read,
    output: &mut impl Write,
    key: &Aes128Key,
    iv: &SegmentIv,
) -> Result<(), ConversionError> {
    let mut cipher = cbc::Decryptor::<aes::Aes128>::new((&*key.0).into(), (&*iv.0).into());
    // Hold one ciphertext block back until EOF establishes its padding boundary.
    let mut buffer = Zeroizing::new(vec![0u8; IO_BUFFER + 16]);
    let mut used = 0;
    loop {
        while used < buffer.len() {
            match input.read(&mut buffer[used..]) {
                Ok(0) => break,
                Ok(count) => used += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Err(ConversionError("segment-read-failed")),
            }
        }
        if used < buffer.len() {
            if used == 0 || !used.is_multiple_of(16) {
                return Err(ConversionError("invalid-encrypted-segment"));
            }
            let plaintext = cipher
                .decrypt_padded::<Pkcs7>(&mut buffer[..used])
                .map_err(|_| ConversionError("invalid-encrypted-segment"))?;
            output
                .write_all(plaintext)
                .map_err(|_| ConversionError("segment-write-failed"))?;
            return Ok(());
        }
        let (blocks, _) = aes::Block::slice_as_chunks_mut(&mut buffer[..IO_BUFFER]);
        cipher.decrypt_blocks(blocks);
        output
            .write_all(&buffer[..IO_BUFFER])
            .map_err(|_| ConversionError("segment-write-failed"))?;
        buffer.copy_within(IO_BUFFER.., 0);
        used = 16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct ShortReader<'a>(&'a [u8]);
    impl Read for ShortReader<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let count = out.len().min(self.0.len()).min(3);
            out[..count].copy_from_slice(&self.0[..count]);
            self.0 = &self.0[count..];
            Ok(count)
        }
    }
    struct FailingWriter;
    #[test]
    fn cbc_decryption_batches_output_while_retaining_the_final_padding_block() {
        struct CountWrites {
            bytes: Vec<u8>,
            calls: usize,
        }
        impl Write for CountWrites {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.calls += 1;
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let key = Aes128Key::from_hex("2b7e151628aed2a6abf7158809cf4f3c").unwrap();
        let iv = SegmentIv::from_hex("000102030405060708090a0b0c0d0e0f").unwrap();
        for length in [
            0,
            1,
            15,
            16,
            17,
            IO_BUFFER - 1,
            IO_BUFFER,
            IO_BUFFER * 2 + 31,
        ] {
            let original = (0..length)
                .map(|i| u8::try_from(i % 251).unwrap())
                .collect::<Vec<_>>();
            // The independent mode implementation supplies a padded CBC oracle.
            let mut encrypted = original.clone();
            encrypted.resize((length / 16 + 1) * 16, 0);
            let cipher = cbc::Encryptor::<aes::Aes128>::new((&*key.0).into(), (&*iv.0).into());
            let encrypted = cipher
                .encrypt_padded::<Pkcs7>(&mut encrypted, length)
                .unwrap();
            let mut output = CountWrites {
                bytes: Vec::new(),
                calls: 0,
            };
            decrypt_stream(&mut ShortReader(encrypted), &mut output, &key, &iv).unwrap();
            assert_eq!(output.bytes, original, "length={length}");
            assert!(
                output.calls <= encrypted.len() / IO_BUFFER + 1,
                "length={length}, writes={}",
                output.calls
            );
        }
    }
    #[test]
    fn cancelled_resource_work_preserves_an_existing_destination() {
        let root = super::super::workspace().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("foreign");
        std::fs::write(&source, b"source bytes").unwrap();
        std::fs::write(&destination, b"foreign bytes").unwrap();
        let key = Aes128Key::from_hex("42424242424242424242424242424242").unwrap();
        let iv = SegmentIv::for_sequence(1);
        let stop = crate::concurrency::Stop::new();
        stop.set();
        assert_eq!(
            encrypt_segment_with_stop(&source, &destination, &key, &iv, &stop)
                .unwrap_err()
                .0,
            "cancelled"
        );
        assert_eq!(
            decrypt_segment_with_stop(&source, &destination, &key, &iv, &stop)
                .unwrap_err()
                .0,
            "cancelled"
        );
        assert_eq!(std::fs::read(&destination).unwrap(), b"foreign bytes");
        assert_eq!(std::fs::read(&source).unwrap(), b"source bytes");
    }
    #[test]
    fn cancellation_is_checked_within_one_io_chunk() {
        struct Trip<'a> {
            bytes: std::io::Cursor<Vec<u8>>,
            stop: &'a crate::concurrency::Stop,
        }
        impl Read for Trip<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let count = self.bytes.read(bytes)?;
                if self.bytes.position() >= IO_BUFFER as u64 {
                    self.stop.set();
                }
                Ok(count)
            }
        }
        let stop = crate::concurrency::Stop::new();
        let input = Trip {
            bytes: std::io::Cursor::new(vec![1; IO_BUFFER * 2]),
            stop: &stop,
        };
        let mut checked = StoppableReader {
            input,
            stop: Some(&stop),
            remaining: 0,
        };
        let mut output = Vec::new();
        assert!(std::io::copy(&mut checked, &mut output).is_err());
        assert_eq!(output.len(), IO_BUFFER);
        assert!(stop.is_set());
    }
    impl Write for FailingWriter {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("synthetic failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[test]
    fn short_reads_do_not_reset_chaining_and_write_errors_propagate() {
        let key = Aes128Key::from_hex("42424242424242424242424242424242").unwrap();
        let iv = SegmentIv::for_sequence(1);
        let source = b"a message that crosses more than two blocks";
        let mut encrypted = Vec::new();
        encrypt_stream(&mut ShortReader(source), &mut encrypted, &key, &iv).unwrap();
        let mut decrypted = Vec::new();
        decrypt_stream(&mut ShortReader(&encrypted), &mut decrypted, &key, &iv).unwrap();
        assert_eq!(decrypted, source);
        assert_eq!(
            encrypt_stream(&mut ShortReader(source), &mut FailingWriter, &key, &iv),
            Err(ConversionError("segment-write-failed"))
        );
        assert_eq!(
            decrypt_stream(&mut ShortReader(&encrypted), &mut FailingWriter, &key, &iv),
            Err(ConversionError("segment-write-failed"))
        );
    }
    #[test]
    fn cbc_batches_refuse_truncation_and_bad_padding_without_publishing_plaintext() {
        let key = Aes128Key::from_hex("42424242424242424242424242424242").unwrap();
        let iv = SegmentIv::for_sequence(1);
        let root = super::super::workspace().unwrap();
        let source = root.path().join("ciphertext");
        let destination = root.path().join("plaintext");
        let mut encrypted = Vec::new();
        encrypt_stream(
            &mut &vec![0x55; IO_BUFFER + 19][..],
            &mut encrypted,
            &key,
            &iv,
        )
        .unwrap();
        let original = encrypted.clone();
        // Change the previous CBC block so the final padding byte becomes zero.
        let end = encrypted.len();
        encrypted[end - 17] ^= 0x0d;
        for bytes in [
            &[][..],
            &original[..15],
            &original[..end - 1],
            encrypted.as_slice(),
        ] {
            std::fs::write(&source, bytes).unwrap();
            assert_eq!(
                decrypt_segment(&source, &destination, &key, &iv),
                Err(ConversionError("invalid-encrypted-segment"))
            );
            assert!(!destination.exists());
            assert_eq!(std::fs::read(&source).unwrap(), bytes);
        }
    }
    #[test]
    fn cbc_decryption_observes_cancellation_before_reading_beyond_one_io_chunk() {
        struct Trip<'a> {
            stop: &'a crate::concurrency::Stop,
            read: usize,
        }
        impl Read for Trip<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                bytes.fill(0x55);
                self.read += bytes.len();
                if self.read >= IO_BUFFER {
                    self.stop.set();
                }
                Ok(bytes.len())
            }
        }
        let key = Aes128Key::from_hex("42424242424242424242424242424242").unwrap();
        let iv = SegmentIv::for_sequence(1);
        let stop = crate::concurrency::Stop::new();
        let mut input = StoppableReader {
            input: Trip {
                stop: &stop,
                read: 0,
            },
            stop: Some(&stop),
            remaining: 0,
        };
        let mut output = Vec::new();
        assert_eq!(
            decrypt_stream(&mut input, &mut output, &key, &iv),
            Err(ConversionError("segment-read-failed"))
        );
        assert_eq!(input.input.read, IO_BUFFER);
        assert!(output.is_empty());
    }
    #[test]
    fn sequence_iv_is_a_zero_padded_128_bit_big_endian_integer() {
        assert_eq!(SegmentIv::for_sequence(0).0.as_ref(), &[0; 16]);
        let mut expected = [0; 16];
        expected[8..].copy_from_slice(&u64::MAX.to_be_bytes());
        assert_eq!(SegmentIv::for_sequence(u64::MAX).0.as_ref(), &expected);
        assert_eq!(
            SegmentIv::from_hex("0x1").unwrap().0.as_ref(),
            SegmentIv::for_sequence(1).0.as_ref()
        );
    }
}
