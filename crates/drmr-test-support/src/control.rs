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
    if args.len() == 3 && args[0] == "--fixture-lock" {
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
        fs::write(&args[2], b"ready")?;
        thread::sleep(Duration::from_secs(60));
        drop(held);
        return Ok(true);
    }
    Ok(false)
}

pub fn spawn_descendant(pidfile: &Path, lock_file: Option<&Path>) -> io::Result<()> {
    let mut command = Command::new(env::current_exe()?);
    let ready = lock_file.map(|path| {
        let mut name = path.as_os_str().to_owned();
        name.push(".ready");
        std::path::PathBuf::from(name)
    });
    if let (Some(lock), Some(ready)) = (lock_file, &ready) {
        command.arg("--fixture-lock").arg(lock).arg(ready);
    } else {
        command.arg("--fixture-sleep");
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
        spawn_descendant(
            Path::new(path),
            spec.values.get("descendant_lock_file").map(Path::new),
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
