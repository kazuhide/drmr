use super::{CHUNK_BYTES, PayloadBuffer, PayloadChunk, positioned::PositionedInput};
use std::io::{self, Read};
use zeroize::Zeroize as _;

enum Source<'a> {
    Direct(&'a mut PositionedInput),
    Window {
        input: &'a mut PositionedInput,
        bytes: PayloadChunk<'a>,
        at: usize,
        loaded: bool,
    },
}

/// Batch only the current, already qualified sample's raw input bytes.
/// Large samples and single input ranges retain direct bounded streaming.
pub(in crate::conversion::native_media) struct SampleInput<'a> {
    source: Source<'a>,
}
impl<'a> SampleInput<'a> {
    pub fn new(
        input: &'a mut PositionedInput,
        storage: &'a mut PayloadBuffer,
        size: usize,
        split: bool,
    ) -> Result<Self, super::ConversionError> {
        let source = if split && (1..=CHUNK_BYTES).contains(&size) {
            Source::Window {
                input,
                bytes: storage.chunk(size)?,
                at: 0,
                loaded: false,
            }
        } else {
            Source::Direct(input)
        };
        Ok(Self { source })
    }
}
impl Read for SampleInput<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        match &mut self.source {
            Source::Direct(input) => input.read(output),
            Source::Window {
                input,
                bytes,
                at,
                loaded,
            } => {
                if output.is_empty() {
                    return Ok(0);
                }
                if !*loaded {
                    *loaded = true;
                    if let Err(error) = input.read_exact(bytes.bytes) {
                        // A failed fill cannot retry from a new physical offset.
                        *at = bytes.bytes.len();
                        bytes.bytes.zeroize();
                        return Err(error);
                    }
                }
                let length = output.len().min(bytes.bytes.len() - *at);
                let source = &mut bytes.bytes[*at..*at + length];
                output[..length].copy_from_slice(source);
                source.zeroize();
                *at += length;
                Ok(length)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::File, io::Seek as _};

    fn input(size: usize) -> (crate::native::PrivateTempDir, PositionedInput) {
        let owner = crate::native::private_tempdir().unwrap();
        let path = owner.path().join("sample-bytes");
        let bytes = (0_u8..=255).cycle().take(size).collect::<Vec<_>>();
        std::fs::write(&path, &bytes).unwrap();
        let file = PositionedInput::new(File::open(path).unwrap());
        (owner, file)
    }

    #[test]
    fn subsample_reads_share_only_the_current_sample_and_wipe_consumed_bytes() {
        let (_owner, mut file) = input(96);
        let mut storage = PayloadBuffer::default();
        let mut output = [0; 32];
        {
            let mut sample = SampleInput::new(&mut file, &mut storage, 32, true).unwrap();
            sample.read_exact(&mut output[..3]).unwrap();
            let Source::Window { bytes, at, .. } = &sample.source else {
                panic!("window required")
            };
            assert_eq!(bytes.bytes.len(), 32);
            assert_eq!(*at, 3);
            assert!(bytes.bytes[..3].iter().all(|byte| *byte == 0));
            assert_eq!(&bytes.bytes[3..], &(3_u8..32).collect::<Vec<_>>());
            sample.read_exact(&mut output[3..10]).unwrap();
            sample.read_exact(&mut output[10..]).unwrap();
            assert_eq!(sample.read(&mut [0; 1]).unwrap(), 0);
        }
        assert_eq!(
            output,
            std::array::from_fn(|index| u8::try_from(index).unwrap())
        );
        assert_eq!(file.reads, 1);
        assert_eq!(file.position, Some(32));
        assert!(storage.bytes.iter().all(|byte| *byte == 0));
        let mut next = [0; 32];
        file.read_exact(&mut next).unwrap();
        assert_eq!(
            next,
            std::array::from_fn(|index| u8::try_from(index + 32).unwrap())
        );
        assert_eq!(file.position, Some(64));
    }

    #[test]
    fn clear_single_span_and_large_samples_keep_direct_streaming_without_a_window() {
        let (_owner, mut file) = input(CHUNK_BYTES + 32);
        let mut storage = PayloadBuffer::default();
        for (size, fragmented) in [(17, false), (CHUNK_BYTES + 16, true)] {
            file.seek(std::io::SeekFrom::Start(0)).unwrap();
            let mut sample = SampleInput::new(&mut file, &mut storage, size, fragmented).unwrap();
            assert!(matches!(sample.source, Source::Direct(_)));
            sample.read_exact(&mut [0; 17]).unwrap();
        }
        assert!(storage.bytes.is_empty());
    }

    #[test]
    fn sample_window_is_wiped_on_error_panic_and_short_read_without_touching_unused_storage() {
        let (_owner, mut file) = input(CHUNK_BYTES);
        let mut storage = PayloadBuffer::default();
        {
            let mut sample = SampleInput::new(&mut file, &mut storage, CHUNK_BYTES, true).unwrap();
            sample.read_exact(&mut [0; 1]).unwrap();
        }
        let address = storage.bytes.as_ptr();
        assert!(storage.bytes.iter().all(|byte| *byte == 0));
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        let failure = (|| -> Result<(), super::super::ConversionError> {
            let mut sample = SampleInput::new(&mut file, &mut storage, 17, true)?;
            sample.read_exact(&mut [0; 1]).unwrap();
            let Source::Window { bytes, .. } = &sample.source else {
                panic!("window required")
            };
            assert_eq!(bytes.bytes.len(), 17);
            Err(super::super::BAD)
        })();
        assert_eq!(failure, Err(super::super::BAD));
        assert_eq!(storage.bytes.as_ptr(), address);
        assert!(storage.bytes.iter().all(|byte| *byte == 0));
        file.seek(std::io::SeekFrom::Start(0)).unwrap();
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut sample = SampleInput::new(&mut file, &mut storage, 17, true).unwrap();
            sample.read_exact(&mut [0; 1]).unwrap();
            panic!("sample visitor failed");
        }));
        assert!(panic.is_err());
        assert!(storage.bytes.iter().all(|byte| *byte == 0));
        file.seek(std::io::SeekFrom::End(-8)).unwrap();
        let mut output = [0xff; 3];
        {
            let mut sample = SampleInput::new(&mut file, &mut storage, 17, true).unwrap();
            assert!(sample.read_exact(&mut output).is_err());
            let calls = match &sample.source {
                Source::Window { input, bytes, .. } => {
                    assert!(bytes.bytes.iter().all(|byte| *byte == 0));
                    input.reads
                }
                Source::Direct(_) => panic!("window required"),
            };
            assert_eq!(sample.read(&mut output).unwrap(), 0);
            let Source::Window { input, .. } = &sample.source else {
                panic!("window required")
            };
            assert_eq!(input.reads, calls);
        }
        assert_eq!(output, [0xff; 3]);
        assert!(storage.bytes.iter().all(|byte| *byte == 0));
    }
}
