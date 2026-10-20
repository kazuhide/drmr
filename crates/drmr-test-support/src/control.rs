use crate::protocol::ToolSpec;
use std::{
    env, fs, io,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Descendants use the same immutable executable and inherit the supervised process tree.
pub fn child_entry(args: &[std::ffi::OsString]) -> io::Result<bool> {
    if args.len() == 1 && args[0] == "--fixture-sleep" {
        thread::sleep(Duration::from_secs(60));
        return Ok(true);
    }
    if args.len() == 4 && args[0] == "--fixture-invocation-lock" {
        // Publish the child's identity before it can hold a lock. If Stop cuts
        // registration short, the observer also checks the complete tool group.
        publish_new(Path::new(&args[3]), format!("{}\n", std::process::id()).as_bytes())?;
        let held = private_new(Path::new(&args[1]))?;
        held.lock()?;
        publish_new(Path::new(&args[2]), b"ready\n")?;
        thread::sleep(Duration::from_secs(60));
        drop(held);
        return Ok(true);
    }
    if matches!(args.len(), 3 | 5) && args[0] == "--fixture-lock" {
        let ordinary = if args.len() == 5 {
            let requested = args[3]
                .to_str()
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|count| *count <= 4096)
                .ok_or_else(|| io::Error::other("invalid descriptor fixture bound"))?;
            ordinary_handles(requested)?
        } else {
            Vec::new()
        };
        let mut options = fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            options.share_mode(0);
        }
        let held = options.open(&args[1])?;
        #[cfg(unix)]
        held.lock()?;
        if args.len() == 5 {
            fs::write(&args[4], ordinary.len().to_string())?;
        }
        fs::write(&args[2], b"ready")?;
        thread::sleep(Duration::from_secs(60));
        drop(held);
        return Ok(true);
    }
    Ok(false)
}

fn private_new(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

fn publish_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use io::Write as _;
    let mut pending = path.as_os_str().to_owned();
    pending.push(".pending");
    let pending = std::path::PathBuf::from(pending);
    let mut file = private_new(&pending)?;
    file.write_all(bytes)?;
    drop(file);
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::rename(pending, path),
        _ => Err(io::Error::other("fixture publication already exists")),
    }
}

fn register_invocation(spec: &ToolSpec, root: &Path) -> io::Result<()> {
    if [
        "pid_file",
        "descendant_pid_file",
        "descendant_lock_file",
        "ready_file",
        "descendant_detached_console",
        "descendant_open_files",
    ]
    .iter()
    .any(|key| spec.values.contains_key(*key))
        || !fs::symlink_metadata(root)?.file_type().is_dir()
    {
        return Err(io::Error::other("invalid invocation fixture configuration"));
    }
    let pid = std::process::id();
    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string("/proc/self/stat")?;
        let group = stat
            .rsplit_once(") ")
            .and_then(|(_, fields)| fields.split_whitespace().nth(2))
            .and_then(|value| value.parse::<u32>().ok());
        if group != Some(pid) || pid <= 1 {
            return Err(io::Error::other("fixture requires an independent tool group"));
        }
    }
    // mkdir atomically records the tool/group identity before any descendant is
    // spawned. Records remain available across both Web lifecycle jobs.
    let entry = root.join(pid.to_string());
    fs::create_dir(&entry)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&entry, fs::Permissions::from_mode(0o700))?;
    }
    let child_pid = entry.join("descendant.pid");
    let ready = entry.join("descendant.lock.ready");
    let mut child = Command::new(env::current_exe()?)
        .arg("--fixture-invocation-lock")
        .arg(entry.join("descendant.lock"))
        .arg(&ready)
        .arg(&child_pid)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let result = (|| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.is_file() {
            if child.try_wait()?.is_some() || Instant::now() >= deadline {
                return Err(io::Error::other("fixture descendant did not become ready"));
            }
            thread::sleep(Duration::from_millis(10));
        }
        if fs::read(&child_pid)? != format!("{}\n", child.id()).as_bytes() {
            return Err(io::Error::other("fixture descendant identity changed"));
        }
        publish_new(&entry.join("complete"), b"ready\n")
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    // On success the production supervisor owns the inherited process group.
    result
}

#[cfg(unix)]
fn ordinary_handles(requested: usize) -> io::Result<Vec<fs::File>> {
    (0..requested)
        .map(|_| fs::File::open("/dev/null"))
        .collect()
}

#[cfg(not(unix))]
fn ordinary_handles(requested: usize) -> io::Result<Vec<fs::File>> {
    if requested != 0 {
        return Err(io::Error::other(
            "descriptor teardown fixture requires Unix",
        ));
    }
    Ok(Vec::new())
}

pub fn spawn_descendant(pidfile: &Path, lock_file: Option<&Path>) -> io::Result<()> {
    spawn_descendant_with_console(pidfile, lock_file, false, None)
}

fn spawn_descendant_with_console(
    pidfile: &Path,
    lock_file: Option<&Path>,
    detached_console: bool,
    descriptors: Option<(&str, &Path)>,
) -> io::Result<()> {
    let mut command = Command::new(env::current_exe()?);
    let ready = lock_file.map(|path| {
        let mut name = path.as_os_str().to_owned();
        name.push(".ready");
        std::path::PathBuf::from(name)
    });
    if let (Some(lock), Some(ready)) = (lock_file, &ready) {
        command.arg("--fixture-lock").arg(lock).arg(ready);
        if let Some((count, report)) = descriptors {
            command.arg(count).arg(report);
        }
    } else {
        command.arg("--fixture-sleep");
    }
    #[cfg(windows)]
    if detached_console {
        use std::os::windows::process::CommandExt as _;
        // This changes console membership only. The child still inherits its job.
        command.creation_flags(0x0000_0008); // DETACHED_PROCESS
    }
    #[cfg(not(windows))]
    if detached_console {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "detached console fixture requires Windows",
        ));
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(ready) = ready {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.is_file() {
            if child.try_wait()?.is_some() || Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::other("fixture descendant did not become ready"));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    fs::write(pidfile, format!("{}\n", child.id()))?;
    // Dropping a Child leaves it running; the production supervisor must terminate it.
    drop(child);
    Ok(())
}

pub fn before_response(spec: &ToolSpec) -> io::Result<()> {
    if let Some(root) = spec.values.get("invocation_root") {
        register_invocation(spec, Path::new(root))?;
    }
    if let Some(path) = spec.values.get("pid_file") {
        fs::write(path, format!("{}\n", std::process::id()))?;
    }
    if let Some(path) = spec.values.get("stdin_file") {
        io::copy(&mut io::stdin(), &mut super::create(Path::new(path))?)?;
    } else if spec.values.get("drain_stdin").is_some_and(|s| s == "1") {
        io::copy(&mut io::stdin(), &mut io::sink())?;
    }
    if let Some(path) = spec.values.get("drm_env_file") {
        let values: Vec<_> = ["DRM_KID", "DRM_KEY", "DRM_IV"]
            .into_iter()
            .map(|key| env::var(key).unwrap_or_default())
            .collect();
        fs::write(path, values.join(" "))?;
    }
    if let Some(path) = spec.values.get("descendant_pid_file") {
        let descriptors = spec
            .values
            .get("descendant_open_files")
            .map(|count| {
                spec.values
                    .get("descendant_descriptor_count_file")
                    .map(|report| (count.as_str(), Path::new(report)))
                    .ok_or_else(|| io::Error::other("missing descriptor fixture report"))
            })
            .transpose()?;
        if descriptors.is_some() && !spec.values.contains_key("descendant_lock_file") {
            return Err(io::Error::other("descriptor fixture requires a lock"));
        }
        spawn_descendant_with_console(
            Path::new(path),
            spec.values.get("descendant_lock_file").map(Path::new),
            spec.values
                .get("descendant_detached_console")
                .is_some_and(|value| value == "1"),
            descriptors,
        )?;
    }
    if let Some(path) = spec.values.get("ready_file") {
        fs::write(path, b"ready")?;
    }
    if let Some(path) = spec.values.get("release_file") {
        let deadline = Instant::now() + Duration::from_secs(60);
        while !Path::new(path).is_file() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "fixture release deadline",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
    Ok(())
}
