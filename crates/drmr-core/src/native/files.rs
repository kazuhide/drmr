//! Private file creation and validation without a readable secret-file interval.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// Create private directories, refusing insecure existing directories on Windows.
pub fn create_private_dir_all(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
    }
    #[cfg(windows)]
    {
        let absolute = std::path::absolute(path)?;
        if let Some(parent) = absolute.parent()
            && !parent.exists()
        {
            create_private_dir_all(parent)?;
        }
        // Acquisition rejects ancestor reparse points. New directories receive a
        // protected user-only DACL in the creation syscall, before they are visible.
        private_fs::PrivateDir::ensure(&absolute).map_err(private_error)?;
        Ok(())
    }
}

/// Exclusively create one private directory below an existing private parent.
pub fn create_private_dir_new(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(windows)]
    {
        let parent = path.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "missing parent directory")
        })?;
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing directory name"))?;
        private_fs::PrivateDir::open(parent)
            .and_then(|directory| directory.create_dir(name))
            .map_err(private_error)?;
        Ok(())
    }
}

/// Exclusively create a private regular file. Existing entries are never changed.
pub fn create_private_new(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsHandle;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?;
        let directory = private_fs::PrivateDir::open(parent).map_err(private_error)?;
        let file = directory.create(name).map_err(private_error)?;
        // Clone the validated object, never reopen its pathname. File implements
        // the streaming interfaces used by the existing media and fetch engines.
        Ok(File::from(file.as_handle().try_clone_to_owned()?))
    }
}

/// Open a private file for updates, or exclusively create it if it is absent.
pub fn create_private_update(path: &Path) -> io::Result<File> {
    match create_private_new(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            open_private(path, true, false)
        }
        result => result,
    }
}

/// Create a private file, or truncate an existing file only after validating it.
pub fn create_private_or_truncate(path: &Path) -> io::Result<File> {
    match create_private_new(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let file = open_private(path, true, false)?;
            file.set_len(0)?;
            Ok(file)
        }
        result => result,
    }
}

/// Open a private regular file without changing its ownership, ACL or contents.
pub fn open_private_read(path: &Path) -> io::Result<File> {
    open_private(path, false, false)
}

/// Read a mutable private log while its writer remains open.
/// Ownership, access control, link count, and reparse checks remain mandatory.
pub fn open_private_shared_read(path: &Path) -> io::Result<File> {
    open_private(path, false, true)
}

fn open_private(path: &Path, write: bool, shared_writer: bool) -> io::Result<File> {
    #[cfg(unix)]
    let _ = shared_writer;
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags};
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let access = if write { OFlags::RDWR } else { OFlags::RDONLY };
        // Inspect the object actually opened. NOFOLLOW prevents redirecting an
        // update to another file; NONBLOCK keeps a FIFO replacement from hanging.
        let descriptor = rustix::fs::open(
            path,
            access | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        let file = File::from(descriptor);
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "private file required",
            ));
        }
        Ok(file)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use std::path::{Component, Prefix};
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        let absolute = std::path::absolute(path)?;
        if !matches!(absolute.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "local private file required",
            ));
        }
        // Open the link entity, then reject every reparse point through the held
        // handle. Immutable secret reads deny write/delete sharing; mutable logs
        // permit an existing writer but still deny deletion while being read.
        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .share_mode(FILE_SHARE_READ | if shared_writer { FILE_SHARE_WRITE } else { 0 })
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(absolute)?;
        private_fs::verify(&file).map_err(private_error)?;
        Ok(file)
    }
}

#[cfg(windows)]
fn private_error(error: private_fs::Error) -> io::Error {
    match error {
        private_fs::Error::Io(error) => error,
        private_fs::Error::InvalidRelativePath => {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid private path")
        }
        private_fs::Error::UnsupportedFilesystem { .. }
        | private_fs::Error::UnsupportedPolicy { .. } => {
            io::Error::new(io::ErrorKind::Unsupported, "private filesystem unavailable")
        }
        _ => io::Error::new(io::ErrorKind::PermissionDenied, "private file required"),
    }
}

/// A private capture file whose backing directory is removed after its handle closes.
#[derive(Debug)]
pub struct PrivateTempFile {
    file: File,
    // Fields drop in declaration order: close the file before removing its directory.
    _directory: Option<tempfile::TempDir>,
}

impl std::ops::Deref for PrivateTempFile {
    type Target = File;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

impl std::ops::DerefMut for PrivateTempFile {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.file
    }
}

/// Create a private capture file without writing into an inherited Windows ACL.
pub fn private_tempfile() -> io::Result<PrivateTempFile> {
    #[cfg(unix)]
    {
        Ok(PrivateTempFile {
            file: tempfile::tempfile()?,
            _directory: None,
        })
    }
    #[cfg(windows)]
    {
        let directory = tempfile::tempdir()?;
        let private = directory.path().join("private");
        create_private_dir_all(&private)?;
        Ok(PrivateTempFile {
            file: create_private_new(&private.join("capture"))?,
            _directory: Some(directory),
        })
    }
}

/// An exclusively created private staging directory with automatic cleanup.
#[derive(Debug)]
pub struct PrivateTempDir {
    path: std::path::PathBuf,
}

impl PrivateTempDir {
    /// The directory's native path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Create a private temporary directory below an existing private parent.
pub fn private_tempdir_in(parent: &Path, prefix: &str) -> io::Result<PrivateTempDir> {
    let ((), path) = tempfile::Builder::new()
        .prefix(prefix)
        .make_in(parent, create_private_dir_new)?
        .into_parts();
    Ok(PrivateTempDir {
        path: path.keep().map_err(|error| error.error)?,
    })
}
