//! Bounded private transport. No wire payload is suitable for diagnostics.
use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};
use std::io::{self, IoSlice, IoSliceMut, Read, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use zeroize::Zeroizing;

pub(super) const MAX_PACKET: usize = 1024 * 1024;
pub(super) const MAX_FIELDS: usize = 16384;

pub(super) fn invalid() -> io::Error {
    io::Error::other("tool ownership protocol invalid")
}

pub(super) fn send_rights(stream: &UnixStream, byte: u8, files: &[OwnedFd]) -> io::Result<()> {
    if files.len() > 3 {
        return Err(invalid());
    }
    let fds = files.iter().map(AsFd::as_fd).collect::<Vec<_>>();
    let mut storage = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut ancillary = SendAncillaryBuffer::new(&mut storage);
    if !fds.is_empty() && !ancillary.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(invalid());
    }
    let count = rustix::net::sendmsg(
        stream,
        &[IoSlice::new(&[byte])],
        &mut ancillary,
        SendFlags::empty(),
    )?;
    if count != 1 {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn receive_rights(
    stream: &UnixStream,
    flags: RecvFlags,
) -> io::Result<(u8, Vec<OwnedFd>)> {
    #[cfg(target_os = "linux")]
    let flags = flags | RecvFlags::CMSG_CLOEXEC;
    let mut byte = [0];
    let mut storage = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(3))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut storage);
    let result = rustix::net::recvmsg(
        stream,
        &mut [IoSliceMut::new(&mut byte)],
        &mut ancillary,
        flags,
    )?;
    let mut files = Vec::new();
    for item in ancillary.drain() {
        match item {
            RecvAncillaryMessage::ScmRights(items) => files.extend(items),
            _ => return Err(invalid()),
        }
    }
    for file in &files {
        rustix::io::fcntl_setfd(file, rustix::io::FdFlags::CLOEXEC)?;
    }
    if result.bytes == 0 {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
    }
    if result.bytes != 1
        || result
            .flags
            .intersects(ReturnFlags::CTRUNC | ReturnFlags::TRUNC)
        || files.len() > 3
    {
        return Err(invalid());
    }
    Ok((byte[0], files))
}

pub(super) fn send(
    stream: &mut UnixStream,
    kind: u8,
    bytes: &[u8],
    files: &[OwnedFd],
) -> io::Result<()> {
    if bytes.len() > MAX_PACKET {
        return Err(invalid());
    }
    send_rights(stream, kind, files)?;
    stream.write_all(
        &u32::try_from(bytes.len())
            .map_err(|_| invalid())?
            .to_be_bytes(),
    )?;
    stream.write_all(bytes)
}

#[cfg(test)]
pub(super) fn receive(
    stream: &mut UnixStream,
) -> io::Result<(u8, Zeroizing<Vec<u8>>, Vec<OwnedFd>)> {
    let (kind, files) = receive_rights(stream, RecvFlags::empty())?;
    receive_body(stream, kind, files)
}

pub(super) fn receive_body(
    stream: &mut UnixStream,
    kind: u8,
    files: Vec<OwnedFd>,
) -> io::Result<(u8, Zeroizing<Vec<u8>>, Vec<OwnedFd>)> {
    let mut size = [0; 4];
    stream.read_exact(&mut size)?;
    let size = u32::from_be_bytes(size) as usize;
    if size > MAX_PACKET {
        return Err(invalid());
    }
    let mut bytes = Zeroizing::new(vec![0; size]);
    stream.read_exact(&mut bytes)?;
    Ok((kind, bytes, files))
}

pub(super) struct Decoder<'a> {
    bytes: &'a [u8],
    fields: usize,
}
impl<'a> Decoder<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, fields: 0 }
    }
    pub(super) fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
    pub(super) fn byte(&mut self) -> io::Result<u8> {
        let (&byte, rest) = self.bytes.split_first().ok_or_else(invalid)?;
        self.bytes = rest;
        Ok(byte)
    }
    pub(super) fn field(&mut self) -> io::Result<&'a [u8]> {
        self.fields += 1;
        if self.fields > MAX_FIELDS || self.bytes.len() < 4 {
            return Err(invalid());
        }
        let size = u32::from_be_bytes(self.bytes[..4].try_into().map_err(|_| invalid())?) as usize;
        self.bytes = &self.bytes[4..];
        let data = self.bytes.get(..size).ok_or_else(invalid)?;
        self.bytes = &self.bytes[size..];
        Ok(data)
    }
    pub(super) fn end(self) -> io::Result<()> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(invalid())
        }
    }
}

pub(super) fn field(out: &mut Vec<u8>, value: &[u8]) -> io::Result<()> {
    out.extend_from_slice(
        &u32::try_from(value.len())
            .map_err(|_| invalid())?
            .to_be_bytes(),
    );
    out.extend_from_slice(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transfers_owned_cloexec_pipe_without_changing_bytes() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        let (read, mut write) = std::io::pipe().unwrap();
        send(&mut a, 7, b"bounded", &[read.into()]).unwrap();
        let (kind, bytes, mut files) = receive(&mut b).unwrap();
        assert_eq!(kind, 7);
        assert_eq!(&*bytes, b"bounded");
        assert!(
            rustix::io::fcntl_getfd(&files[0])
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        write.write_all(b"actual").unwrap();
        drop(write);
        let mut actual = String::new();
        std::fs::File::from(files.pop().unwrap())
            .read_to_string(&mut actual)
            .unwrap();
        assert_eq!(actual, "actual");
    }
    #[test]
    fn transfers_each_admitted_pipe_descriptor_count_and_zero_tag() {
        for count in 1..=3 {
            let (mut a, mut b) = UnixStream::pair().unwrap();
            let mut readers = Vec::new();
            let mut writers = Vec::new();
            for _ in 0..count {
                let (reader, writer) = std::io::pipe().unwrap();
                readers.push(reader.into());
                writers.push(writer);
            }
            send(&mut a, 0, b"response", &readers).unwrap();
            let (kind, bytes, files) = receive(&mut b).unwrap();
            assert_eq!(kind, 0);
            assert_eq!(&*bytes, b"response");
            assert_eq!(files.len(), count);
            for (mut writer, file) in writers.into_iter().zip(files) {
                writer.write_all(b"unchanged").unwrap();
                drop(writer);
                let mut bytes = Vec::new();
                std::fs::File::from(file).read_to_end(&mut bytes).unwrap();
                assert_eq!(bytes, b"unchanged");
            }
        }
    }

    #[test]
    fn excess_rights_are_rejected_and_every_received_descriptor_is_closed() {
        const WORKER: &str = "DRMR_TEST_EXCESS_RIGHTS_WORKER";
        if std::env::var_os(WORKER).is_none() {
            // This raw recvmsg boundary deliberately observes descriptors before
            // CLOEXEC validation. Isolate its FD table from unrelated test fixture
            // installers that use std::Command rather than the production gate.
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command.args(["--exact","native::broker::wire::tests::excess_rights_are_rejected_and_every_received_descriptor_is_closed","--nocapture"]).env(WORKER,"1");
            let mut child = crate::native::ManagedChild::spawn(command, true).unwrap();
            let status = child.wait().unwrap();
            child.finish().unwrap();
            assert!(
                status.success(),
                "isolated malformed-rights ownership control failed"
            );
            return;
        }
        let (a, b) = UnixStream::pair().unwrap();
        let mut readers: Vec<OwnedFd> = Vec::new();
        let mut writers = Vec::new();
        for _ in 0..4 {
            let (reader, writer) = std::io::pipe().unwrap();
            readers.push(reader.into());
            writers.push(writer);
        }
        let borrowed = readers.iter().map(AsFd::as_fd).collect::<Vec<_>>();
        let mut storage = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(4))];
        let mut ancillary = SendAncillaryBuffer::new(&mut storage);
        assert!(ancillary.push(SendAncillaryMessage::ScmRights(&borrowed)));
        rustix::net::sendmsg(
            &a,
            &[IoSlice::new(&[1])],
            &mut ancillary,
            SendFlags::empty(),
        )
        .unwrap();
        assert!(receive_rights(&b, RecvFlags::empty()).is_err());
        // The caller rejects and closes the malformed transport; queued rights
        // not delivered by a truncated recvmsg are owned by that socket.
        drop(b);
        drop(a);
        drop(readers);
        for (index, mut writer) in writers.into_iter().enumerate() {
            assert_eq!(
                writer.write_all(b"x").map_err(|e| e.kind()),
                Err(io::ErrorKind::BrokenPipe),
                "reader index {index}"
            );
        }
    }

    #[test]
    fn rejects_oversize_before_payload_allocation_and_truncated_fields() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        send_rights(&a, 1, &[]).unwrap();
        a.write_all(&u32::try_from(MAX_PACKET + 1).unwrap().to_be_bytes())
            .unwrap();
        assert!(receive(&mut b).is_err());
        assert!(Decoder::new(&[0, 0, 0, 2, 1]).field().is_err());
        assert!(Decoder::new(&[1]).end().is_err());
    }
}
