//! Descriptor-relative cleanup beneath an owned root and a protected parent entry.
use super::{CLEANUP_ERROR, ConversionError, File, OpenOptions, Path, PathBuf, native};
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use std::{
    ffi::OsStr,
    os::unix::fs::{MetadataExt as _, OpenOptionsExt as _},
};

const PARENT_ERROR: ConversionError = ConversionError("temporary-workspace-parent-unsafe");
const MAX_DEPTH: usize = 256;
const MAX_ENTRIES: usize = 1_000_000;

/// Final unlink has no atomic identity predicate. Exclude parents in which a
/// different unprivileged user can replace our entry after its identity check.
pub(in crate::conversion) fn check_workspace_parent(file: &File) -> Result<(), ConversionError> {
    let metadata = file.metadata().map_err(|_| PARENT_ERROR)?;
    let trusted_owner =
        metadata.uid() == rustix::process::geteuid().as_raw() || metadata.uid() == 0;
    let protected_entry = metadata.mode() & 0o022 == 0 || metadata.mode() & 0o1000 != 0;
    if !metadata.is_dir() || !trusted_owner || !protected_entry {
        return Err(PARENT_ERROR);
    }
    Ok(())
}

pub(super) struct Parent {
    path: PathBuf,
    held: File,
    identity: native::FileIdentity,
}
impl Parent {
    pub(super) fn open(path: &Path) -> Result<Self, ConversionError> {
        let path = path.canonicalize().map_err(|_| PARENT_ERROR)?;
        let held = open_directory(&path).map_err(|_| PARENT_ERROR)?;
        check_workspace_parent(&held)?;
        let identity = native::FileIdentity::read(&held).map_err(|_| PARENT_ERROR)?;
        Ok(Self {
            path,
            held,
            identity,
        })
    }
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
    pub(super) fn check(&self) -> Result<(), ConversionError> {
        check_workspace_parent(&self.held).map_err(|_| CLEANUP_ERROR)?;
        if native::FileIdentity::read(&self.held).map_err(|_| CLEANUP_ERROR)? != self.identity
            || native::FileIdentity::read(&open_directory(&self.path)?)
                .map_err(|_| CLEANUP_ERROR)?
                != self.identity
        {
            return Err(CLEANUP_ERROR);
        }
        Ok(())
    }
    pub(super) fn open_owned(&self, name: &OsStr) -> Result<File, ConversionError> {
        let file = directory_at(&self.held, name)?;
        super::check_stage_directory(&file)?;
        Ok(file)
    }
    pub(super) fn remove_owned(&self, name: &OsStr, owned: &File) -> Result<(), ConversionError> {
        self.check()?;
        let current = self.open_owned(name)?;
        if native::FileIdentity::read(&current).map_err(|_| CLEANUP_ERROR)?
            != native::FileIdentity::read(owned).map_err(|_| CLEANUP_ERROR)?
        {
            return Err(CLEANUP_ERROR);
        }
        rustix::fs::unlinkat(&self.held, name, AtFlags::REMOVEDIR).map_err(|_| CLEANUP_ERROR)
    }
}

pub(super) fn clear_owned(root: &File) -> Result<(), ConversionError> {
    super::check_stage_directory(root)?;
    let mut remaining = MAX_ENTRIES;
    clear(root, 0, &mut remaining)
}

fn clear(directory: &File, depth: usize, remaining: &mut usize) -> Result<(), ConversionError> {
    if depth > MAX_DEPTH {
        return Err(CLEANUP_ERROR);
    }
    let entries = rustix::fs::Dir::read_from(directory).map_err(|_| CLEANUP_ERROR)?;
    for entry in entries {
        let entry = entry.map_err(|_| CLEANUP_ERROR)?;
        let name = entry.file_name();
        if matches!(name.to_bytes(), b"." | b"..") {
            continue;
        }
        *remaining = remaining.checked_sub(1).ok_or(CLEANUP_ERROR)?;
        let metadata = rustix::fs::statat(directory, name, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| CLEANUP_ERROR)?;
        if metadata.st_uid != rustix::process::geteuid().as_raw() {
            return Err(CLEANUP_ERROR);
        }
        if FileType::from_raw_mode(metadata.st_mode) == FileType::Directory {
            let child = directory_at(directory, name)?;
            clear(&child, depth + 1, remaining)?;
            let current = directory_at(directory, name)?;
            if native::FileIdentity::read(&current).map_err(|_| CLEANUP_ERROR)?
                != native::FileIdentity::read(&child).map_err(|_| CLEANUP_ERROR)?
            {
                return Err(CLEANUP_ERROR);
            }
            rustix::fs::unlinkat(directory, name, AtFlags::REMOVEDIR).map_err(|_| CLEANUP_ERROR)?;
        } else {
            // This unlinks a link itself, never the object to which it points.
            rustix::fs::unlinkat(directory, name, AtFlags::empty()).map_err(|_| CLEANUP_ERROR)?;
        }
    }
    Ok(())
}

fn directory_at(parent: &File, name: impl rustix::path::Arg) -> Result<File, ConversionError> {
    let file = File::from(
        rustix::fs::openat(parent, name, directory_flags(), Mode::empty())
            .map_err(|_| CLEANUP_ERROR)?,
    );
    let metadata = file.metadata().map_err(|_| CLEANUP_ERROR)?;
    if !metadata.is_dir() || metadata.uid() != rustix::process::geteuid().as_raw() {
        return Err(CLEANUP_ERROR);
    }
    Ok(file)
}

fn open_directory(path: &Path) -> Result<File, ConversionError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(directory_flags().bits().cast_signed())
        .open(path)
        .map_err(|_| CLEANUP_ERROR)
}

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
}
