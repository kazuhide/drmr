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
        match private_fs::PrivateDir::open(&absolute) {
            Ok(_) => {}
            Err(private_fs::Error::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                drop(private_fs::PrivateDir::ensure(&absolute).map_err(private_error)?);
            }
            Err(error) => return Err(private_error(error)),
        }
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
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const SECURITY_IDENTIFICATION: u32 = 0x0001_0000;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        const FILE_SHARE_DELETE: u32 = 4;
        let absolute = std::path::absolute(path)?;
        // Keep every directory stable until the leaf is acquired. A private
        // leaf ACL alone cannot reject an ADS or an ancestor junction alias.
        let _ancestors = private_ancestors(&absolute)?;
        // Open the link entity, then reject every reparse point through the held
        // handle. Immutable secret reads deny write/delete sharing; mutable logs
        // permit the DELETE access carried by private-fs writers. The validated
        // held object remains stable; directory guards protect pathname users.
        let file = OpenOptions::new()
            .read(true)
            .write(write)
            .share_mode(
                FILE_SHARE_READ
                    | if shared_writer {
                        FILE_SHARE_WRITE | FILE_SHARE_DELETE
                    } else {
                        0
                    },
            )
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(absolute)?;
        private_fs::verify(&file).map_err(private_error)?;
        Ok(file)
    }
}

#[cfg(windows)]
fn private_ancestors(absolute: &Path) -> io::Result<Vec<File>> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use std::path::{Component, PathBuf, Prefix};
    let invalid = || io::Error::new(io::ErrorKind::InvalidInput, "local private file required");
    if !matches!(absolute.components().next(), Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
    {
        return Err(invalid());
    }
    for component in absolute.components() {
        if let Component::Normal(name) = component {
            super::validate_windows_relative_path(name.to_str().ok_or_else(invalid)?)?;
        }
    }
    let parent = absolute.parent().ok_or_else(invalid)?;
    let mut prefix = PathBuf::new();
    let mut handles = Vec::new();
    for component in parent.components() {
        prefix.push(component);
        if matches!(component, Component::RootDir | Component::Normal(_)) {
            if handles.len() >= 256 {
                return Err(invalid());
            }
            let directory = OpenOptions::new()
                // FILE_LIST_DIRECTORY participates in sharing checks; metadata
                // access alone does not prevent a later rename or write open.
                .access_mode(0x81) // FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES
                .share_mode(3) // Allow child-entry updates; deny directory deletion/rename.
                .security_qos_flags(0x0001_0000)
                .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
                .open(&prefix)?;
            let metadata = directory.metadata()?;
            if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
                return Err(invalid());
            }
            handles.push(directory);
        }
    }
    Ok(handles)
}

/// Retains a Windows directory and its ancestors against pathname replacement.
#[cfg(windows)]
pub struct PrivateDirectoryGuard {
    handles: Vec<File>,
}

#[cfg(windows)]
impl std::fmt::Debug for PrivateDirectoryGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PrivateDirectoryGuard")
            .finish_non_exhaustive()
    }
}

#[cfg(windows)]
pub(super) fn pin_directory_path(path: &Path) -> io::Result<PrivateDirectoryGuard> {
    let absolute = std::path::absolute(path)?;
    Ok(PrivateDirectoryGuard {
        handles: private_ancestors(&absolute.join("pending"))?,
    })
}

/// Prepare a strict private directory and retain its pathname for an operation.
/// Existing directories are verified without repairing their access controls.
#[cfg(windows)]
pub fn pin_private_directory(path: &Path) -> io::Result<PrivateDirectoryGuard> {
    let absolute = std::path::absolute(path)?;
    create_private_dir_all(&absolute)?;
    let held = pin_directory_path(&absolute)?;
    drop(private_fs::PrivateDir::open(&absolute).map_err(private_error)?);
    Ok(held)
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
    _directory: Option<PrivateTempDir>,
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
        let directory = private_tempdir()?;
        Ok(PrivateTempFile {
            file: create_private_new(&directory.path().join("capture"))?,
            _directory: Some(directory),
        })
    }
}

/// An exclusively created private staging directory with automatic cleanup.
#[derive(Debug)]
pub struct PrivateTempDir {
    path: std::path::PathBuf,
    #[cfg(windows)]
    _parents: PrivateDirectoryGuard,
    #[cfg(windows)]
    container: Option<TempContainer>,
}

impl PrivateTempDir {
    /// The directory's native path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Remove this workspace while its container guards are still held.
    pub fn close(mut self) -> io::Result<()> {
        let path = std::mem::take(&mut self.path);
        std::fs::remove_dir_all(path)
    }
}

impl Drop for PrivateTempDir {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

/// Create a private temporary directory below an existing private parent.
pub fn private_tempdir_in(parent: &Path, prefix: &str) -> io::Result<PrivateTempDir> {
    #[cfg(windows)]
    let held = {
        // This API requires an existing strict parent. Retain every ancestor
        // before allocating the exclusive child and until its cleanup finishes.
        drop(private_fs::PrivateDir::open(parent).map_err(private_error)?);
        let held = pin_directory_path(parent)?;
        drop(private_fs::PrivateDir::open(parent).map_err(private_error)?);
        held
    };
    let ((), path) = tempfile::Builder::new()
        .prefix(prefix)
        .make_in(parent, create_private_dir_new)?
        .into_parts();
    Ok(PrivateTempDir {
        path: path.keep().map_err(|error| error.error)?,
        #[cfg(windows)]
        container: None,
        #[cfg(windows)]
        _parents: held,
    })
}

/// Create a private temporary workspace whose pathname stays stable until cleanup.
pub fn private_tempdir() -> io::Result<PrivateTempDir> {
    private_tempdir_at(&std::env::temp_dir())
}

fn private_tempdir_at(parent: &Path) -> io::Result<PrivateTempDir> {
    #[cfg(unix)]
    {
        Ok(PrivateTempDir {
            path: tempfile::Builder::new()
                .prefix(".drmr-private-")
                .tempdir_in(parent)?
                .keep(),
        })
    }
    #[cfg(windows)]
    {
        let mut random = [0; 16];
        getrandom::fill(&mut random)
            .map_err(|_| io::Error::other("secure random source unavailable"))?;
        let path = parent.join(format!(".drmr-private-{}", hex::encode(random)));
        let container = TempContainer::acquire(path)?;
        let mut child = private_tempdir_in(&container.path, "work-")?;
        child.container = Some(container);
        Ok(child)
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct TempContainer {
    path: std::path::PathBuf,
    held: PrivateDirectoryGuard,
}

#[cfg(windows)]
impl TempContainer {
    fn acquire(path: std::path::PathBuf) -> io::Result<Self> {
        let path = std::path::absolute(path)?;
        // Install the strict descriptor in the creation syscall. An existing
        // random-name container is verified, never adopted for recursive cleanup.
        let held = pin_private_directory(&path)?;
        Ok(Self { path, held })
    }
}

#[cfg(windows)]
impl Drop for TempContainer {
    fn drop(&mut self) {
        self.held.handles.clear();
        // The exclusive child has already been removed while these pins were
        // held. Do not recursively remove an ensured, potentially existing path.
        let _ = std::fs::remove_dir(&self.path);
    }
}

#[cfg(all(test, windows))]
mod ancestor_tests {
    use super::*;

    #[test]
    fn held_private_ancestors_refuse_rename_until_leaf_acquisition_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let private = temp.path().join("private");
        create_private_dir_all(&private).unwrap();
        let moved = temp.path().join("moved");
        let held = private_ancestors(&private.join("keys.json")).unwrap();
        let rename = std::fs::rename(&private, &moved);
        drop(held);
        if rename.is_ok() {
            std::fs::rename(&moved, &private).unwrap();
        }
        assert!(rename.is_err(), "ancestor moved while its guard was held");
        std::fs::rename(&private, &moved).unwrap();
    }

    #[test]
    fn temporary_container_cleanup_preserves_preexisting_sentinels() {
        use std::io::Write;
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("private");
        create_private_dir_all(&path).unwrap();
        create_private_new(&path.join("sentinel"))
            .unwrap()
            .write_all(b"unchanged")
            .unwrap();
        let container = TempContainer::acquire(path.clone()).unwrap();
        let child = private_tempdir_in(&container.path, "owned-").unwrap();
        let child_path = child.path().to_path_buf();
        drop(child);
        drop(container);
        assert!(!child_path.exists());
        assert_eq!(std::fs::read(path.join("sentinel")).unwrap(), b"unchanged");
    }
}
