//! Windows-only filesystem fixtures; production never invokes a command interpreter.
use std::path::Path;
/// Junctions need no symbolic-link privilege and exercise native reparse traversal.
pub fn junction(link: &Path, target: &Path) {
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
