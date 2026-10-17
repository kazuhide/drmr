//! Owner-only workspaces for files created by external media tools.

use std::fs;
use std::io;
use std::path::Path;

#[cfg(windows)]
mod windows;

/// Prepare an owner-only media directory with inheritance for external outputs.
/// Existing entries must already satisfy the workspace policy; unsafe entries are
/// refused before any permission changes. Secret configuration files use the
/// stricter private-file APIs instead.
pub fn create_media_dir_all(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::create(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        super::create_private_dir_all(path)?;
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.is_dir()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(invalid());
        }
        walk(path, verify_unix)
    }
}

/// Exclusively create an empty strict file inside a prepared media directory.
/// Windows creation inherits only the current user's access from the retained
/// parent, then removes inheritance before returning a writable handle. Existing
/// destinations are never opened or overwritten.
pub fn create_media_new(path: &Path) -> io::Result<fs::File> {
    #[cfg(windows)]
    {
        windows::create_new(path)
    }
    #[cfg(unix)]
    {
        super::create_private_new(path)
    }
}

/// Verify generated media, then give every file the strict private-file policy.
/// No unsafe ACL or filesystem entry is silently repaired. Files are private
/// throughout generation; sealing protects them from later ACL inheritance.
pub fn seal_media_tree(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        windows::seal(path)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        create_media_dir_all(path)?;
        walk(path, |entry, directory| {
            use rustix::fs::{Mode, OFlags};
            let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
            let file = fs::File::from(rustix::fs::open(entry, flags, Mode::empty())?);
            verify_unix_metadata(&file.metadata()?, directory)?;
            file.set_permissions(fs::Permissions::from_mode(if directory {
                0o700
            } else {
                0o600
            }))
        })
    }
}

#[cfg(unix)]
fn verify_unix(path: &Path, directory: bool) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    verify_unix_metadata(&metadata, directory)
}

#[cfg(unix)]
fn verify_unix_metadata(metadata: &fs::Metadata, directory: bool) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if metadata.uid() != rustix::process::geteuid().as_raw()
        || (directory && !metadata.is_dir())
        || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
    {
        return Err(invalid());
    }
    Ok(())
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private media workspace required",
    )
}

// Stream directory entries instead of retaining the entire tree. Both passes
// have explicit depth/entry bounds, including on an operator-provided workspace.
fn walk(root: &Path, mut visit: impl FnMut(&Path, bool) -> io::Result<()>) -> io::Result<()> {
    visit(root, true)?;
    let mut pending = vec![fs::read_dir(root)?];
    let mut count = 0usize;
    while let Some(entries) = pending.last_mut() {
        let Some(entry) = entries.next() else {
            pending.pop();
            continue;
        };
        count += 1;
        if count > 1_000_000 {
            return Err(invalid());
        }
        let path = entry?.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
            return Err(invalid());
        }
        let directory = metadata.is_dir();
        visit(&path, directory)?;
        if directory {
            if pending.len() >= 256 {
                return Err(invalid());
            }
            pending.push(fs::read_dir(path)?);
        }
    }
    Ok(())
}
