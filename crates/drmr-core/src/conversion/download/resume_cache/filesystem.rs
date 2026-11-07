//! Small handle-based operations confined to an admitted resume cache.
use super::{CLEANUP, ConversionError, FileIdentity, UNSAFE};
use std::{
    ffi::OsStr,
    fs::{File, OpenOptions},
    path::{Component, Path, PathBuf},
};

pub(super) struct Directory {
    pub(super) path: PathBuf,
    held: Option<File>,
    identity: FileIdentity,
    private: bool,
    #[cfg(windows)]
    _ancestors: Vec<File>,
}

impl Directory {
    pub(super) fn open(path: &Path, private: bool) -> Result<Self, ConversionError> {
        #[cfg(windows)]
        let ancestors = pin_ancestors(path)?;
        let held = open_directory(path, private)?;
        let identity = FileIdentity::read(&held).map_err(|_| UNSAFE)?;
        Ok(Self {
            path: path.to_path_buf(),
            held: Some(held),
            identity,
            private,
            #[cfg(windows)]
            _ancestors: ancestors,
        })
    }
    pub(super) fn identity(&self) -> FileIdentity {
        self.identity
    }
    pub(super) fn check(&self) -> Result<(), ConversionError> {
        let held = self.held.as_ref().ok_or(UNSAFE)?;
        verify_directory(held, self.private)?;
        if FileIdentity::read(held).map_err(|_| UNSAFE)? != self.identity
            || FileIdentity::read(&open_directory(&self.path, self.private)?).map_err(|_| UNSAFE)?
                != self.identity
        {
            return Err(UNSAFE);
        }
        Ok(())
    }
    pub(super) fn child(&self, name: &str) -> Result<Self, ConversionError> {
        leaf(name)?;
        self.check()?;
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let held = File::from(
                rustix::fs::openat(
                    self.held.as_ref().ok_or(UNSAFE)?,
                    name,
                    OFlags::RDONLY
                        | OFlags::DIRECTORY
                        | OFlags::NOFOLLOW
                        | OFlags::NONBLOCK
                        | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| UNSAFE)?,
            );
            verify_directory(&held, true)?;
            let identity = FileIdentity::read(&held).map_err(|_| UNSAFE)?;
            let child = Self {
                path: self.path.join(name),
                held: Some(held),
                identity,
                private: true,
            };
            self.check()?;
            child.check()?;
            Ok(child)
        }
        #[cfg(windows)]
        {
            let child = Self::open(&self.path.join(name), true)?;
            self.check()?;
            Ok(child)
        }
        #[cfg(not(any(unix, windows)))]
        Err(UNSAFE)
    }
    pub(super) fn file(&self, name: &str, write: bool) -> Result<File, ConversionError> {
        leaf(name)?;
        self.check()?;
        #[cfg(unix)]
        let file = {
            use rustix::fs::{Mode, OFlags};
            let access = if write { OFlags::RDWR } else { OFlags::RDONLY };
            File::from(
                rustix::fs::openat(
                    self.held.as_ref().ok_or(UNSAFE)?,
                    name,
                    access | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|_| UNSAFE)?,
            )
        };
        #[cfg(windows)]
        let file = if write {
            use std::os::windows::fs::OpenOptionsExt as _;
            // Existing strict lock only: never create, truncate, repair its ACL,
            // or require the separate inherited-media parent policy.
            OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(1)
                .security_qos_flags(0x0001_0000)
                .custom_flags(0x0020_0000)
                .open(self.path.join(name))
        } else {
            crate::native::open_private_read(&self.path.join(name))
        }
        .map_err(|_| UNSAFE)?;
        #[cfg(not(any(unix, windows)))]
        let file: File = return Err(UNSAFE);
        verify_file(&file)?;
        self.check()?;
        Ok(file)
    }
    pub(super) fn shared_file(&self, name: &str) -> Result<File, ConversionError> {
        #[cfg(unix)]
        {
            self.file(name, false)
        }
        #[cfg(windows)]
        {
            leaf(name)?;
            self.check()?;
            let file = crate::native::open_private_shared_read(&self.path.join(name))
                .map_err(|_| UNSAFE)?;
            self.check()?;
            Ok(file)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = name;
            Err(UNSAFE)
        }
    }
    pub(super) fn remove_file(
        &self,
        name: &str,
        expected: FileIdentity,
    ) -> Result<(), ConversionError> {
        let current = self.file(name, false).map_err(|_| CLEANUP)?;
        if FileIdentity::read(&current).map_err(|_| CLEANUP)? != expected {
            return Err(CLEANUP);
        }
        drop(current);
        self.check().map_err(|_| CLEANUP)?;
        #[cfg(unix)]
        rustix::fs::unlinkat(
            self.held.as_ref().ok_or(CLEANUP)?,
            name,
            rustix::fs::AtFlags::empty(),
        )
        .map_err(|_| CLEANUP)?;
        #[cfg(windows)]
        {
            let file = crate::native::open_media_mutation(&self.path.join(name), false)
                .map_err(|_| CLEANUP)?;
            if FileIdentity::read(&file).map_err(|_| CLEANUP)? != expected {
                return Err(CLEANUP);
            }
            crate::native::delete_media_handle(&file, false).map_err(|_| CLEANUP)?;
        }
        self.check().map_err(|_| CLEANUP)
    }
    pub(super) fn remove_child(
        &self,
        name: &OsStr,
        expected: FileIdentity,
    ) -> Result<(), ConversionError> {
        if Path::new(name).components().count() != 1
            || !matches!(
                Path::new(name).components().next(),
                Some(Component::Normal(_))
            )
        {
            return Err(CLEANUP);
        }
        self.check().map_err(|_| CLEANUP)?;
        let current = Self::open(&self.path.join(name), true).map_err(|_| CLEANUP)?;
        if current.identity != expected {
            return Err(CLEANUP);
        }
        drop(current);
        self.check().map_err(|_| CLEANUP)?;
        #[cfg(unix)]
        rustix::fs::unlinkat(
            self.held.as_ref().ok_or(CLEANUP)?,
            name,
            rustix::fs::AtFlags::REMOVEDIR,
        )
        .map_err(|_| CLEANUP)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            // The cache's containing directory need not have a media ACL. Its
            // held parent/ancestor pins establish this pathname; verify the
            // admitted child's identity and private ACL through the delete handle.
            let directory = OpenOptions::new()
                .access_mode(0x0013_0080)
                .share_mode(5)
                .security_qos_flags(0x0001_0000)
                .custom_flags(0x0220_0000)
                .open(self.path.join(name))
                .map_err(|_| CLEANUP)?;
            if FileIdentity::read(&directory).map_err(|_| CLEANUP)? != expected {
                return Err(CLEANUP);
            }
            crate::native::delete_media_handle(&directory, true).map_err(|_| CLEANUP)?;
        }
        self.check().map_err(|_| CLEANUP)
    }
    pub(super) fn release(&mut self) {
        self.held.take();
    }
    pub(super) fn estimated_bytes(&self) -> u64 {
        // Includes path copies, native conversions, and at most 256 ancestor handles.
        self.path
            .as_os_str()
            .len()
            .saturating_mul(16)
            .saturating_add(32 * 1024) as u64
    }
}

fn leaf(name: &str) -> Result<(), ConversionError> {
    if name.is_empty()
        || Path::new(name)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || name.contains(['/', '\\'])
    {
        return Err(UNSAFE);
    }
    #[cfg(windows)]
    crate::native::validate_windows_relative_path(name).map_err(|_| UNSAFE)?;
    Ok(())
}

pub(super) fn open_directory(path: &Path, private: bool) -> Result<File, ConversionError> {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.read(true).custom_flags(
            (rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK
                | rustix::fs::OFlags::CLOEXEC)
                .bits()
                .cast_signed(),
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options
            .access_mode(0x81)
            .share_mode(3)
            .custom_flags(0x0220_0000)
            .security_qos_flags(0x0001_0000);
        if private {
            drop(private_fs::PrivateDir::open(path).map_err(|_| UNSAFE)?);
        }
    }
    let file = options.open(path).map_err(|_| UNSAFE)?;
    verify_directory(&file, private)?;
    Ok(file)
}

fn verify_directory(file: &File, private: bool) -> Result<(), ConversionError> {
    let metadata = file.metadata().map_err(|_| UNSAFE)?;
    if !metadata.is_dir() {
        return Err(UNSAFE);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if private {
            if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0
            {
                return Err(UNSAFE);
            }
        } else {
            super::super::super::files::check_workspace_parent(file).map_err(|_| UNSAFE)?;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(UNSAFE);
        }
        // Directory privacy is checked by PrivateDir::open at every pathname
        // revalidation. private_fs::verify accepts regular files only.
        let _ = private;
    }
    Ok(())
}

fn verify_file(file: &File) -> Result<(), ConversionError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata().map_err(|_| UNSAFE)?;
        if !metadata.is_file()
            || metadata.uid() != rustix::process::geteuid().as_raw()
            || metadata.nlink() != 1
            || metadata.mode() & 0o077 != 0
        {
            return Err(UNSAFE);
        }
    }
    #[cfg(windows)]
    private_fs::verify(file).map_err(|_| UNSAFE)?;
    Ok(())
}

#[cfg(windows)]
fn pin_ancestors(path: &Path) -> Result<Vec<File>, ConversionError> {
    let mut prefix = PathBuf::new();
    let mut held = Vec::new();
    for component in path.parent().ok_or(UNSAFE)?.components() {
        prefix.push(component);
        if matches!(component, Component::RootDir | Component::Normal(_)) {
            if held.len() >= 256 {
                return Err(UNSAFE);
            }
            held.push(open_directory(&prefix, false)?);
        }
    }
    Ok(held)
}
