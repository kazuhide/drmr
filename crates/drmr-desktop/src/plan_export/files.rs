//! Identity-bound destinations outside the folders served as media.

use drmr_core::native::{self, FileIdentity};
use std::fs::File;
#[cfg(windows)]
use std::fs::OpenOptions;
use std::io;
use std::path::{Component, Path, PathBuf};

pub(super) fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private plan destination required",
    )
}

pub(super) fn open_directory(path: &Path) -> io::Result<File> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(refused());
    }
    #[cfg(unix)]
    let file = {
        use rustix::fs::{Mode, OFlags};
        File::from(rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?)
    };
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(refused());
        }
        OpenOptions::new()
            .access_mode(0x81) // FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES
            .share_mode(3) // Retain each directory against deletion or rename.
            .security_qos_flags(0x0001_0000)
            .custom_flags(0x0220_0000) // BACKUP_SEMANTICS | OPEN_REPARSE_POINT
            .open(path)?
    };
    if !file.metadata()?.is_dir() || std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(refused());
    }
    Ok(file)
}

pub(super) struct Directory {
    requested: PathBuf,
    pub(super) path: PathBuf,
    ancestors: Vec<(PathBuf, FileIdentity, File)>,
}

impl Directory {
    pub(super) fn capture(path: &Path) -> io::Result<Self> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir))
        {
            return Err(refused());
        }
        let canonical = std::fs::canonicalize(path)?;
        let mut current = PathBuf::new();
        let mut ancestors = Vec::new();
        for part in canonical.components() {
            current.push(part);
            if matches!(part, Component::Prefix(_)) {
                continue;
            }
            if ancestors.len() >= 256 {
                return Err(refused());
            }
            let file = open_directory(&current)?;
            ancestors.push((current.clone(), FileIdentity::read(&file)?, file));
        }
        let directory = Self {
            requested: path.to_path_buf(),
            path: canonical,
            ancestors,
        };
        directory.check()?;
        Ok(directory)
    }

    #[cfg(unix)]
    pub(super) fn held(&self) -> io::Result<&File> {
        self.ancestors
            .last()
            .map(|(_, _, file)| file)
            .ok_or_else(refused)
    }

    fn check(&self) -> io::Result<()> {
        if std::fs::canonicalize(&self.requested)? != self.path || self.ancestors.is_empty() {
            return Err(refused());
        }
        for (path, identity, held) in &self.ancestors {
            if FileIdentity::read(held)? != *identity
                || FileIdentity::read(&open_directory(path)?)? != *identity
            {
                return Err(refused());
            }
        }
        Ok(())
    }
}

pub(super) struct MediaFolders {
    input: Directory,
    output: Directory,
}

impl MediaFolders {
    pub fn capture(input: &Path, output: &Path) -> io::Result<Self> {
        Ok(Self {
            input: Directory::capture(input)?,
            output: Directory::capture(output)?,
        })
    }

    fn check_directory(&self, path: &Path) -> io::Result<()> {
        self.input.check()?;
        self.output.check()?;
        if path.starts_with(&self.input.path)
            || path.starts_with(&self.output.path)
            || self.input.path.starts_with(path)
            || self.output.path.starts_with(path)
        {
            Err(refused())
        } else {
            Ok(())
        }
    }

    pub fn prepare_directory(&self, path: &Path) -> io::Result<PathBuf> {
        let parent = Directory::capture(path.parent().ok_or_else(refused)?)?;
        let resolved = parent.path.join(path.file_name().ok_or_else(refused)?);
        // Resolve an existing alias before considering creation or permissions.
        let resolved = native::resolve_path(&resolved)?;
        self.check_directory(&resolved)?;
        native::create_private_dir_all(&resolved)?;
        parent.check()?;
        let directory = Directory::capture(&resolved)?;
        private_directory(&directory.path)?;
        self.check_directory(&directory.path)?;
        Ok(directory.path)
    }
}

pub(super) fn private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if std::fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
            return Err(refused());
        }
    }
    #[cfg(windows)]
    {
        let _guard = native::pin_private_directory(path)?;
    }
    Ok(())
}

pub(super) struct Destination {
    path: PathBuf,
    parent: Directory,
    #[cfg(windows)]
    _private_parent: native::PrivateDirectoryGuard,
}

impl Destination {
    pub fn capture(path: &Path, media: &MediaFolders) -> io::Result<Self> {
        let name = path.file_name().ok_or_else(refused)?;
        let parent = Directory::capture(path.parent().ok_or_else(refused)?)?;
        let path = parent.path.join(name);
        media.check_directory(&parent.path)?;
        private_directory(&parent.path)?;
        #[cfg(windows)]
        let private_parent = native::pin_private_directory(&parent.path)?;
        Ok(Self {
            path,
            parent,
            #[cfg(windows)]
            _private_parent: private_parent,
        })
    }

    pub fn check(&self, media: &MediaFolders) -> io::Result<()> {
        self.parent.check()?;
        media.check_directory(&self.parent.path)
    }

    pub fn create(self, media: &MediaFolders) -> io::Result<OwnedFile> {
        self.check(media)?;
        let file = native::create_private_new(&self.path)?;
        // Keep the actual created handle even if a later identity read fails.
        Ok(OwnedFile {
            destination: self,
            file: Some(file),
            quarantine: None,
            cleanup_phase: CleanupPhase::Original,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CleanupPoint {
    BeforeMove,
    AfterMove,
    Inspect,
    Restore,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CleanupPhase {
    Original,
    Moved,
    Restored,
    Unlinked,
    Finished,
}

pub(super) struct OwnedFile {
    destination: Destination,
    pub file: Option<File>,
    quarantine: Option<super::quarantine::Quarantine>,
    cleanup_phase: CleanupPhase,
}

impl OwnedFile {
    pub fn check(&self, media: &MediaFolders) -> io::Result<()> {
        self.destination.check(media)?;
        self.check_leaf()
    }

    fn check_leaf(&self) -> io::Result<()> {
        let current = native::open_private_shared_read(&self.destination.path)?;
        if FileIdentity::read(&current)?
            != FileIdentity::read(self.file.as_ref().ok_or_else(refused)?)?
        {
            return Err(refused());
        }
        Ok(())
    }

    pub fn cleanup(&mut self) -> io::Result<()> {
        self.cleanup_with(|_| Ok(()))
    }

    pub fn cleanup_with(
        &mut self,
        mut hook: impl FnMut(CleanupPoint) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.cleanup_phase == CleanupPhase::Finished {
            return Ok(());
        }
        let parent = &self.destination.parent;
        parent.check()?;
        let name = Path::new(self.destination.path.file_name().ok_or_else(refused)?);
        if self.cleanup_phase == CleanupPhase::Restored {
            self.quarantine
                .as_mut()
                .ok_or_else(refused)?
                .retire(parent)?;
            self.quarantine = None;
            self.cleanup_phase = CleanupPhase::Original;
        }
        if self.cleanup_phase == CleanupPhase::Original {
            if let Err(error) = self.check_leaf() {
                #[cfg(unix)]
                if error.kind() == io::ErrorKind::NotFound && self.quarantine.is_none() {
                    use std::os::unix::fs::MetadataExt;
                    if self.file.as_ref().ok_or_else(refused)?.metadata()?.nlink() == 0 {
                        self.file = None;
                        self.cleanup_phase = CleanupPhase::Finished;
                        return Ok(());
                    }
                }
                // Missing names do not prove that a held writer was unlinked.
                return Err(error);
            }
            parent.check()?;
            for _ in 0..8 {
                if self.quarantine.is_none() {
                    self.quarantine = Some(super::quarantine::Quarantine::planned(parent));
                }
                let quarantine = self.quarantine.as_mut().ok_or_else(refused)?;
                match quarantine.initialize(parent) {
                    Ok(()) => break,
                    Err(error)
                        if error.kind() == io::ErrorKind::AlreadyExists
                            && !quarantine.created() =>
                    {
                        self.quarantine = None;
                    }
                    Err(error) => return Err(error),
                }
            }
            let quarantine = self.quarantine.as_ref().ok_or_else(refused)?;
            hook(CleanupPoint::BeforeMove)?;
            quarantine.move_in(parent, name)?;
            // Retain the moved entry before any fallible inspection or callback.
            self.cleanup_phase = CleanupPhase::Moved;
            hook(CleanupPoint::AfterMove)?;
        }
        if self.cleanup_phase == CleanupPhase::Moved {
            parent.check()?;
            let quarantine = self.quarantine.as_ref().ok_or_else(refused)?;
            let inspected = hook(CleanupPoint::Inspect).and_then(|()| {
                quarantine.verify_entry(parent, self.file.as_ref().ok_or_else(refused)?)
            });
            if let Err(error) = inspected {
                if hook(CleanupPoint::Restore)
                    .and_then(|()| quarantine.restore(parent, name))
                    .is_ok()
                {
                    self.cleanup_phase = CleanupPhase::Restored;
                }
                return Err(error);
            }
            parent.check()?;
            quarantine.remove_entry(parent)?;
            self.cleanup_phase = CleanupPhase::Unlinked;
            // Windows completes deletion only after the last writer closes.
            // Retain the quarantine and phase if an external reader delays it.
            self.file = None;
        }
        if self.cleanup_phase == CleanupPhase::Unlinked {
            parent.check()?;
            self.quarantine
                .as_mut()
                .ok_or_else(refused)?
                .retire(parent)?;
            self.quarantine = None;
            self.cleanup_phase = CleanupPhase::Finished;
            return Ok(());
        }
        Err(refused())
    }

    #[cfg(test)]
    pub fn quarantine_path(&self) -> Option<&Path> {
        self.quarantine
            .as_ref()
            .map(super::quarantine::Quarantine::path)
    }
}
