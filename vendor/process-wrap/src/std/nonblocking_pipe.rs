//! Readiness-gated reads of exclusively owned child output pipes.

use std::{
	io::{self, Read},
	os::windows::io::AsRawHandle,
	process::{ChildStderr, ChildStdout},
};
use windows::Win32::{
	Foundation::{ERROR_BROKEN_PIPE, HANDLE},
	System::Pipes::{GetNamedPipeInfo, NAMED_PIPE_MODE, PIPE_TYPE_MESSAGE, PeekNamedPipe},
};

/// An exclusively owned byte reader for a standard child output pipe.
///
/// Standard-library `Stdio::piped` parent handles support overlapped I/O, so
/// `PeekNamedPipe` does not wait for a writer. A read starts only when bytes are
/// available, and requests no more than that amount. This reader neither exposes
/// nor duplicates its handle; no other reader may consume bytes between the peek
/// and read. The caller must transfer ownership before starting any pipe I/O.
///
/// A caller-supplied synchronous handle does not have the same readiness bound:
/// Microsoft documents that a peek on a synchronous handle can block. This type
/// does not reopen handles, change pipe modes or security, start I/O in the
/// background, or cancel an operation with a borrowed buffer still in flight.
#[derive(Debug)]
pub struct NonblockingPipe {
	pipe: Output,
}

#[derive(Debug)]
enum Output {
	Stdout(ChildStdout),
	Stderr(ChildStderr),
}

impl Output {
	fn handle(&self) -> HANDLE {
		HANDLE(match self {
			Self::Stdout(pipe) => pipe.as_raw_handle(),
			Self::Stderr(pipe) => pipe.as_raw_handle(),
		})
	}

	fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
		match self {
			Self::Stdout(pipe) => pipe.read(bytes),
			Self::Stderr(pipe) => pipe.read(bytes),
		}
	}
}

impl NonblockingPipe {
	/// Own a piped child stdout before any read or handle duplication.
	pub fn stdout(pipe: ChildStdout) -> io::Result<Self> {
		Self::new(Output::Stdout(pipe))
	}

	/// Own a piped child stderr before any read or handle duplication.
	pub fn stderr(pipe: ChildStderr) -> io::Result<Self> {
		Self::new(Output::Stderr(pipe))
	}

	fn new(pipe: Output) -> io::Result<Self> {
		let mut flags = NAMED_PIPE_MODE(0);
		// SAFETY: Output keeps the handle alive; flags points to writable storage,
		// and all other optional output pointers are null. No I/O is submitted.
		unsafe { GetNamedPipeInfo(pipe.handle(), Some(&mut flags), None, None, None) }
			.map_err(io::Error::other)?;
		if flags.0 & PIPE_TYPE_MESSAGE.0 != 0 {
			return Err(io::Error::new(
				io::ErrorKind::InvalidInput,
				"captured output must be a byte pipe",
			));
		}
		Ok(Self { pipe })
	}
}

impl Read for NonblockingPipe {
	fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
		if buffer.is_empty() {
			return Ok(0);
		}
		let mut available = 0;
		// SAFETY: Output owns a live pipe. available is writable for this
		// synchronous metadata call; no data buffer or pending read is supplied.
		let peek = unsafe {
			PeekNamedPipe(
				self.pipe.handle(),
				None,
				0,
				None,
				Some(&mut available),
				None,
			)
		};
		match peek {
			Err(error) if error.code() == ERROR_BROKEN_PIPE.to_hresult() => Ok(0),
			Err(error) => Err(io::Error::other(error)),
			Ok(()) if available == 0 => Err(io::ErrorKind::WouldBlock.into()),
			Ok(()) => {
				let take = buffer.len().min(available as usize);
				// This is the sole reader; a producer can append bytes or close
				// but cannot consume the bytes already observed. Standard Read
				// owns the OS read completion and its borrowed-buffer lifetime.
				self.pipe.read(&mut buffer[..take])
			}
		}
	}
}
