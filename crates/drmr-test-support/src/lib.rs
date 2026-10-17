//! Native subprocess fixtures, available only through development dependencies.
mod protocol;
pub use protocol::{Mode, ToolSpec};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
/// Install through an immutable child. No writable executable descriptor enters the test parent.
pub fn install(path: &Path, spec: &ToolSpec) -> io::Result<PathBuf> {
    let executable = Path::new(env!("DRMR_NATIVE_FIXTURE"));
    if !executable.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "cross-target fixture compilation does not provide a runnable native executable",
        ));
    }
    let path = program_path(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut child = Command::new(executable)
        .arg("--install")
        .arg(&path)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("fixture installer stdin missing"))?;
    let written = spec.encode(&mut input).and_then(|()| input.flush());
    drop(input);
    let status = child.wait()?;
    written?;
    if !status.success() {
        return Err(io::Error::other("native fixture installation failed"));
    }
    Ok(path)
}

/// The actual native executable name; Windows tools require their `.exe` suffix.
pub fn program_path(path: &Path) -> PathBuf {
    let mut path = path.to_path_buf();
    if cfg!(windows) && path.extension().is_none_or(|s| s != "exe") {
        let mut name = path.into_os_string();
        name.push(".exe");
        path = PathBuf::from(name);
    }
    path
}

impl ToolSpec {
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.values.insert(name.into(), value.into());
        self
    }
}
