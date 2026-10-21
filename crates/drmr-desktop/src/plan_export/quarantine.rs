//! Descriptor-bound, nonrecursive cleanup inside an exclusively owned folder.

use super::files::{Directory, refused};
#[cfg(windows)]
use super::files::{open_directory, private_directory};
#[cfg(windows)]
use drmr_core::native;
use drmr_core::native::FileIdentity;
use std::ffi::OsString;
use std::fs::File;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::Path;
#[cfg(any(windows, test))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
const ENTRY: &str = "entry";

/// No Drop cleanup: an unconfirmed entry must remain intact for an owned retry.
pub(super) struct Quarantine {
    name: OsString,
    #[cfg(any(windows, test))]
    path: PathBuf,
    created: bool,
    file: Option<File>,
    identity: Option<FileIdentity>,
    #[cfg(windows)]
    private_guard: Option<native::PrivateDirectoryGuard>,
}

impl Quarantine {
    pub fn planned(parent: &Directory) -> Self {
        #[cfg(not(any(windows, test)))]
        let _ = parent;
        let name = OsString::from(format!(
            ".drmr-plan-cleanup-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        Self {
            #[cfg(any(windows, test))]
            path: parent.path.join(&name),
            name,
            created: false,
            file: None,
            identity: None,
            #[cfg(windows)]
            private_guard: None,
        }
    }

    pub fn created(&self) -> bool {
        self.created
    }

    pub fn initialize(&mut self, parent: &Directory) -> io::Result<()> {
        #[cfg(unix)]
        validate_directory(parent.held()?)?;
        if !self.created {
            #[cfg(unix)]
            rustix::fs::mkdirat(
                parent.held()?,
                &self.name,
                rustix::fs::Mode::from_raw_mode(0o700),
            )?;
            #[cfg(windows)]
            native::create_private_dir_new(&self.path)?;
            // Record ownership before opening or validating the new directory.
            self.created = true;
        }
        if self.file.is_none() {
            self.file = Some(self.open(parent)?);
        }
        let file = self.file.as_ref().ok_or_else(refused)?;
        validate_directory(file)?;
        let identity = FileIdentity::read(file)?;
        if self.identity.is_some_and(|expected| expected != identity) {
            return Err(refused());
        }
        self.identity = Some(identity);
        #[cfg(windows)]
        if self.private_guard.is_none() {
            self.private_guard = Some(native::pin_private_directory(&self.path)?);
        }
        self.check(parent)
    }

    fn open(&self, parent: &Directory) -> io::Result<File> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            Ok(File::from(rustix::fs::openat(
                parent.held()?,
                &self.name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )?))
        }
        #[cfg(windows)]
        {
            let _ = parent;
            open_directory(&self.path)
        }
    }

    fn check(&self, parent: &Directory) -> io::Result<()> {
        #[cfg(unix)]
        validate_directory(parent.held()?)?;
        let expected = self.identity.ok_or_else(refused)?;
        if let Some(file) = &self.file {
            validate_directory(file)?;
            if FileIdentity::read(file)? != expected {
                return Err(refused());
            }
        }
        let current = self.open(parent)?;
        validate_directory(&current)?;
        if FileIdentity::read(&current)? != expected {
            return Err(refused());
        }
        #[cfg(windows)]
        private_directory(&self.path)?;
        Ok(())
    }

    pub fn move_in(&self, parent: &Directory, name: &Path) -> io::Result<()> {
        self.check(parent)?;
        #[cfg(unix)]
        {
            rename_no_replace(parent.held()?, name, self.held()?, Path::new(ENTRY))
        }
        #[cfg(windows)]
        {
            rename_no_replace(&parent.path.join(name), &self.path.join(ENTRY))
        }
    }

    pub fn restore(&self, parent: &Directory, name: &Path) -> io::Result<()> {
        self.check(parent)?;
        #[cfg(unix)]
        {
            rename_no_replace(self.held()?, Path::new(ENTRY), parent.held()?, name)
        }
        #[cfg(windows)]
        {
            rename_no_replace(&self.path.join(ENTRY), &parent.path.join(name))
        }
    }

    pub fn verify_entry(&self, parent: &Directory, writer: &File) -> io::Result<()> {
        self.check(parent)?;
        #[cfg(unix)]
        let current = {
            use rustix::fs::{Mode, OFlags};
            let current = File::from(rustix::fs::openat(
                self.held()?,
                ENTRY,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )?);
            let metadata = current.metadata()?;
            if !metadata.is_file()
                || metadata.uid() != rustix::process::getuid().as_raw()
                || metadata.mode() & 0o077 != 0
                || metadata.nlink() != 1
            {
                return Err(refused());
            }
            current
        };
        #[cfg(windows)]
        let current = native::open_private_shared_read(&self.path.join(ENTRY))?;
        if FileIdentity::read(&current)? != FileIdentity::read(writer)? {
            return Err(refused());
        }
        Ok(())
    }

    pub fn remove_entry(&self, parent: &Directory) -> io::Result<()> {
        self.check(parent)?;
        #[cfg(unix)]
        rustix::fs::unlinkat(self.held()?, ENTRY, rustix::fs::AtFlags::empty())?;
        #[cfg(windows)]
        std::fs::remove_file(self.path.join(ENTRY))?;
        Ok(())
    }

    pub fn retire(&mut self, parent: &Directory) -> io::Result<()> {
        self.check(parent)?;
        #[cfg(windows)]
        {
            // Release only the owned directory pins. The selected parent and
            // its ancestors stay pinned during this nonrecursive removal.
            self.private_guard = None;
            self.file = None;
            std::fs::remove_dir(&self.path)?;
        }
        #[cfg(unix)]
        rustix::fs::unlinkat(parent.held()?, &self.name, rustix::fs::AtFlags::REMOVEDIR)?;
        Ok(())
    }

    #[cfg(unix)]
    fn held(&self) -> io::Result<&File> {
        self.file.as_ref().ok_or_else(refused)
    }

    #[cfg(test)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

fn validate_directory(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_dir() {
        return Err(refused());
    }
    #[cfg(unix)]
    {
        if metadata.mode() & 0o077 != 0 || metadata.uid() != rustix::process::getuid().as_raw() {
            return Err(refused());
        }
    }
    Ok(())
}

#[cfg(unix)]
fn rename_no_replace(
    source_parent: &File,
    source: &Path,
    target_parent: &File,
    target: &Path,
) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        rustix::fs::renameat_with(
            source_parent,
            source,
            target_parent,
            target,
            rustix::fs::RenameFlags::NOREPLACE,
        )?;
        Ok(())
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (source_parent, source, target_parent, target);
        Err(io::ErrorKind::Unsupported.into())
    }
}

#[cfg(windows)]
fn rename_no_replace(source: &Path, target: &Path) -> io::Result<()> {
    // Zero flags forbids replacement and cross-volume copy fallback.
    winsafe::MoveFileEx(
        source.to_str().ok_or_else(refused)?,
        Some(target.to_str().ok_or_else(refused)?),
        winsafe::co::MOVEFILE::default(),
    )
    .map_err(|_| refused())
}
