//! Larger bounded batches for the measured macOS file-copy fallback.
use std::io::{self, Read, Write};
use zeroize::Zeroizing;

pub(super) fn copy(input: &mut impl Read, output: &mut impl Write) -> io::Result<u64> {
    let mut buffer = Zeroizing::new(vec![0; 64 * 1024]);
    let mut total = 0_u64;
    loop {
        let read = match input.read(&mut buffer) {
            Ok(0) => return Ok(total),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        output.write_all(&buffer[..read])?;
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| io::Error::other("copy byte count overflow"))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[derive(Default)]
    struct Writer {
        bytes: Vec<u8>,
        calls: usize,
        flushed: bool,
    }
    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            self.flushed = true;
            Ok(())
        }
    }

    struct Input {
        bytes: Cursor<Vec<u8>>,
        calls: usize,
    }
    impl Read for Input {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            self.bytes.read(output)
        }
    }

    #[test]
    fn large_streams_use_fewer_io_calls_without_changing_the_bytes() {
        let bytes = (0_u8..=255)
            .cycle()
            .take(3 * 64 * 1024 + 17)
            .collect::<Vec<_>>();
        let mut input = Input {
            bytes: Cursor::new(bytes.clone()),
            calls: 0,
        };
        let mut output = Writer::default();
        assert_eq!(copy(&mut input, &mut output).unwrap(), bytes.len() as u64);
        assert_eq!(output.bytes, bytes);
        assert!(input.calls <= 5, "read calls: {}", input.calls);
        assert!(output.calls <= 4, "write calls: {}", output.calls);
        assert!(!output.flushed);
    }

    #[test]
    fn caller_limits_leave_the_following_byte_unread_at_every_batch_boundary() {
        let bytes = (0_u8..=255).cycle().take(65554).collect::<Vec<_>>();
        for limit in [0, 1, 8191, 8192, 8193, 65535, 65536, 65537] {
            let mut input = Cursor::new(&bytes);
            let mut output = Writer::default();
            assert_eq!(
                copy(&mut (&mut input).take(limit), &mut output).unwrap(),
                limit
            );
            let end = usize::try_from(limit).unwrap();
            assert_eq!(output.bytes, bytes[..end]);
            assert_eq!(input.position(), limit);
            let mut next = [0];
            input.read_exact(&mut next).unwrap();
            assert_eq!(next[0], bytes[end]);
            assert!(!output.flushed);
        }
    }

    #[test]
    fn interrupted_reads_and_writes_and_short_writes_preserve_the_full_stream() {
        struct InterruptedInput {
            interrupted: bool,
            bytes: Cursor<Vec<u8>>,
        }
        impl Read for InterruptedInput {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let length = output.len().min(11);
                self.bytes.read(&mut output[..length])
            }
        }
        struct InterruptedWriter {
            interrupted: bool,
            bytes: Vec<u8>,
        }
        impl Write for InterruptedWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if !self.interrupted {
                    self.interrupted = true;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                let length = bytes.len().min(3);
                self.bytes.extend_from_slice(&bytes[..length]);
                Ok(length)
            }
            fn flush(&mut self) -> io::Result<()> {
                panic!("stream copy must not flush the caller's writer")
            }
        }
        let bytes = (0_u8..=255).cycle().take(257).collect::<Vec<_>>();
        let mut input = InterruptedInput {
            interrupted: false,
            bytes: Cursor::new(bytes.clone()),
        };
        let mut output = InterruptedWriter {
            interrupted: false,
            bytes: Vec::new(),
        };
        assert_eq!(copy(&mut input, &mut output).unwrap(), bytes.len() as u64);
        assert_eq!(output.bytes, bytes);
        assert!(input.interrupted && output.interrupted);
    }

    #[test]
    fn a_read_failure_preserves_only_the_successfully_reported_prefix() {
        struct FailingInput(bool);
        impl Read for FailingInput {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    output.fill(0xaa);
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
                self.0 = true;
                output[..4].copy_from_slice(b"safe");
                Ok(4)
            }
        }
        let mut input = FailingInput(false);
        let mut output = Writer::default();
        assert_eq!(
            copy(&mut input, &mut output).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(output.bytes, b"safe");
        assert!(!output.flushed);
    }

    #[test]
    fn write_failure_and_zero_write_are_returned_without_hiding_the_prefix() {
        struct FailingWriter {
            bytes: Vec<u8>,
            zero: bool,
        }
        impl Write for FailingWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if !self.bytes.is_empty() {
                    return if self.zero {
                        Ok(0)
                    } else {
                        Err(io::ErrorKind::PermissionDenied.into())
                    };
                }
                self.bytes.extend_from_slice(&bytes[..3]);
                Ok(3)
            }
            fn flush(&mut self) -> io::Result<()> {
                panic!("stream copy must not flush a failed writer")
            }
        }
        for zero in [false, true] {
            let mut input = Cursor::new(b"safe bytes");
            let mut output = FailingWriter {
                bytes: Vec::new(),
                zero,
            };
            assert_eq!(
                copy(&mut input, &mut output).unwrap_err().kind(),
                if zero {
                    io::ErrorKind::WriteZero
                } else {
                    io::ErrorKind::PermissionDenied
                }
            );
            assert_eq!(output.bytes, b"saf");
        }
    }
}
