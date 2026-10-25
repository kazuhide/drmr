use super::{CHUNK_BYTES, PayloadBuffer, PayloadChunk, positioned::PositionedInput};
use std::io::{self, Read};
use zeroize::Zeroize as _;

enum Source<'a> {
    Direct(&'a mut PositionedInput),
    Window {
        input: &'a mut PositionedInput,
        window: Window<'a>,
    },
}

struct Window<'a> {
    bytes: PayloadChunk<'a>,
    at: usize,
    available: usize,
    loaded: bool,
    pending: Option<io::Error>,
}
impl<'a> Window<'a> {
    fn new(bytes: PayloadChunk<'a>) -> Self {
        Self {
            bytes,
            at: 0,
            available: 0,
            loaded: false,
            pending: None,
        }
    }
    fn fill(&mut self, input: &mut impl Read) {
        self.loaded = true;
        while self.available < self.bytes.bytes.len() {
            match input.read(&mut self.bytes.bytes[self.available..]) {
                Ok(0) => break,
                Ok(count) => self.available += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.pending = Some(error);
                    break;
                }
            }
        }
        // Preserve only bytes reported as valid by Read. A failed read may have
        // touched its output without reporting any additional valid prefix.
        self.bytes.bytes[self.available..].zeroize();
    }
    fn read(&mut self, input: &mut impl Read, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if !self.loaded {
            self.fill(input);
        }
        if self.at == self.available {
            // Later I/O failure must not hide an earlier logical read/callback.
            // Once reported, the window remains exhausted and never refills.
            return self.pending.take().map_or(Ok(0), Err);
        }
        let length = output.len().min(self.available - self.at);
        let source = &mut self.bytes.bytes[self.at..self.at + length];
        output[..length].copy_from_slice(source);
        source.zeroize();
        self.at += length;
        Ok(length)
    }
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
                window: Window::new(storage.chunk(size)?),
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
            Source::Window { input, window } => window.read(input, output),
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
            let Source::Window { window, .. } = &sample.source else {
                panic!("window required")
            };
            assert_eq!(window.bytes.bytes.len(), 32);
            assert_eq!(window.at, 3);
            assert!(window.bytes.bytes[..3].iter().all(|byte| *byte == 0));
            assert_eq!(&window.bytes.bytes[3..], &(3_u8..32).collect::<Vec<_>>());
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
    fn sample_window_is_wiped_on_error_or_panic_without_touching_unused_storage() {
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
            let Source::Window { window, .. } = &sample.source else {
                panic!("window required")
            };
            assert_eq!(window.bytes.bytes.len(), 17);
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
    }

    #[test]
    fn truncated_current_sample_delivers_its_available_subsamples_before_eof() {
        for buffered in [false, true] {
            let (_owner, mut file) = input(8);
            let mut storage = PayloadBuffer::default();
            let mut output = [0xff; 8];
            {
                let mut sample = SampleInput::new(&mut file, &mut storage, 17, buffered).unwrap();
                sample.read_exact(&mut output[..3]).unwrap();
                sample.read_exact(&mut output[3..5]).unwrap();
                sample.read_exact(&mut output[5..]).unwrap();
                assert_eq!(
                    sample.read_exact(&mut [0; 1]).unwrap_err().kind(),
                    io::ErrorKind::UnexpectedEof
                );
                if buffered {
                    let Source::Window { input, .. } = &sample.source else {
                        panic!("window required")
                    };
                    let calls = input.reads;
                    assert_eq!(sample.read(&mut [0; 1]).unwrap(), 0);
                    let Source::Window { input, .. } = &sample.source else {
                        panic!("window required")
                    };
                    assert_eq!(input.reads, calls);
                }
            }
            assert_eq!(output, [0, 1, 2, 3, 4, 5, 6, 7]);
            assert!(storage.bytes.iter().all(|byte| *byte == 0));
        }
    }

    #[test]
    fn available_prefix_precedes_a_late_io_error_and_interrupted_fill_is_retried() {
        struct PrefixThenError {
            calls: usize,
        }
        impl Read for PrefixThenError {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                self.calls += 1;
                match self.calls {
                    1 => Err(io::ErrorKind::Interrupted.into()),
                    2 => {
                        output[..8].copy_from_slice(&[0, 1, 2, 3, 4, 5, 6, 7]);
                        Ok(8)
                    }
                    _ => {
                        output.fill(0xaa);
                        Err(io::ErrorKind::PermissionDenied.into())
                    }
                }
            }
        }
        let mut input = PrefixThenError { calls: 0 };
        let mut storage = PayloadBuffer::default();
        let mut output = [0xff; 8];
        {
            let mut window = Window::new(storage.chunk(17).unwrap());
            assert_eq!(window.read(&mut input, &mut []).unwrap(), 0);
            assert_eq!(input.calls, 0);
            assert_eq!(window.read(&mut input, &mut output[..3]).unwrap(), 3);
            assert_eq!(input.calls, 3);
            assert!(window.bytes.bytes[..3].iter().all(|byte| *byte == 0));
            assert!(window.bytes.bytes[8..].iter().all(|byte| *byte == 0));
            assert_eq!(window.read(&mut input, &mut output[3..]).unwrap(), 5);
            assert_eq!(
                window.read(&mut input, &mut [0; 1]).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            assert_eq!(window.read(&mut input, &mut [0; 1]).unwrap(), 0);
            assert_eq!(input.calls, 3);
            assert!(window.bytes.bytes.iter().all(|byte| *byte == 0));
        }
        assert_eq!(output, [0, 1, 2, 3, 4, 5, 6, 7]);
        assert!(storage.bytes.iter().all(|byte| *byte == 0));
    }
}
