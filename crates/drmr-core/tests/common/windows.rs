//! Windows-only filesystem fixtures; production never invokes a command interpreter.
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::Path;

fn command_path(path: &Path) -> OsString {
    let absolute = drmr_core::native::resolve_path(path).unwrap();
    let mut wide: Vec<u16> = absolute
        .as_os_str()
        .encode_wide()
        .map(|unit| {
            if unit == u16::from(b'/') {
                u16::from(b'\\')
            } else {
                unit
            }
        })
        .collect();
    let verbatim_unc: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
    let verbatim: Vec<u16> = "\\\\?\\".encode_utf16().collect();
    if wide.starts_with(&verbatim_unc) {
        wide.splice(..verbatim_unc.len(), [u16::from(b'\\'), u16::from(b'\\')]);
    } else if wide.starts_with(&verbatim) {
        wide.drain(..verbatim.len());
    }
    OsString::from_wide(&wide)
}

/// Junctions need no symbolic-link privilege and exercise native reparse traversal.
pub fn junction(link: &Path, target: &Path) {
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(command_path(link))
        .arg(command_path(target))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "junction fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
