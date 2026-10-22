//! Test-only in-container observer. Never copied into the production runtime image.
use std::{fs, io, path::PathBuf};
#[cfg(any(target_os = "linux", test))]
#[path = "container-qualification/registry.rs"]
mod registry;
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
    held: Vec<HeldInvocation>,
}

#[cfg(target_os = "linux")]
struct HeldInvocation {
    tool: u32,
    descendant: u32,
    group: i32,
    lock: File,
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
        for held in &self.held {
            if matches!(held.lock.try_lock(), Err(fs::TryLockError::WouldBlock)) {
                // Only a validated, still-held invocation lock pins a fallback group.
                let _ = signal(-held.group, "KILL");
            }
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
fn failed(check: &'static str) -> io::Error {
    eprintln!("container-qualification: check={check}");
    fail()
}

#[cfg(target_os = "linux")]
fn registry_records(root: &Path, partial: bool) -> io::Result<Vec<registry::Invocation>> {
    registry::read(root, partial).map_err(|error| failed(error.check))
}

#[cfg(target_os = "linux")]
fn admit_held(
    owner: u32,
    root: &Path,
    record: &registry::Invocation,
) -> io::Result<HeldInvocation> {
    let tool = record.tool;
    let descendant = record.descendant.ok_or_else(|| failed("registry-record"))?;
    let lock = File::open(root.join(tool.to_string()).join("descendant.lock"))?;
    let (parent, group) = identity(tool)?;
    let (descendant_parent, descendant_group) = identity(descendant)?;
    let (_, owner_group) = identity(owner)?;
    for (valid, check) in [
        (parent == owner, "tool-parent"),
        (group == tool, "tool-group"),
        (descendant_parent == tool, "descendant-parent"),
        (descendant_group == group, "descendant-group"),
        (owner_group != group, "owner-group"),
    ] {
        if !valid {
            return Err(failed(check));
        }
    }
    if !matches!(lock.try_lock(), Err(fs::TryLockError::WouldBlock)) {
        return Err(failed("held-lock"));
    }
    // Never retain fallback signal authority before every live identity and the
    // unique held lock have been validated together.
    Ok(HeldInvocation {
        tool,
        descendant,
        group: i32::try_from(group).map_err(|_| failed("tool-group"))?,
        lock,
    })
}

#[cfg(target_os = "linux")]
fn wait_held(owned: &mut Owned, root: &Path, previous: &[registry::Invocation]) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let mut admitted = false;
        for record in registry_records(root, false)? {
            if !record.complete || previous.iter().any(|old| old.tool == record.tool) {
                continue;
            }
            if !owned.held.iter().any(|held| held.tool == record.tool) {
                let held = admit_held(owned.child.id(), root, &record)?;
                owned.held.push(held);
            }
            admitted = true;
        }
        if admitted {
            return Ok(());
        }
        if owned.child.try_wait()?.is_some() {
            return Err(failed("owner-exit"));
        }
        if Instant::now() >= deadline {
            return Err(failed("fixture-deadline"));
        }
        thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
fn install_control(fixture: &Path, control: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let registry = control.join("invocations");
    fs::create_dir(&registry)?;
    fs::set_permissions(&registry, fs::Permissions::from_mode(0o700))?;
    let program = control.join("ffprobe");
    install(
        fixture,
        &program,
        &drmr_test_support::ToolSpec::default()
            .with("invocation_root", registry.to_str().ok_or_else(fail)?)
            .with(
                "release_file",
                control.join("never-release").to_str().ok_or_else(fail)?,
            ),
    )?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn group_absent(tool: u32) -> io::Result<()> {
    let group = rustix::process::Pid::from_raw(i32::try_from(tool).map_err(|_| fail())?)
        .ok_or_else(fail)?;
    require_group_absent(rustix::process::test_kill_process_group(group))
}

#[cfg(target_os = "linux")]
fn require_group_absent(result: Result<(), rustix::io::Errno>) -> io::Result<()> {
    match result {
        Err(rustix::io::Errno::SRCH) => Ok(()),
        Err(error) => Err(error.into()),
        Ok(()) => Err(fail()),
    }
}

#[cfg(target_os = "linux")]
fn assert_gone(
    owned: &Owned,
    root: &Path,
    previous: &[registry::Invocation],
) -> io::Result<Vec<registry::Invocation>> {
    use std::os::unix::fs::MetadataExt as _;
    // The first terminal observation inventories every entry, including arrivals
    // after the ready barrier and registration cut short by Stop. No waits,
    // eventual-absence polling, or log writes precede these observations.
    let records = registry_records(root, true)?;
    let mut failure = None;
    let mut remember = |check, error| {
        if failure.is_none() {
            failure = Some((check, error));
        }
    };
    for old in previous {
        if !records.iter().any(|record| record == old) {
            remember("registry-record", fail());
        }
    }
    for held in &owned.held {
        if !records.iter().any(|record| {
            record.tool == held.tool
                && record.descendant == Some(held.descendant)
                && record.has_lock
                && record.complete
        }) {
            remember("registry-record", fail());
        }
        let current =
            fs::symlink_metadata(root.join(held.tool.to_string()).join("descendant.lock"));
        match (held.lock.metadata(), current) {
            (Ok(original), Ok(current))
                if original.dev() == current.dev() && original.ino() == current.ino() => {}
            _ => remember("registry-record", fail()),
        }
        match held.lock.try_lock() {
            Ok(()) => {
                if let Err(error) = held.lock.unlock() {
                    remember("lock-released", error);
                }
            }
            Err(error) => remember("lock-released", error.into()),
        }
    }
    for record in &records {
        if Path::new(&format!("/proc/{}", record.tool)).exists() {
            remember("tool-absent", fail());
        }
        if let Some(descendant) = record.descendant
            && Path::new(&format!("/proc/{descendant}")).exists()
        {
            remember("descendant-absent", fail());
        }
        // Even an empty, partial entry identifies the independently created tool
        // group. ESRCH also covers a child killed before publishing its own PID.
        if let Err(error) = group_absent(record.tool) {
            remember("group-absent", error);
        }
        if record.has_lock && !owned.held.iter().any(|held| held.tool == record.tool) {
            match File::open(root.join(record.tool.to_string()).join("descendant.lock")) {
                Ok(lock) => match lock.try_lock() {
                    Ok(()) => {
                        if let Err(error) = lock.unlock() {
                            remember("lock-released", error);
                        }
                    }
                    Err(error) => remember("lock-released", error.into()),
                },
                Err(error) => remember("lock-released", error),
            }
        }
    }
    if records.is_empty() || owned.held.is_empty() {
        remember("registry-record", fail());
    }
    if let Some((check, error)) = failure {
        eprintln!("container-qualification: check={check}");
        return Err(error);
    }
    Ok(records)
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
        held: Vec::new(),
    };
    eprintln!("container-qualification: phase=held-stop-barrier");
    let registry = control.join("invocations");
    wait_held(&mut owned, &registry, &[])?;
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
    let _ = assert_gone(&owned, &registry, &[])?;
    eprintln!("container-qualification: phase=stop-return-state");
    if status.code() != Some(130) || output.exists() {
        return Err(fail());
    }
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
        "GET /api/common/jobs/{id} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
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
fn report_job_state(port: u16, id: &str) {
    let state = job_state(port, id).ok();
    let value = match state.as_deref() {
        Some("running") => "running",
        Some("interrupted") => "interrupted",
        Some("failed") => "failed",
        Some("succeeded") => "succeeded",
        _ => "unavailable",
    };
    eprintln!("container-qualification: job-state={value}");
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
                    return Err(failed("loopback-listener"));
                }
                matched += 1;
            }
        }
    }
    if matched != 1 {
        return Err(failed("loopback-listener"));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn web(cli: &Path, fixture: &Path, work: &Path, port: u16) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let control = work.join("web-control");
    for path in [&control, &work.join("web-input"), &work.join("web-output")] {
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
        .env("DRMR_INPUT_ROOT", work.join("web-input"))
        .env("DRMR_OUTPUT_ROOT", work.join("web-output"))
        .env("DRMR_COMMON_CONFIG_ROOT", work.join("web-config"))
        .env("DRMR_BIND_PORT", port.to_string())
        .env("PATH", path)
        .stdin(Stdio::null())
        .stdout(File::create(control.join("server.stdout"))?)
        .stderr(File::create(control.join("server.stderr"))?)
        .spawn()?;
    let mut owned = Owned {
        child,
        held: Vec::new(),
    };
    eprintln!("container-qualification: phase=web-await-stop-job");
    let id = wait_job(&control, "stop-job", &mut owned)?;
    eprintln!("container-qualification: phase=web-check-stop-listener");
    assert_loopback(owned.child.id(), port)?;
    let registry = control.join("invocations");
    eprintln!("container-qualification: phase=web-await-stop-fixture");
    wait_held(&mut owned, &registry, &[]).inspect_err(|_| {
        report_job_state(port, &id);
    })?;
    fs::write(control.join("stop-ready"), b"ready")?;
    eprintln!("container-qualification: phase=web-await-stop-terminal");
    let deadline = Instant::now() + Duration::from_secs(10);
    let stopped = loop {
        match job_state(port, &id)?.as_str() {
            "interrupted" => {
                let records = assert_gone(&owned, &registry, &[])?;
                eprintln!("container-qualification: phase=web-check-stop-return");
                break records;
            }
            "running" if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => {
                report_job_state(port, &id);
                return Err(failed("job-state"));
            }
        }
    };
    fs::write(control.join("stop-confirmed"), b"confirmed")?;
    eprintln!("container-qualification: phase=web-await-shutdown-job");
    let id = wait_job(&control, "shutdown-job", &mut owned)?;
    eprintln!("container-qualification: phase=web-await-shutdown-fixture");
    wait_held(&mut owned, &registry, &stopped).inspect_err(|_| {
        report_job_state(port, &id);
    })?;
    fs::write(control.join("shutdown-ready"), b"ready")?;
    eprintln!("container-qualification: phase=web-await-shutdown-exit");
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
    let _ = assert_gone(&owned, &registry, &stopped)?;
    eprintln!("container-qualification: phase=web-check-shutdown-return");
    eprintln!("container-qualification: phase=web-shutdown-return-state");
    if !status.success() || TcpStream::connect(("127.0.0.1", port)).is_ok() {
        return Err(fail());
    }
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt as _;

    fn sleeping() -> Owned {
        Owned {
            child: Command::new("/bin/sh")
                .args(["-c", "read value"])
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
            held: Vec::new(),
        }
    }

    fn exited() -> u32 {
        let mut child = Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id();
        assert!(child.wait().unwrap().success());
        pid
    }

    fn baseline(root: &Path) -> Owned {
        let mut owner = sleeping();
        let tool = exited();
        let descendant = exited();
        let entry = root.join(tool.to_string());
        fs::create_dir(&entry).unwrap();
        fs::write(entry.join("descendant.pid"), format!("{descendant}\n")).unwrap();
        fs::write(entry.join("descendant.lock"), b"").unwrap();
        fs::write(entry.join("descendant.lock.ready"), b"ready\n").unwrap();
        fs::write(entry.join("complete"), b"ready\n").unwrap();
        owner.held.push(HeldInvocation {
            tool,
            descendant,
            group: i32::try_from(tool).unwrap(),
            lock: File::open(entry.join("descendant.lock")).unwrap(),
        });
        owner
    }

    #[test]
    fn only_esrch_proves_that_an_owned_group_is_absent() {
        assert!(require_group_absent(Err(rustix::io::Errno::SRCH)).is_ok());
        for result in [
            Ok(()),
            Err(rustix::io::Errno::PERM),
            Err(rustix::io::Errno::INVAL),
            Err(rustix::io::Errno::IO),
        ] {
            assert!(require_group_absent(result).is_err());
        }
    }

    #[test]
    fn a_partial_invocation_started_after_the_barrier_is_checked_at_first_return() {
        let root = tempfile::tempdir().unwrap();
        let owner = baseline(root.path());
        let prior = assert_gone(&owner, root.path(), &[]).unwrap();

        // This arrival is deliberately later than the admitted ready snapshot.
        // It is not complete and must still prevent a successful terminal check.
        let mut late = sleeping();
        fs::create_dir(root.path().join(late.child.id().to_string())).unwrap();
        assert!(assert_gone(&owner, root.path(), &prior).is_err());
        late.child.kill().unwrap();
        late.child.wait().unwrap();
        let records = assert_gone(&owner, root.path(), &prior).unwrap();
        assert_eq!(records.len(), 2);
        assert!(records.iter().any(|record| !record.complete));
    }

    #[test]
    fn an_unreported_descendant_cannot_hide_behind_its_exited_tool() {
        let root = tempfile::tempdir().unwrap();
        let owner = baseline(root.path());
        let prior = assert_gone(&owner, root.path(), &[]).unwrap();
        let controls = tempfile::tempdir().unwrap();
        let pid_path = controls.path().join("child.pid");
        let lock_path = controls.path().join("child.lock");
        let release = controls.path().join("release");
        let tool = drmr_test_support::install(
            &controls.path().join("worker"),
            &drmr_test_support::ToolSpec::default()
                .with("descendant_pid_file", pid_path.to_str().unwrap())
                .with("descendant_lock_file", lock_path.to_str().unwrap())
                .with("release_file", release.to_str().unwrap()),
        )
        .unwrap();
        let mut late = Owned {
            child: Command::new(tool)
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
            held: Vec::new(),
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        let descendant = loop {
            if let Ok(value) = fs::read_to_string(&pid_path)
                && let Some(value) = value.strip_suffix('\n')
                && let Ok(pid) = value.parse::<u32>()
                && pid > 1
            {
                break pid;
            }
            assert!(late.child.try_wait().unwrap().is_none());
            assert!(Instant::now() < deadline);
            thread::yield_now();
        };
        let tool = late.child.id();
        let (parent, group) = identity(tool).unwrap();
        assert_eq!((parent, group), (std::process::id(), tool));
        assert_eq!(identity(descendant).unwrap(), (tool, tool));
        let lock = File::open(lock_path).unwrap();
        assert!(matches!(lock.try_lock(), Err(fs::TryLockError::WouldBlock)));
        late.held.push(HeldInvocation {
            tool,
            descendant,
            group: i32::try_from(tool).unwrap(),
            lock,
        });
        fs::create_dir(root.path().join(tool.to_string())).unwrap();
        fs::write(release, b"release").unwrap();
        assert!(late.child.wait().unwrap().success());
        assert!(!Path::new(&format!("/proc/{tool}")).exists());
        assert!(Path::new(&format!("/proc/{descendant}")).exists());
        // The registry has no descendant PID, but the tool group remains live.
        assert!(assert_gone(&owner, root.path(), &prior).is_err());
    }
}
