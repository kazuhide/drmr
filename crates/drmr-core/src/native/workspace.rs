//! Owner-only workspaces for files created by external media tools.

use std::fs;
use std::io;
use std::path::Path;

#[cfg(windows)]
mod windows;

/// Retain a prepared private/media directory while external tools use its paths.
/// Access controls are verified without mutating unrelated, concurrent outputs.
#[cfg(windows)]
pub fn pin_media_directory(path: &Path) -> io::Result<super::PrivateDirectoryGuard> {
    windows::pin(path)
}

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

/// Open existing protected media for an identity-checked update. This never creates
/// or truncates a file; the caller must compare its held identity and bytes before writing.
pub(crate) fn open_media_update(path: &Path) -> io::Result<fs::File> {
    #[cfg(windows)]
    {
        windows::open_existing(path)
    }
    #[cfg(unix)]
    {
        super::files::open_private(path, true, false)
    }
}

/// Prepare a private parent and create a strict media file, or validate and
/// truncate an existing private file. Strict and prepared media parents are
/// accepted. The media parent is retained while the target is opened. Links and
/// insecure existing targets are rejected before any contents are changed.
pub fn create_media_or_truncate(path: &Path) -> io::Result<fs::File> {
    #[cfg(windows)]
    {
        windows::create_or_truncate(path)
    }
    #[cfg(unix)]
    {
        super::create_private_or_truncate(path)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    #[test]
    fn existing_media_update_preserves_bytes_and_does_not_create_missing_files() {
        let owner = crate::native::private_tempdir().unwrap();
        let root = owner.path().join("media");
        create_media_dir_all(&root).unwrap();
        let path = root.join("init.mp4");
        create_media_new(&path)
            .unwrap()
            .write_all(b"original")
            .unwrap();
        let identity =
            crate::native::FileIdentity::read(&std::fs::File::open(&path).unwrap()).unwrap();
        let mut file = open_media_update(&path).unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original");
        assert!(crate::native::FileIdentity::read(&file).unwrap() == identity);
        drop(file);
        let absent = root.join("missing.mp4");
        assert!(open_media_update(&absent).is_err());
        assert!(!absent.exists());
        #[cfg(windows)]
        {
            let secret = root.join("keys.json");
            assert!(crate::native::create_private_new(&secret).is_err());
            assert!(!secret.exists());
        }
        let alias = root.join("alias.mp4");
        std::fs::hard_link(&path, &alias).unwrap();
        assert!(open_media_update(&alias).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
    }
}
