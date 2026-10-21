//! Identity-bound destinations outside the folders served as media.

use drmr_core::native::{self, FileIdentity};
use std::fs::File;
#[cfg(windows)]
use std::fs::OpenOptions;
use std::io;
use std::path::{Component, Path, PathBuf};

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "private plan destination required",
    )
}

fn open_directory(path: &Path) -> io::Result<File> {
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

struct Directory {
    requested: PathBuf,
    path: PathBuf,
    ancestors: Vec<(PathBuf, FileIdentity, File)>,
}

impl Directory {
    fn capture(path: &Path) -> io::Result<Self> {
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

    fn check_destination(&self, path: &Path) -> io::Result<()> {
        self.input.check()?;
        self.output.check()?;
        if path.starts_with(&self.input.path) || path.starts_with(&self.output.path) {
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
        self.check_destination(&resolved)?;
        native::create_private_dir_all(&resolved)?;
        parent.check()?;
        let directory = Directory::capture(&resolved)?;
        private_directory(&directory.path)?;
        self.check_destination(&directory.path)?;
        Ok(directory.path)
    }
}

fn private_directory(path: &Path) -> io::Result<()> {
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
        media.check_destination(&path)?;
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
        media.check_destination(&self.path)
    }

    pub fn create(self, media: &MediaFolders) -> io::Result<OwnedFile> {
        self.check(media)?;
        let file = native::create_private_new(&self.path)?;
        // Keep the actual created handle even if a later identity read fails.
        Ok(OwnedFile {
            destination: self,
            file,
        })
    }
}

pub(super) struct OwnedFile {
    destination: Destination,
    pub file: File,
}

impl OwnedFile {
    pub fn check(&self, media: &MediaFolders) -> io::Result<()> {
        self.destination.check(media)?;
        self.check_leaf()
    }

    fn check_leaf(&self) -> io::Result<()> {
        let current = native::open_private_read(&self.destination.path)?;
        if FileIdentity::read(&current)? != FileIdentity::read(&self.file)? {
            return Err(refused());
        }
        Ok(())
    }

    pub fn cleanup(&self) -> io::Result<()> {
        self.destination.parent.check()?;
        match self.check_leaf() {
            Ok(()) => {
                self.destination.parent.check()?;
                std::fs::remove_file(&self.destination.path)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}
