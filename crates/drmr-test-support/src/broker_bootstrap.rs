//! Minimal bounded broker handshake for native fixtures used as a direct server CLI.
use std::{ffi::OsString, io};

#[cfg(unix)]
pub fn connect(args: &mut Vec<OsString>) -> io::Result<Option<std::os::unix::net::UnixStream>> {
    use std::{
        io::{Read as _, Write as _},
        os::{fd::AsFd as _, unix::net::UnixStream},
        time::Duration,
    };
    let marker = std::env::var_os("DRMR_TOOL_BROKER_V1");
    let prefix = args
        .first()
        .is_some_and(|arg| arg == "--internal-tool-broker-v1");
    if marker.is_none() && !prefix {
        return Ok(None);
    }
    let valid = marker
        .as_ref()
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse::<u32>().ok())
        .is_some_and(|pid| pid > 1);
    if !prefix || !valid {
        return Err(io::Error::other("fixture-broker-invalid"));
    }
    let mut stream = UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?);
    stream.peer_addr()?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(b"DRMRBRK1")?;
    let mut ack = [0; 4];
    stream.read_exact(&mut ack)?;
    if ack != *b"OK01" {
        return Err(io::Error::other("fixture-broker-invalid"));
    }
    args.remove(0);
    Ok(Some(stream))
}

#[cfg(not(unix))]
pub fn connect(_args: &mut Vec<OsString>) -> io::Result<()> {
    Ok(())
}
