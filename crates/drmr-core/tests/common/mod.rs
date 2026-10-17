//! Shared batch and storage fixtures: a local S3 endpoint and synthetic DASH assets.
#![allow(dead_code)] // reason: a shared fixture; each test binary uses a different subset

pub mod dash;
pub mod fake_s3;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Synthetic key material only (never a real key).
pub const KID: &str = "000102030405060708090a0b0c0d0e0f";
/// Synthetic content key.
pub const KEY: &str = "10101010101010101010101010101010";
/// Synthetic IV.
pub const IV: &str = "20202020202020202020202020202020";

/// Write `content` to `path` (parents created) with `mode`.
pub fn write_mode(path: &Path, content: &[u8], mode: u32) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Write `content` to `path`, parents created.
pub fn write(path: &Path, content: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// Every regular file under `root` with its bytes, by relative path.
pub fn snapshot_tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() && !path.is_symlink() {
                pending.push(path);
            } else if path.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                files.push((relative, std::fs::read(&path).unwrap()));
            }
        }
    }
    files.sort();
    files
}

/// Every file name under `root` (recursively) that starts with `prefix`.
pub fn names_with_prefix(root: &Path, prefix: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with(prefix) {
                found.push(name);
            }
            if path.is_dir() && !path.is_symlink() {
                pending.push(path);
            }
        }
    }
    found
}
