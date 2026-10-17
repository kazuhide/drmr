//! Create executable fixtures without opening writable executable descriptors in the test process.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

pub fn write_executable(path: &Path, content: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    // Concurrent forks can inherit a writable descriptor until exec and make Linux
    // reject the fixture with ETXTBSY. Only this child opens the executable for writing.
    let mut writer = Command::new("/bin/sh")
        .args([
            "-c",
            "cat > \"$1\" && chmod \"$2\" \"$1\"",
            "write-executable",
        ])
        .arg(path)
        .arg("755")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(content.as_ref())
        .unwrap();
    let output = writer.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "executable fixture writer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
