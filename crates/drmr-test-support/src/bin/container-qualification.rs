//! Test-only in-container observer. Never copied into the production runtime image.
use std::{fs, io, path::PathBuf};
#[cfg(target_os = "linux")]
use std::{
    fs::File,
    io::{Read as _, Write as _},
    net::TcpStream,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn fail() -> io::Error {
    io::Error::other("container-lifecycle-unconfirmed")
}

#[cfg(target_os = "linux")]
fn install(fixture: &Path, target: &Path, spec: &drmr_test_support::ToolSpec) -> io::Result<()> {
    let mut child = Command::new(fixture)
        .arg("--install")
        .arg(target)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let written = spec.encode(child.stdin.take().ok_or_else(fail)?);
    let status = child.wait()?;
    written?;
    if !status.success() {
        return Err(fail());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn signal(pid: i32, value: &str) -> io::Result<()> {
    if pid == 0 || pid == 1 || pid == -1 || !matches!(value, "TERM" | "KILL") {
        return Err(fail());
    }
    let status = Command::new("/bin/sh")
        .args([
            "-c",
            "kill -s \"$1\" \"$2\"",
            "fixture",
            value,
            &pid.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(fail())
    }
}

#[cfg(target_os = "linux")]
fn identity(pid: u32) -> io::Result<(u32, u32)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let (_, fields) = stat.rsplit_once(") ").ok_or_else(fail)?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    // After comm: state, ppid, pgrp. Read only explicitly owned process IDs.
    Ok((
        fields
            .get(1)
            .ok_or_else(fail)?
            .parse()
            .map_err(|_| fail())?,
        fields
            .get(2)
            .ok_or_else(fail)?
            .parse()
            .map_err(|_| fail())?,
    ))
}

#[cfg(target_os = "linux")]
struct Owned {
    child: Child,
    lock: Option<File>,
    group: Option<i32>,
}

#[cfg(target_os = "linux")]
impl Drop for Owned {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = signal(i32::try_from(self.child.id()).unwrap_or(0), "TERM");
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
        }
        let _ = self.child.kill();
        let status = self.child.wait();
        if let (Some(lock), Some(group)) = (&self.lock, self.group)
            && matches!(lock.try_lock(), Err(fs::TryLockError::WouldBlock))
        {
            // The original still-held fixture lock pins this group's identity.
            let _ = signal(-group, "KILL");
        }
        if let Ok(status) = status {
            use std::os::unix::process::ExitStatusExt;
            let _ = writeln!(
                io::stderr().lock(),
                "container-qualification: owner-exit code={:?} signal={:?}",
                status.code(),
                status.signal()
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn pid_file(path: &Path) -> Option<u32> {
    let value = fs::read_to_string(path).ok()?;
    value.ends_with('\n').then_some(())?;
    value.trim().parse().ok().filter(|pid| *pid > 1)
}

#[cfg(target_os = "linux")]
fn wait_held(owned: &mut Owned, control: &Path) -> io::Result<(u32, u32)> {
    let tool_pid = control.join("tool.pid");
    let descendant_pid = control.join("descendant.pid");
    let lock_path = control.join("descendant.lock");
    let deadline = Instant::now() + Duration::from_secs(10);
    let (tool, descendant) = loop {
        if let (Some(tool), Some(descendant)) = (pid_file(&tool_pid), pid_file(&descendant_pid)) {
            break (tool, descendant);
        }
        if owned.child.try_wait()?.is_some() || Instant::now() >= deadline {
            return Err(fail());
        }
        thread::sleep(Duration::from_millis(5));
    };
    let lock = File::open(&lock_path)?;
    let (parent, group) = identity(tool)?;
    let (_, descendant_group) = identity(descendant)?;
    let (_, cli_group) = identity(owned.child.id())?;
    owned.group = Some(i32::try_from(group).map_err(|_| fail())?);
    owned.lock = Some(lock);
    if parent != owned.child.id()
        || group != tool
        || descendant_group != group
        || cli_group == group
        || !matches!(
            owned.lock.as_ref().ok_or_else(fail)?.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        )
    {
        return Err(fail());
    }
    Ok((tool, descendant))
}

#[cfg(target_os = "linux")]
fn install_control(fixture: &Path, control: &Path) -> io::Result<()> {
    let program = control.join("ffprobe");
    install(
        fixture,
        &program,
        &drmr_test_support::ToolSpec::default()
            .with(
                "pid_file",
                control.join("tool.pid").to_str().ok_or_else(fail)?,
            )
            .with(
                "descendant_pid_file",
                control.join("descendant.pid").to_str().ok_or_else(fail)?,
            )
            .with(
                "descendant_lock_file",
                control.join("descendant.lock").to_str().ok_or_else(fail)?,
            )
            .with(
                "release_file",
                control.join("never-release").to_str().ok_or_else(fail)?,
            ),
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn assert_gone(owned: &Owned, tool: u32, descendant: u32) -> io::Result<()> {
    // First observations after terminal state/return, before file/log reads. No eventual polling.
    let released = owned.lock.as_ref().ok_or_else(fail)?.try_lock();
    let tool_absent = !Path::new(&format!("/proc/{tool}")).exists();
    let descendant_absent = !Path::new(&format!("/proc/{descendant}")).exists();
    if released.is_err() || !tool_absent || !descendant_absent {
        return Err(fail());
    }
    owned.lock.as_ref().ok_or_else(fail)?.unlock()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn stop(cli: &Path, fixture: &Path, source: &Path, work: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let control = work.join("stop-control");
    fs::create_dir(&control)?;
    fs::set_permissions(&control, fs::Permissions::from_mode(0o700))?;
    eprintln!("container-qualification: phase=install-stop-fixture");
    install_control(fixture, &control)?;
    let output = work.join("cancelled-output");
    let stdout_path = control.join("stdout");
    let stderr_path = control.join("stderr");
    let child = Command::new(cli)
        .arg("convert")
        .arg(source)
        .args(["--to", "clear", "--format", "dash", "--output"])
        .arg(&output)
        .args(["--json", "--max-workers", "1"])
        .env("PATH", &control)
        .stdin(Stdio::null())
        .stdout(File::create(&stdout_path)?)
        .stderr(File::create(&stderr_path)?)
        .spawn()?;
    let mut owned = Owned {
        child,
        lock: None,
        group: None,
    };
    eprintln!("container-qualification: phase=held-stop-barrier");
    let (tool, descendant) = wait_held(&mut owned, &control)?;
    signal(i32::try_from(owned.child.id()).map_err(|_| fail())?, "TERM")?;
    let began = Instant::now();
    let status = loop {
        if let Some(status) = owned.child.try_wait()? {
            break status;
        }
        if began.elapsed() >= Duration::from_secs(5) {
            return Err(fail());
        }
        thread::sleep(Duration::from_millis(5));
    };
    assert_gone(&owned, tool, descendant)?;
    eprintln!("container-qualification: phase=stop-return-state");
    if status.code() != Some(130) || output.exists() {
        return Err(fail());
    }
    owned.group = None;
    for path in [&stdout_path, &stderr_path] {
        if path.metadata()?.len() > 1024 * 1024 {
            return Err(fail());
        }
    }
    println!(
        "{{\"schema_version\":1,\"success\":true,\"kind\":\"sigterm-cli-return\",\"driver_still_alive\":true,\"independent_tool_group\":true,\"immediate_lock_released\":true,\"tool_absent\":true,\"descendant_absent\":true,\"exit_code\":130,\"elapsed_milliseconds\":{}}}",
        began.elapsed().as_millis()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
fn job_state(port: u16, id: &str) -> io::Result<String> {
    if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(fail());
    }
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    write!(
        stream,
        "GET /api/jobs/{id} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )?;
    let mut bytes = Vec::new();
    stream.take(65_537).read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 || !bytes.starts_with(b"HTTP/1.1 200 ") {
        return Err(fail());
    }
    // The fixture uses this server's fixed status endpoint, not media-supplied JSON.
    let text = String::from_utf8(bytes).map_err(|_| fail())?;
    let (_, body) = text.split_once("\r\n\r\n").ok_or_else(fail)?;
    let compact: String = body.split_whitespace().collect();
    for state in ["running", "interrupted", "failed", "succeeded"] {
        if compact.contains(&format!("\"state\":\"{state}\"")) {
            return Ok(state.into());
        }
    }
    Err(fail())
}

#[cfg(target_os = "linux")]
fn wait_job(control: &Path, name: &str, owner: &mut Owned) -> io::Result<String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(value) = fs::read_to_string(control.join(name)) {
            let id = value.trim();
            if id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Ok(id.into());
            }
            return Err(fail());
        }
        if owner.child.try_wait()?.is_some() || Instant::now() >= deadline {
            return Err(fail());
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
fn assert_loopback(pid: u32, port: u16) -> io::Result<()> {
    let descriptors = fs::read_dir(format!("/proc/{pid}/fd"))?;
    let sockets: Vec<_> = descriptors
        .take(512)
        .filter_map(Result::ok)
        .filter_map(|entry| fs::read_link(entry.path()).ok())
        .filter_map(|path| path.to_str().map(str::to_owned))
        .filter_map(|value| {
            value
                .strip_prefix("socket:[")?
                .strip_suffix(']')
                .map(str::to_owned)
        })
        .collect();
    let expected = format!("0100007F:{port:04X}");
    let mut matched = 0;
    for name in ["tcp", "tcp6"] {
        let contents = fs::read_to_string(format!("/proc/{pid}/net/{name}"))?;
        for row in contents.lines().skip(1) {
            let fields: Vec<_> = row.split_whitespace().collect();
            if fields.len() > 9
                && fields[3] == "0A"
                && sockets.iter().any(|inode| inode == fields[9])
                && fields[1].ends_with(&format!(":{port:04X}"))
            {
                if fields[1] != expected {
                    return Err(fail());
                }
                matched += 1;
            }
        }
    }
    if matched != 1 {
        return Err(fail());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn web(cli: &Path, fixture: &Path, work: &Path, port: u16) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let control = work.join("web-control");
    for path in [
        &control,
        &work.join("web-catalog"),
        &work.join("web-output"),
    ] {
        fs::create_dir(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    install_control(fixture, &control)?;
    let path = std::env::join_paths([
        control.clone(),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ])
    .map_err(|_| fail())?;
    let child = Command::new(cli)
        .arg("serve")
        .env("DRMR_CATALOG_ROOT", work.join("web-catalog"))
        .env("DRMR_OUTPUT_ROOT", work.join("web-output"))
        .env("DRMR_HLS_PROFILES", work.join("web-profiles.json"))
        .env("DRMR_BIND_PORT", port.to_string())
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(File::create(control.join("server.stdout"))?)
        .stderr(File::create(control.join("server.stderr"))?)
        .spawn()?;
    let mut owned = Owned {
        child,
        lock: None,
        group: None,
    };
    eprintln!("container-qualification: phase=web-await-stop-job");
    let id = wait_job(&control, "stop-job", &mut owned)?;
    assert_loopback(owned.child.id(), port)?;
    let (tool, descendant) = wait_held(&mut owned, &control)?;
    fs::write(control.join("stop-ready"), b"ready")?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match job_state(port, &id)?.as_str() {
            "interrupted" => {
                assert_gone(&owned, tool, descendant)?;
                break;
            }
            "running" if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => return Err(fail()),
        }
    }
    owned.group = None;
    owned.lock = None;
    for name in [
        "tool.pid",
        "descendant.pid",
        "descendant.lock",
        "descendant.lock.ready",
    ] {
        let _ = fs::remove_file(control.join(name));
    }
    fs::write(control.join("stop-confirmed"), b"confirmed")?;
    eprintln!("container-qualification: phase=web-await-shutdown-job");
    let _ = wait_job(&control, "shutdown-job", &mut owned)?;
    let (tool, descendant) = wait_held(&mut owned, &control)?;
    fs::write(control.join("shutdown-ready"), b"ready")?;
    signal(i32::try_from(owned.child.id()).map_err(|_| fail())?, "TERM")?;
    let began = Instant::now();
    let status = loop {
        if let Some(status) = owned.child.try_wait()? {
            break status;
        }
        if began.elapsed() >= Duration::from_secs(10) {
            return Err(fail());
        }
        thread::sleep(Duration::from_millis(5));
    };
    assert_gone(&owned, tool, descendant)?;
    eprintln!("container-qualification: phase=web-shutdown-return-state");
    if !status.success() || TcpStream::connect(("127.0.0.1", port)).is_ok() {
        return Err(fail());
    }
    owned.group = None;
    println!(
        "{{\"schema_version\":1,\"success\":true,\"kind\":\"web-stop-and-orderly-shutdown\",\"driver_still_alive\":true,\"immediate_lock_released\":true,\"tool_absent\":true,\"descendant_absent\":true,\"listener_released\":true,\"elapsed_milliseconds\":{}}}",
        began.elapsed().as_millis()
    );
    Ok(())
}

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let result = match args.as_slice() {
        [mode, destination] if mode == "export-fixture" => {
            let destination = PathBuf::from(destination);
            if destination.exists() {
                Err(fail())
            } else {
                fs::copy(env!("DRMR_NATIVE_FIXTURE"), destination).map(|_| ())
            }
        }
        #[cfg(target_os = "linux")]
        [mode, cli, fixture, source, work] if mode == "stop" => stop(
            Path::new(cli),
            Path::new(fixture),
            Path::new(source),
            Path::new(work),
        ),
        #[cfg(target_os = "linux")]
        [mode, cli, fixture, work, port] if mode == "web" => port
            .to_str()
            .and_then(|value| value.parse().ok())
            .ok_or_else(fail)
            .and_then(|port| web(Path::new(cli), Path::new(fixture), Path::new(work), port)),
        _ => Err(fail()),
    };
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!(
                "container-qualification: lifecycle-unconfirmed kind={:?} errno={:?}",
                error.kind(),
                error.raw_os_error()
            );
            std::process::ExitCode::from(1)
        }
    }
}
