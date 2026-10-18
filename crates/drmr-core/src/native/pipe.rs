//! Readers for exclusively owned captured output, configured before threads start.
//!
//! Production callers transfer the untouched output of `ManagedChild`'s standard
//! `Stdio::piped` creation path. No other reader/duplicate may consume those bytes.
//! Windows readiness relies on std's overlapped parent handles; synthetic
//! synchronous `io::pipe` tests do not establish that production-handle property.

#[cfg(windows)]
pub(crate) type CapturedStdout = process_wrap::std::NonblockingPipe;
#[cfg(unix)]
pub(crate) type CapturedStdout = std::process::ChildStdout;
#[cfg(windows)]
pub(crate) type CapturedStderr = process_wrap::std::NonblockingPipe;
#[cfg(unix)]
pub(crate) type CapturedStderr = std::process::ChildStderr;

#[cfg(unix)]
fn nonblocking(pipe: &impl std::os::fd::AsFd) -> std::io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(pipe)?;
    rustix::fs::fcntl_setfl(pipe, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}

pub(crate) fn prepare_stdout(pipe: std::process::ChildStdout) -> std::io::Result<CapturedStdout> {
    #[cfg(unix)]
    {
        nonblocking(&pipe)?;
        Ok(pipe)
    }
    #[cfg(windows)]
    {
        process_wrap::std::NonblockingPipe::stdout(pipe)
    }
}

pub(crate) fn prepare_stderr(pipe: std::process::ChildStderr) -> std::io::Result<CapturedStderr> {
    #[cfg(unix)]
    {
        nonblocking(&pipe)?;
        Ok(pipe)
    }
    #[cfg(windows)]
    {
        process_wrap::std::NonblockingPipe::stderr(pipe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    #[cfg(unix)]
    use std::io::Write as _;

    #[cfg(unix)]
    type PipeHandle = std::os::fd::OwnedFd;
    #[cfg(windows)]
    type PipeHandle = std::os::windows::io::OwnedHandle;

    fn isolated(name: &str) -> bool {
        const MARKER: &str = "DRMR_TEST_PIPE_MODE_CHILD";
        if std::env::var_os(MARKER).as_deref() == Some(std::ffi::OsStr::new(name)) {
            return false;
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", name, "--nocapture"])
            .env(MARKER, name)
            .stdin(std::process::Stdio::null());
        let mut child = crate::native::ManagedChild::spawn(command, true).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break Some(status);
            }
            if std::time::Instant::now() >= deadline {
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        };
        child.finish().unwrap();
        assert!(
            status.is_some_and(|status| status.success()),
            "pipe-mode harness failed or blocked"
        );
        true
    }

    #[cfg(windows)]
    #[test]
    fn regular_files_are_rejected_before_a_pipe_reader_can_start() {
        let file = tempfile::tempfile().unwrap();
        let pipe = std::process::ChildStdout::from(PipeHandle::from(file));
        assert!(prepare_stdout(pipe).is_err());
    }

    #[test]
    fn actual_managed_stdio_distinguishes_empty_data_and_closed_writers() {
        if isolated(
            "native::pipe::tests::actual_managed_stdio_distinguishes_empty_data_and_closed_writers",
        ) {
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let ready = root.path().join("ready");
        let release = root.path().join("release");
        let output = b"exact\0binary\xff";
        let error_output = b"stderr bytes";
        let spec = drmr_test_support::ToolSpec {
            stdout: output.to_vec(),
            stderr: error_output.to_vec(),
            ..Default::default()
        }
        .with("ready_file", ready.to_str().unwrap())
        .with("release_file", release.to_str().unwrap());
        let program = drmr_test_support::install(&root.path().join("response"), &spec).unwrap();
        let mut command = std::process::Command::new(program);
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = crate::native::ManagedChild::spawn(command, true).unwrap();
        let mut stdout = prepare_stdout(child.stdout.take().unwrap()).unwrap();
        let mut stderr = prepare_stderr(child.stderr.take().unwrap()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !ready.is_file() {
            assert!(
                std::time::Instant::now() < deadline,
                "stdio fixture did not become ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let mut buffer = [0; 3];
        assert_eq!(
            stdout.read(&mut buffer).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        assert_eq!(
            stderr.read(&mut buffer).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        std::fs::write(release, b"release").unwrap();
        let mut actual = [Vec::new(), Vec::new()];
        let mut done = [false; 2];
        while !done.iter().all(|done| *done) {
            for (index, pipe) in [
                &mut stdout as &mut dyn std::io::Read,
                &mut stderr as &mut dyn std::io::Read,
            ]
            .into_iter()
            .enumerate()
            {
                match pipe.read(&mut buffer) {
                    Ok(0) => done[index] = true,
                    Ok(count) => actual[index].extend_from_slice(&buffer[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("stdio read failed: {}", error.kind()),
                }
            }
            assert!(std::time::Instant::now() < deadline, "stdio did not close");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        child.finish().unwrap();
        assert_eq!(actual, [output.to_vec(), error_output.to_vec()]);
    }

    // std creates synchronous anonymous pipes. Keep a writer in this process:
    // process-tree cleanup cannot close it and therefore cannot fake EOF.
    #[cfg(unix)]
    #[test]
    fn empty_output_pipe_is_not_eof_and_retains_every_later_byte() {
        if isolated(
            "native::pipe::tests::empty_output_pipe_is_not_eof_and_retains_every_later_byte",
        ) {
            return;
        }
        let (read, mut write) = std::io::pipe().unwrap();
        let stdout = std::process::ChildStdout::from(PipeHandle::from(read));
        let mut pipe = prepare_stdout(stdout).unwrap();
        let mut bytes = [0; 32];
        assert_eq!(
            pipe.read(&mut bytes).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        write.write_all(b"exact\0binary\xff").unwrap();
        assert_eq!(pipe.read(&mut bytes).unwrap(), 13);
        assert_eq!(&bytes[..13], b"exact\0binary\xff");
        assert_eq!(
            pipe.read(&mut bytes).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(write);
        assert_eq!(pipe.read(&mut bytes).unwrap(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn empty_stderr_pipe_is_not_eof() {
        if isolated("native::pipe::tests::empty_stderr_pipe_is_not_eof") {
            return;
        }
        let (read, mut write) = std::io::pipe().unwrap();
        let stderr = std::process::ChildStderr::from(PipeHandle::from(read));
        let mut pipe = prepare_stderr(stderr).unwrap();
        let mut bytes = [0; 32];
        assert_eq!(
            pipe.read(&mut bytes).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
        write.write_all(b"stderr bytes").unwrap();
        assert_eq!(pipe.read(&mut bytes).unwrap(), 12);
        assert_eq!(&bytes[..12], b"stderr bytes");
        drop(write);
        assert_eq!(pipe.read(&mut bytes).unwrap(), 0);
    }
}
