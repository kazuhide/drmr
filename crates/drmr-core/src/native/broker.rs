//! Private per-job Unix tool ownership. The server is the actual parent of every tool.
//! This module never accepts file descriptors in the host process.
mod wire;

use super::{ManagedChild, ManagedIo, ManagedStdio};
use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::{ffi::OsStrExt, net::UnixStream, process::ExitStatusExt};
use std::process::{Command, ExitStatus};
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const ENV: &str = "DRMR_TOOL_BROKER_V1";
pub const ARG: &str = "--internal-tool-broker-v1";
const HELLO: &[u8; 8] = b"DRMRBRK1";
const ACK: &[u8; 4] = b"OK01";
const MAX_SESSIONS: usize = 256;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_TIMEOUT: Duration = Duration::from_millis(100);
static CLIENT: OnceLock<Mutex<UnixStream>> = OnceLock::new();
// Darwin has no MSG_CMSG_CLOEXEC. Serialize received-descriptor CLOEXEC setup
// with all project-managed local process births in this CLI process.
pub(super) static SPAWN_GATE: Mutex<()> = Mutex::new(());

fn unavailable() -> io::Error {
    io::Error::other("tool ownership unavailable")
}
fn lock<T>(value: &Mutex<T>) -> io::Result<std::sync::MutexGuard<'_, T>> {
    value.lock().map_err(|_| unavailable())
}

pub fn active() -> bool {
    CLIENT.get().is_some()
}

pub fn initialize() -> io::Result<()> {
    let Some(parent) = std::env::var_os(ENV) else {
        return if std::env::args_os().nth(1).as_deref() == Some(OsStr::new(ARG)) {
            Err(unavailable())
        } else {
            Ok(())
        };
    };
    if std::env::args_os().nth(1).as_deref() != Some(OsStr::new(ARG)) {
        return Err(unavailable());
    }
    let parent = parent
        .to_str()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(unavailable)?;
    if rustix::process::getppid().map(|p| p.as_raw_nonzero().get().cast_unsigned()) != Some(parent)
        || rustix::process::getpgrp() != rustix::process::getpid()
    {
        return Err(unavailable());
    }
    let descriptor = rustix::io::fcntl_dupfd_cloexec(std::io::stdin(), 3)?;
    let mut stream = UnixStream::from(descriptor);
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(HELLO)?;
    let mut ack = [0; 4];
    stream.read_exact(&mut ack)?;
    if &ack != ACK {
        return Err(unavailable());
    }
    CLIENT.set(Mutex::new(stream)).map_err(|_| unavailable())
}

fn encoded(fields: &[&[u8]], prefix: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let size = fields
        .iter()
        .try_fold(prefix, |size, field| {
            size.checked_add(4)?.checked_add(field.len())
        })
        .ok_or_else(wire::invalid)?;
    if fields.len() > wire::MAX_FIELDS || size > wire::MAX_PACKET {
        return Err(wire::invalid());
    }
    Ok(Zeroizing::new(Vec::with_capacity(size)))
}

fn base_environment(environment: &[(OsString, OsString)]) -> io::Result<Zeroizing<Vec<u8>>> {
    let fields = environment
        .iter()
        .filter(|(name, _)| name != ENV)
        .flat_map(|(name, value)| [name.as_bytes(), value.as_bytes()])
        .collect::<Vec<_>>();
    let mut output = encoded(&fields, 0)?;
    for field in fields {
        wire::field(&mut output, field)?;
    }
    Ok(output)
}

fn encode_command(command: &Command, io: &ManagedIo) -> io::Result<Zeroizing<Vec<u8>>> {
    let directory = command
        .get_current_dir()
        .map(std::path::Path::to_path_buf)
        .map_or_else(std::env::current_dir, Ok)?;
    let args = command.get_args().collect::<Vec<_>>();
    let environment = command
        .get_envs()
        .filter(|(name, _)| *name != ENV)
        .collect::<Vec<_>>();
    let mut fields = vec![
        command.get_program().as_bytes(),
        directory.as_os_str().as_bytes(),
    ];
    fields.extend(args.iter().map(|s| s.as_bytes()));
    for (name, value) in &environment {
        fields.push(name.as_bytes());
        if let Some(value) = value {
            fields.push(value.as_bytes());
        }
    }
    let mut output = encoded(&fields, 11 + environment.len())?;
    for mode in [&io.stdin, &io.stdout, &io.stderr] {
        output.push(match mode {
            ManagedStdio::Null => 0,
            ManagedStdio::Piped => 1,
            ManagedStdio::File(_) => return Err(unavailable()),
        });
    }
    output.extend_from_slice(
        &u32::try_from(args.len())
            .map_err(|_| wire::invalid())?
            .to_be_bytes(),
    );
    output.extend_from_slice(
        &u32::try_from(environment.len())
            .map_err(|_| wire::invalid())?
            .to_be_bytes(),
    );
    wire::field(&mut output, command.get_program().as_bytes())?;
    wire::field(&mut output, directory.as_os_str().as_bytes())?;
    for arg in args {
        wire::field(&mut output, arg.as_bytes())?;
    }
    for (name, value) in environment {
        wire::field(&mut output, name.as_bytes())?;
        output.push(u8::from(value.is_some()));
        if let Some(value) = value {
            wire::field(&mut output, value.as_bytes())?;
        }
    }
    Ok(output)
}

fn count(decoder: &mut wire::Decoder<'_>) -> io::Result<usize> {
    let mut value = [0; 4];
    for byte in &mut value {
        *byte = decoder.byte()?;
    }
    let value = u32::from_be_bytes(value) as usize;
    if value > wire::MAX_FIELDS {
        return Err(wire::invalid());
    }
    Ok(value)
}

fn decode_command(bytes: &[u8], base: &[u8]) -> io::Result<(Command, ManagedIo)> {
    let mut input = wire::Decoder::new(bytes);
    let mut mode = || -> io::Result<ManagedStdio> {
        match input.byte()? {
            0 => Ok(ManagedStdio::Null),
            1 => Ok(ManagedStdio::Piped),
            _ => Err(wire::invalid()),
        }
    };
    let io = ManagedIo {
        stdin: mode()?,
        stdout: mode()?,
        stderr: mode()?,
    };
    let argc = count(&mut input)?;
    let envc = count(&mut input)?;
    let mut command = Command::new(OsStr::from_bytes(input.field()?));
    command
        .current_dir(OsStr::from_bytes(input.field()?))
        .env_clear();
    let mut base = wire::Decoder::new(base);
    while !base.is_empty() {
        let name = base.field()?;
        let value = base.field()?;
        command.env(OsStr::from_bytes(name), OsStr::from_bytes(value));
    }
    for _ in 0..argc {
        command.arg(OsStr::from_bytes(input.field()?));
    }
    for _ in 0..envc {
        let name = OsStr::from_bytes(input.field()?);
        match input.byte()? {
            0 => {
                command.env_remove(name);
            }
            1 => {
                command.env(name, OsStr::from_bytes(input.field()?));
            }
            _ => return Err(wire::invalid()),
        }
    }
    command.env_remove(ENV);
    input.end()?;
    Ok((command, io))
}

fn receive_rights_guarded(stream: &UnixStream) -> io::Result<(u8, Vec<OwnedFd>)> {
    let deadline = Instant::now() + Duration::from_secs(31);
    loop {
        let received = {
            let _gate = lock(&SPAWN_GATE)?;
            wire::receive_rights(stream, rustix::net::RecvFlags::DONTWAIT)
        };
        match received {
            Err(error) if retry(&error) && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            result => return result,
        }
    }
}

/// A remote handle retaining the same pipe/status interface as a local child.
pub(super) struct Remote {
    stream: UnixStream,
    pid: u32,
    status: Option<ExitStatus>,
    finished: bool,
}
impl std::fmt::Debug for Remote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteTool")
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}
impl Remote {
    pub(super) fn spawn(command: &Command, io: &ManagedIo) -> io::Result<ManagedChild> {
        Self::spawn_on(CLIENT.get().ok_or_else(unavailable)?, command, io)
    }
    fn spawn_on(
        connection: &Mutex<UnixStream>,
        command: &Command,
        io: &ManagedIo,
    ) -> io::Result<ManagedChild> {
        let payload = encode_command(command, io)?;
        let mut main = lock(connection)?;
        main.write_all(&[1])?;
        let (kind, mut endpoints) = receive_rights_guarded(&main)?;
        if kind != 1 || endpoints.len() != 1 {
            return Err(wire::invalid());
        }
        let mut stream = UnixStream::from(endpoints.pop().ok_or_else(wire::invalid)?);
        // Acknowledge owned, CLOEXEC receipt before the host drops its sender
        // endpoint. No sender copy remains alive while this tool runs.
        main.write_all(&[2])?;
        drop(main);
        stream.set_read_timeout(Some(Duration::from_secs(31)))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        // The host reads plain frames and never receives ancillary descriptors.
        stream.write_all(
            &u32::try_from(payload.len())
                .map_err(|_| wire::invalid())?
                .to_be_bytes(),
        )?;
        stream.write_all(&payload)?;
        let (kind, files) = receive_rights_guarded(&stream)?;
        let (kind, data, files) = wire::receive_body(&mut stream, kind, files)?;
        if kind != 0 || data.len() != 4 {
            return Err(unavailable());
        }
        let pid = u32::from_be_bytes(data[..4].try_into().map_err(|_| wire::invalid())?);
        if pid == 0 {
            return Err(wire::invalid());
        }
        let expected = [&io.stdin, &io.stdout, &io.stderr]
            .iter()
            .filter(|m| matches!(m, ManagedStdio::Piped))
            .count();
        if files.len() != expected {
            return Err(wire::invalid());
        }
        let mut files = files.into_iter();
        let stdin = if matches!(io.stdin, ManagedStdio::Piped) {
            Some(std::process::ChildStdin::from(
                files.next().ok_or_else(wire::invalid)?,
            ))
        } else {
            None
        };
        let stdout = if matches!(io.stdout, ManagedStdio::Piped) {
            Some(std::process::ChildStdout::from(
                files.next().ok_or_else(wire::invalid)?,
            ))
        } else {
            None
        };
        let stderr = if matches!(io.stderr, ManagedStdio::Piped) {
            Some(std::process::ChildStderr::from(
                files.next().ok_or_else(wire::invalid)?,
            ))
        } else {
            None
        };
        // The host may now release its copies without extending a writer's
        // lifetime into tool execution or racing receiver ownership.
        stream.write_all(&[4])?;
        Ok(ManagedChild::remote(
            Self {
                stream,
                pid,
                status: None,
                finished: false,
            },
            stdin,
            stdout,
            stderr,
        ))
    }
    pub(super) fn id(&self) -> u32 {
        self.pid
    }
    pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.status.is_some() {
            return Ok(self.status);
        }
        self.stream.write_all(&[2])?;
        let mut bytes = [0; 5];
        self.stream.read_exact(&mut bytes)?;
        match bytes[0] {
            0 => Ok(None),
            1 => {
                self.status = Some(ExitStatus::from_raw(i32::from_be_bytes(
                    bytes[1..].try_into().map_err(|_| wire::invalid())?,
                )));
                Ok(self.status)
            }
            _ => Err(unavailable()),
        }
    }
    pub(super) fn wait(&mut self) -> io::Result<ExitStatus> {
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    pub(super) fn finish(&mut self) -> io::Result<()> {
        if self.finished {
            return Ok(());
        }
        self.stream.write_all(&[3])?;
        let mut bytes = [0; 5];
        self.stream.read_exact(&mut bytes)?;
        if bytes[0] != 1 {
            return Err(unavailable());
        }
        self.status = Some(ExitStatus::from_raw(i32::from_be_bytes(
            bytes[1..].try_into().map_err(|_| wire::invalid())?,
        )));
        self.finished = true;
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
        Ok(())
    }
}

struct Session {
    connection: UnixStream,
    child: Mutex<Option<ManagedChild>>,
    result: Mutex<Option<bool>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}
struct State {
    closing: AtomicBool,
    handshake: AtomicBool,
    main_done: AtomicBool,
    entries: Mutex<Vec<Arc<Session>>>,
}
/// Server-side owner. Every entry retains an actual direct child until confirmed cleanup.
pub(super) struct Broker {
    state: Arc<State>,
    main: Option<JoinHandle<()>>,
    connection: UnixStream,
    attempted: bool,
}
impl std::fmt::Debug for Broker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolBroker").finish_non_exhaustive()
    }
}
impl Broker {
    pub(super) fn new(environment: &[(OsString, OsString)]) -> io::Result<(Self, OwnedFd)> {
        let base = Arc::new(base_environment(environment)?);
        let (mut server, client) = UnixStream::pair()?;
        server.set_read_timeout(Some(POLL_TIMEOUT))?;
        server.set_write_timeout(Some(IO_TIMEOUT))?;
        let connection = server.try_clone()?;
        let state = Arc::new(State {
            closing: AtomicBool::new(false),
            handshake: AtomicBool::new(false),
            main_done: AtomicBool::new(false),
            entries: Mutex::new(Vec::new()),
        });
        let task = Arc::clone(&state);
        let main = std::thread::Builder::new()
            .name("tool-broker".into())
            .spawn(move || {
                let handshake = (|| -> io::Result<()> {
                    let deadline = Instant::now() + IO_TIMEOUT;
                    let mut hello = [0; 8];
                    let mut offset = 0;
                    while offset < hello.len() {
                        match server.read(&mut hello[offset..]) {
                            Ok(0) => return Err(unavailable()),
                            Ok(n) => offset += n,
                            Err(e)
                                if retry(&e)
                                    && Instant::now() < deadline
                                    && !task.closing.load(Ordering::Acquire) => {}
                            Err(e) => return Err(e),
                        }
                    }
                    if &hello != HELLO {
                        return Err(unavailable());
                    }
                    server.write_all(ACK)?;
                    Ok(())
                })();
                if handshake.is_ok() {
                    task.handshake.store(true, Ordering::Release);
                    broker_main(&mut server, &task, &base);
                }
                task.closing.store(true, Ordering::Release);
                let _ = server.shutdown(std::net::Shutdown::Both);
                if let Ok(entries) = task.entries.lock() {
                    for entry in entries.iter() {
                        let _ = entry.connection.shutdown(std::net::Shutdown::Both);
                    }
                }
                task.main_done.store(true, Ordering::Release);
            })?;
        Ok((
            Self {
                state,
                main: Some(main),
                connection,
                attempted: false,
            },
            client.into(),
        ))
    }
    pub(super) fn confirm(&self) -> io::Result<()> {
        let deadline = Instant::now() + IO_TIMEOUT;
        loop {
            if self.state.handshake.load(Ordering::Acquire) {
                return Ok(());
            }
            if self.state.main_done.load(Ordering::Acquire) || Instant::now() >= deadline {
                return Err(unavailable());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    pub(super) fn close(&self) {
        self.state.closing.store(true, Ordering::Release);
        let _ = self.connection.shutdown(std::net::Shutdown::Both);
        if let Ok(entries) = self.state.entries.lock() {
            for entry in entries.iter() {
                let _ = entry.connection.shutdown(std::net::Shutdown::Both);
            }
        }
    }
    pub(super) fn finish(&mut self) -> io::Result<()> {
        self.attempted = true;
        self.close();
        let deadline = Instant::now() + Duration::from_secs(31);
        let mut retried = std::collections::BTreeSet::new();
        loop {
            let entries = lock(&self.state.entries)?.clone();
            for entry in &entries {
                let failed = *lock(&entry.result)? == Some(false);
                let exited = lock(&entry.thread)?
                    .as_ref()
                    .is_none_or(JoinHandle::is_finished);
                if failed && exited && retried.insert(Arc::as_ptr(entry) as usize) {
                    if let Some(thread) = lock(&entry.thread)?.take() {
                        thread.join().map_err(|_| unavailable())?;
                    }
                    let confirmed = lock(&entry.child)?
                        .as_mut()
                        .map_or(Ok(()), ManagedChild::finish)
                        .is_ok();
                    *lock(&entry.result)? = Some(confirmed);
                }
            }
            let complete = self.state.main_done.load(Ordering::Acquire)
                && entries.iter().all(|e| {
                    e.result.lock().is_ok_and(|r| r.is_some())
                        && e.thread
                            .lock()
                            .is_ok_and(|t| t.as_ref().is_none_or(JoinHandle::is_finished))
                });
            if complete {
                if let Some(thread) = self.main.take() {
                    thread.join().map_err(|_| unavailable())?;
                }
                let mut success = true;
                for entry in entries {
                    if let Some(thread) = lock(&entry.thread)?.take() {
                        thread.join().map_err(|_| unavailable())?;
                    }
                    success &= *lock(&entry.result)? == Some(true);
                }
                return if success { Ok(()) } else { Err(unavailable()) };
            }
            if Instant::now() >= deadline {
                return Err(unavailable());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        // Explicit failure must remain visible to the retaining supervisor. Do
        // not spend another cleanup deadline silently from its destructor.
        if !self.attempted {
            let _ = self.finish();
        }
    }
}

fn retry(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}
fn broker_main(main: &mut UnixStream, state: &Arc<State>, base: &Arc<Zeroizing<Vec<u8>>>) {
    while !state.closing.load(Ordering::Acquire) {
        let mut request = [0];
        match main.read(&mut request) {
            Err(e) if retry(&e) => continue,
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if request[0] != 1 {
            break;
        }
        let Ok(mut entries) = state.entries.lock() else {
            break;
        };
        // Reap only already completed sessions. Unconfirmed children remain registered.
        entries.retain(|entry| {
            let done = entry.result.lock().is_ok_and(|r| *r == Some(true))
                && entry
                    .thread
                    .lock()
                    .is_ok_and(|t| t.as_ref().is_some_and(JoinHandle::is_finished));
            if done
                && let Ok(mut thread) = entry.thread.lock()
                && let Some(thread) = thread.take()
            {
                let _ = thread.join();
            }
            !done
        });
        if entries.len() >= MAX_SESSIONS || state.closing.load(Ordering::Acquire) {
            break;
        }
        let Ok((server, client)) = UnixStream::pair() else {
            break;
        };
        let Ok(connection) = server.try_clone() else {
            break;
        };
        let entry = Arc::new(Session {
            connection,
            child: Mutex::new(None),
            result: Mutex::new(None),
            thread: Mutex::new(None),
        });
        entries.push(Arc::clone(&entry));
        let worker = Arc::clone(&entry);
        let worker_state = Arc::clone(state);
        let base = Arc::clone(base);
        if let Ok(thread) = std::thread::Builder::new()
            .name("owned-tool".into())
            .spawn(move || session(server, &worker, &worker_state, &base))
        {
            if let Ok(mut slot) = entry.thread.lock() {
                *slot = Some(thread);
            }
        } else {
            if let Ok(mut result) = entry.result.lock() {
                *result = Some(true);
            }
            break;
        }
        drop(entries);
        let client: OwnedFd = client.into();
        if wire::send_rights(main, 1, std::slice::from_ref(&client)).is_err() {
            break;
        }
        let deadline = Instant::now() + IO_TIMEOUT;
        let acknowledged = loop {
            let mut ack = [0];
            match main.read(&mut ack) {
                Ok(1) => break ack[0] == 2,
                Err(error)
                    if retry(&error)
                        && Instant::now() < deadline
                        && !state.closing.load(Ordering::Acquire) => {}
                _ => break false,
            }
        };
        // Explicit ACK is an ownership handoff, not a keepalive until tool exit.
        drop(client);
        if !acknowledged {
            break;
        }
    }
}

fn session(mut stream: UnixStream, entry: &Session, state: &State, base: &[u8]) {
    let mut attempted = None;
    let _operation = (|| -> io::Result<()> {
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut length = [0; 4];
        stream.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if length > wire::MAX_PACKET {
            return Err(wire::invalid());
        }
        let mut bytes = Zeroizing::new(vec![0; length]);
        stream.read_exact(&mut bytes)?;
        let (command, io) = decode_command(&bytes, base)?;
        let mut slot = lock(&entry.child)?;
        // Admission and process birth share this gate with close(): either the
        // child is registered before shutdown's snapshot, or no birth occurs.
        let admission = lock(&state.entries)?;
        if state.closing.load(Ordering::Acquire) {
            return Err(unavailable());
        }
        let mut child = ManagedChild::spawn_with_io_local(command, true, io)?;
        let mut files = Vec::new();
        if let Some(pipe) = child.stdin.take() {
            files.push(pipe.into());
        }
        if let Some(pipe) = child.stdout.take() {
            files.push(pipe.into());
        }
        if let Some(pipe) = child.stderr.take() {
            files.push(pipe.into());
        }
        let pid = child.id();
        *slot = Some(child);
        drop(admission);
        drop(slot);
        wire::send(&mut stream, 0, &pid.to_be_bytes(), &files)?;
        let mut ack = [0];
        stream.read_exact(&mut ack)?;
        if ack != [4] {
            return Err(wire::invalid());
        }
        drop(files);
        stream.set_read_timeout(Some(POLL_TIMEOUT))?;
        loop {
            if state.closing.load(Ordering::Acquire) {
                break;
            }
            let mut kind = [0];
            match stream.read(&mut kind) {
                Ok(0) => break,
                Err(e) if retry(&e) => continue,
                Err(e) => return Err(e),
                Ok(_) => {}
            }
            let mut child = lock(&entry.child)?;
            let child = child.as_mut().ok_or_else(unavailable)?;
            let status = match kind[0] {
                2 => child.try_wait()?,
                3 => {
                    let result = child.finish();
                    attempted = Some(result.is_ok());
                    result?;
                    Some(child.wait()?)
                }
                _ => return Err(wire::invalid()),
            };
            let mut response = [0; 5];
            if let Some(status) = status {
                response[0] = 1;
                response[1..].copy_from_slice(&status.into_raw().to_be_bytes());
            }
            stream.write_all(&response)?;
            if kind[0] == 3 {
                break;
            }
        }
        Ok(())
    })();
    // Even malformed requests or a disappeared CLI cannot discard a spawned owner.
    let confirmed = final_confirmation(attempted, || {
        lock(&entry.child)
            .and_then(|mut child| child.as_mut().map_or(Ok(()), ManagedChild::finish))
            .is_ok()
    });
    if let Ok(mut result) = entry.result.lock() {
        *result = Some(confirmed);
    }
}

fn final_confirmation(attempted: Option<bool>, cleanup: impl FnOnce() -> bool) -> bool {
    // An explicit failed request must not hide a second cleanup deadline. A
    // later broker-level operation may retry the retained owner after joining.
    attempted.unwrap_or_else(cleanup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::path::Path;

    fn connected() -> (Broker, Mutex<UnixStream>) {
        let (broker, endpoint) = Broker::new(&[]).unwrap();
        let mut stream = UnixStream::from(endpoint);
        stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        stream.write_all(HELLO).unwrap();
        let mut ack = [0; 4];
        stream.read_exact(&mut ack).unwrap();
        assert_eq!(&ack, ACK);
        broker.confirm().unwrap();
        (broker, Mutex::new(stream))
    }
    fn captured() -> ManagedIo {
        ManagedIo {
            stdin: ManagedStdio::Null,
            stdout: ManagedStdio::Piped,
            stderr: ManagedStdio::Piped,
        }
    }
    fn fixture(root: &Path) -> std::path::PathBuf {
        drmr_test_support::install(
            &root.join("tool"),
            &drmr_test_support::ToolSpec::default()
                .with(
                    "descendant_pid_file",
                    root.join("descendant.pid").to_str().unwrap(),
                )
                .with(
                    "descendant_lock_file",
                    root.join("held.lock").to_str().unwrap(),
                )
                .with("release_file", root.join("release").to_str().unwrap()),
        )
        .unwrap()
    }
    fn ready(root: &Path, child: &mut ManagedChild) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.join("descendant.pid").is_file() {
            assert!(child.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline, "held child readiness");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(immediate_lock(root).is_err());
    }
    fn immediate_lock(root: &Path) -> io::Result<std::fs::File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("held.lock"))?;
        file.try_lock().map_err(io::Error::other)?;
        Ok(file)
    }
    #[test]
    fn individual_remote_finish_preserves_distinct_group_and_immediate_descendant_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let tool = fixture(root.path());
        let (mut broker, connection) = connected();
        let mut child = Remote::spawn_on(&connection, &Command::new(tool), &captured()).unwrap();
        ready(root.path(), &mut child);
        let pid = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
        assert_eq!(rustix::process::getpgid(Some(pid)).unwrap(), pid);
        assert_ne!(pid, rustix::process::getpgrp());
        child.finish().unwrap();
        let immediate = immediate_lock(root.path());
        immediate.unwrap();
        child.finish().unwrap();
        broker.finish().unwrap();
    }
    #[test]
    fn disappeared_cli_control_retains_tool_until_broker_confirms_all_groups() {
        let root = tempfile::tempdir().unwrap();
        let tool = fixture(root.path());
        let (mut broker, connection) = connected();
        let mut child = Remote::spawn_on(&connection, &Command::new(tool), &captured()).unwrap();
        ready(root.path(), &mut child);
        // The server owns the actual tool even when the CLI's main channel dies.
        lock(&connection)
            .unwrap()
            .shutdown(std::net::Shutdown::Both)
            .unwrap();
        broker.finish().unwrap();
        let immediate = immediate_lock(root.path());
        immediate.unwrap();
        // Remote cleanup cannot turn a lost protocol response into success.
        assert!(child.finish().is_err());
    }
    #[test]
    fn partial_spawn_frame_and_idle_child_channel_do_not_block_broker_shutdown() {
        let (mut broker, connection) = connected();
        let mut main = lock(&connection).unwrap();
        main.write_all(&[1]).unwrap();
        let (kind, mut endpoints) =
            wire::receive_rights(&main, rustix::net::RecvFlags::empty()).unwrap();
        assert_eq!(kind, 1);
        let mut session = UnixStream::from(endpoints.pop().unwrap());
        main.write_all(&[2]).unwrap();
        session.write_all(&100_u32.to_be_bytes()).unwrap();
        session.write_all(b"partial").unwrap();
        let started = Instant::now();
        broker.finish().unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
    }
    #[test]
    fn failed_explicit_cleanup_is_not_repeated_by_session_teardown() {
        let calls = std::cell::Cell::new(0);
        for outcome in [false, true] {
            assert_eq!(
                final_confirmation(Some(outcome), || {
                    calls.set(calls.get() + 1);
                    true
                }),
                outcome
            );
        }
        assert_eq!(calls.get(), 0);
        assert!(!final_confirmation(None, || {
            calls.set(calls.get() + 1);
            false
        }));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn an_explicit_retry_finishes_a_retained_failed_owner_before_releasing_it() {
        let root = tempfile::tempdir().unwrap();
        let tool = fixture(root.path());
        let mut command = Command::new(tool);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = ManagedChild::spawn(command, true).unwrap();
        ready(root.path(), &mut child);
        let (mut broker, _connection) = connected();
        let (session, _peer) = UnixStream::pair().unwrap();
        // Model a completed handler whose explicit cleanup attempt failed. The
        // real native owner must survive the error and be used by the next call.
        let retained = Arc::new(Session {
            connection: session,
            child: Mutex::new(Some(child)),
            result: Mutex::new(Some(false)),
            thread: Mutex::new(None),
        });
        lock(&broker.state.entries).unwrap().push(retained);
        broker.finish().unwrap();
        let immediate = immediate_lock(root.path());
        immediate.unwrap();
        broker.finish().unwrap();
    }

    #[test]
    fn malformed_command_and_remote_file_mode_fail_before_any_child_birth() {
        assert!(decode_command(&[0; 3], &[]).is_err());
        let root = tempfile::tempdir().unwrap();
        let file = std::fs::File::create(root.path().join("record")).unwrap();
        let io = ManagedIo {
            stdin: ManagedStdio::Null,
            stdout: ManagedStdio::File(file),
            stderr: ManagedStdio::Null,
        };
        assert!(encode_command(&Command::new("unavailable"), &io).is_err());
        let (mut broker, _endpoint) = Broker::new(&[]).unwrap();
        broker.finish().unwrap();
    }
    #[test]
    fn fast_parallel_tools_preserve_every_response_and_payload() {
        let root = tempfile::tempdir().unwrap();
        let spec = drmr_test_support::ToolSpec {
            stdout: b"actual".to_vec(),
            ..Default::default()
        };
        let executable = drmr_test_support::install(&root.path().join("quick"), &spec).unwrap();
        let (mut broker, connection) = connected();
        std::thread::scope(|scope| {
            let workers = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        for _ in 0..16 {
                            let mut child = Remote::spawn_on(
                                &connection,
                                &Command::new(&executable),
                                &captured(),
                            )
                            .unwrap();
                            assert!(child.wait().unwrap().success());
                            child.finish().unwrap();
                            let mut data = String::new();
                            child
                                .stdout
                                .take()
                                .unwrap()
                                .read_to_string(&mut data)
                                .unwrap();
                            assert_eq!(data, "actual");
                        }
                    })
                })
                .collect::<Vec<_>>();
            for worker in workers {
                worker.join().unwrap();
            }
        });
        broker.finish().unwrap();
    }

    #[test]
    fn two_remote_children_keep_independent_control_and_cleanup_owners() {
        let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let tools = [fixture(roots[0].path()), fixture(roots[1].path())];
        let (mut broker, connection) = connected();
        let mut a = Remote::spawn_on(&connection, &Command::new(&tools[0]), &captured()).unwrap();
        let mut b = Remote::spawn_on(&connection, &Command::new(&tools[1]), &captured()).unwrap();
        ready(roots[0].path(), &mut a);
        ready(roots[1].path(), &mut b);
        a.finish().unwrap();
        immediate_lock(roots[0].path()).unwrap();
        assert!(immediate_lock(roots[1].path()).is_err());
        b.finish().unwrap();
        immediate_lock(roots[1].path()).unwrap();
        broker.finish().unwrap();
    }
}
