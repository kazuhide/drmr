//! Creation-bound stage authority and identity checks for small metadata updates.
use super::{BAD, CHANGED, ConversionError, files};
use crate::native::{self, FileIdentity};
use std::{
    fs::File,
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

pub(super) struct BoundFile<'a> {
    owner: &'a files::Stage,
    path: PathBuf,
    identity: FileIdentity,
    length: u64,
    modified: SystemTime,
    lease: Option<File>,
    #[cfg(windows)]
    _parent: native::PrivateDirectoryGuard,
}

fn relative<'a>(owner: &files::Stage, path: &'a Path) -> Result<&'a Path, ConversionError> {
    let relative = path.strip_prefix(owner.path()).map_err(|_| BAD)?;
    let count = relative.components().count();
    if count == 0
        || count > 16
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(BAD);
    }
    Ok(relative)
}

pub(super) fn directory(owner: &files::Stage, path: &Path) -> Result<(), ConversionError> {
    owner.directory_source().map_err(|_| CHANGED)?;
    if path != owner.path() {
        relative(owner, path)?;
        #[cfg(unix)]
        confined(owner, path)?;
        #[cfg(windows)]
        let _directory = native::pin_media_directory(path).map_err(|_| CHANGED)?;
        let metadata = std::fs::symlink_metadata(path).map_err(|_| CHANGED)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(CHANGED);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0
            {
                return Err(CHANGED);
            }
        }
    }
    owner.directory_source().map_err(|_| CHANGED)?;
    Ok(())
}

#[cfg(unix)]
fn confined(owner: &files::Stage, path: &Path) -> Result<FileIdentity, ConversionError> {
    use rustix::fs::{Mode, OFlags};
    let names: Vec<_> = relative(owner, path)?.components().collect();
    owner.directory_source().map_err(|_| CHANGED)?;
    let mut directory = File::from(
        rustix::fs::open(
            owner.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| CHANGED)?,
    );
    let root_identity = FileIdentity::read(&directory).map_err(|_| CHANGED)?;
    owner.directory_source().map_err(|_| CHANGED)?;
    let current = File::from(
        rustix::fs::open(
            owner.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| CHANGED)?,
    );
    if FileIdentity::read(&current).map_err(|_| CHANGED)? != root_identity {
        return Err(CHANGED);
    }
    for (ordinal, part) in names.iter().enumerate() {
        let Component::Normal(name) = part else {
            return Err(BAD);
        };
        let last = ordinal + 1 == names.len();
        let flags = OFlags::RDONLY
            | OFlags::NOFOLLOW
            | OFlags::NONBLOCK
            | OFlags::CLOEXEC
            | if last {
                OFlags::empty()
            } else {
                OFlags::DIRECTORY
            };
        directory = File::from(
            rustix::fs::openat(&directory, *name, flags, Mode::empty()).map_err(|_| CHANGED)?,
        );
    }
    FileIdentity::read(&directory).map_err(|_| CHANGED)
}

#[cfg(windows)]
fn confined(owner: &files::Stage, path: &Path) -> Result<FileIdentity, ConversionError> {
    relative(owner, path)?;
    owner.directory_source().map_err(|_| CHANGED)?;
    FileIdentity::read(&native::open_private_read(path).map_err(|_| CHANGED)?).map_err(|_| CHANGED)
}

fn confined_writer(
    owner: &files::Stage,
    path: &Path,
    _writer: &File,
) -> Result<FileIdentity, ConversionError> {
    #[cfg(unix)]
    {
        confined(owner, path)
    }
    #[cfg(windows)]
    {
        relative(owner, path)?;
        owner.directory_source().map_err(|_| CHANGED)?;
        // Only this identity probe shares the existing writer. The writer keeps
        // denying other write/delete opens, and BoundFile retains the parent pin.
        let current = native::open_private_shared_read(path).map_err(|_| CHANGED)?;
        FileIdentity::read(&current).map_err(|_| CHANGED)
    }
}

impl<'a> BoundFile<'a> {
    pub(super) fn open(owner: &'a files::Stage, path: &Path) -> Result<Self, ConversionError> {
        relative(owner, path)?;
        #[cfg(windows)]
        let parent = native::pin_media_directory(path.parent().ok_or(BAD)?).map_err(|_| CHANGED)?;
        let expected = confined(owner, path)?;
        let lease = native::open_private_read(path).map_err(|_| CHANGED)?;
        let metadata = lease.metadata().map_err(|_| CHANGED)?;
        let identity = FileIdentity::read(&lease).map_err(|_| CHANGED)?;
        if identity != expected
            || !metadata.is_file()
            || metadata.len() > super::super::MAX_MEDIA_BYTES
        {
            return Err(CHANGED);
        }
        owner.directory_source().map_err(|_| CHANGED)?;
        Ok(Self {
            owner,
            path: path.into(),
            identity,
            length: metadata.len(),
            modified: metadata.modified().map_err(|_| CHANGED)?,
            lease: Some(lease),
            #[cfg(windows)]
            _parent: parent,
        })
    }

    pub(super) fn length(&self) -> u64 {
        self.length
    }
    pub(super) fn identity(&self) -> FileIdentity {
        self.identity
    }
    pub(super) fn suspend(&mut self) {
        self.lease.take();
    }
    pub(super) fn reader(&self) -> Result<File, ConversionError> {
        self.check()?;
        if let Some(lease) = &self.lease {
            lease.try_clone().map_err(|_| CHANGED)
        } else {
            let reader = native::open_private_read(&self.path).map_err(|_| CHANGED)?;
            if FileIdentity::read(&reader).map_err(|_| CHANGED)? != self.identity {
                return Err(CHANGED);
            }
            Ok(reader)
        }
    }
    pub(super) fn check(&self) -> Result<(), ConversionError> {
        if confined(self.owner, &self.path)? != self.identity {
            return Err(CHANGED);
        }
        let fresh;
        let lease = if let Some(lease) = &self.lease {
            lease
        } else {
            fresh = native::open_private_read(&self.path).map_err(|_| CHANGED)?;
            &fresh
        };
        let metadata = lease.metadata().map_err(|_| CHANGED)?;
        if FileIdentity::read(lease).map_err(|_| CHANGED)? != self.identity
            || metadata.len() != self.length
            || metadata.modified().map_err(|_| CHANGED)? != self.modified
        {
            return Err(CHANGED);
        }
        Ok(())
    }
    pub(super) fn update<T>(
        &mut self,
        edit: impl FnOnce(&mut File) -> Result<T, ConversionError>,
    ) -> Result<T, ConversionError> {
        self.check()?;
        self.lease.take();
        let mut writer = native::open_media_update(&self.path).map_err(|_| CHANGED)?;
        if FileIdentity::read(&writer).map_err(|_| CHANGED)? != self.identity
            || confined_writer(self.owner, &self.path, &writer)? != self.identity
        {
            return Err(CHANGED);
        }
        let before = writer.metadata().map_err(|_| CHANGED)?;
        if before.len() != self.length || before.modified().map_err(|_| CHANGED)? != self.modified {
            return Err(CHANGED);
        }
        let result = edit(&mut writer);
        drop(writer);
        let lease = native::open_private_read(&self.path).map_err(|_| CHANGED)?;
        let metadata = lease.metadata().map_err(|_| CHANGED)?;
        if FileIdentity::read(&lease).map_err(|_| CHANGED)? != self.identity
            || confined(self.owner, &self.path)? != self.identity
        {
            return Err(CHANGED);
        }
        self.length = metadata.len();
        self.modified = metadata.modified().map_err(|_| CHANGED)?;
        self.lease = Some(lease);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, io::Write as _};

    fn nested_file(owner: &files::Stage) -> PathBuf {
        let directory = owner.path().join("track-0000");
        files::create_dir(&directory).unwrap();
        let path = directory.join("init.mp4");
        files::create_new(&path)
            .unwrap()
            .write_all(b"original")
            .unwrap();
        path
    }

    #[test]
    fn owned_nested_directories_are_distinct_from_regular_files() {
        let owner = files::Stage::new(None).unwrap();
        let path = nested_file(&owner);
        directory(&owner, path.parent().unwrap()).unwrap();
        assert!(directory(&owner, &path).is_err());
        let foreign = files::Stage::new(None).unwrap();
        assert!(directory(&foreign, path.parent().unwrap()).is_err());
    }

    #[test]
    fn updates_keep_the_same_object_and_reacquire_the_read_lease() {
        let owner = files::Stage::new(None).unwrap();
        let path = nested_file(&owner);
        let mut bound = BoundFile::open(&owner, &path).unwrap();
        let identity = bound.identity();
        let mut edited = false;
        bound
            .update(|writer| {
                assert!(FileIdentity::read(writer).unwrap() == identity);
                writer.write_all(b"updated!").unwrap();
                writer.flush().unwrap();
                edited = true;
                Ok(())
            })
            .unwrap();
        assert!(edited);
        assert!(bound.identity() == identity);
        bound.check().unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"updated!");
    }

    #[test]
    fn suspended_updates_refuse_replaced_or_resized_objects_before_editing() {
        let owner = files::Stage::new(None).unwrap();
        let path = nested_file(&owner);
        let mut bound = BoundFile::open(&owner, &path).unwrap();
        bound.suspend();
        let parked = path.with_file_name("parked.mp4");
        fs::rename(&path, &parked).unwrap();
        files::create_new(&path)
            .unwrap()
            .write_all(b"original")
            .unwrap();
        let mut edited = false;
        assert!(
            bound
                .update(|_| {
                    edited = true;
                    Ok(())
                })
                .is_err()
        );
        assert!(!edited);
        assert_eq!(fs::read(&parked).unwrap(), b"original");

        let mut bound = BoundFile::open(&owner, &path).unwrap();
        bound.suspend();
        native::open_media_update(&path)
            .unwrap()
            .set_len(9)
            .unwrap();
        assert!(
            bound
                .update(|_| {
                    edited = true;
                    Ok(())
                })
                .is_err()
        );
        assert!(!edited);
    }

    #[cfg(windows)]
    #[test]
    fn windows_identity_probe_coexists_with_the_exclusive_writer_and_parent_pins() {
        let owner = files::Stage::new(None).unwrap();
        let path = nested_file(&owner);
        let parent = path.parent().unwrap();
        let moved = owner.path().join("moved");
        let mut bound = BoundFile::open(&owner, &path).unwrap();
        let mut edited = false;
        bound
            .update(|writer| {
                // The old strict probe conflicts with the existing writer. Its
                // failure is a control for the sharing mode this test exercises.
                let strict = native::open_private_read(&path).unwrap_err();
                assert_eq!(strict.raw_os_error(), Some(32));
                assert!(native::open_media_update(&path).is_err());
                assert!(fs::remove_file(&path).is_err());
                assert!(fs::rename(parent, &moved).is_err());
                writer.write_all(b"updated!").unwrap();
                edited = true;
                Ok(())
            })
            .unwrap();
        assert!(edited);
        bound.check().unwrap();
        assert!(native::open_media_update(&path).is_err());
        assert!(fs::rename(parent, &moved).is_err());
        drop(bound);
        drop(native::open_media_update(&path).unwrap());
        fs::rename(parent, &moved)
            .expect("releasing the bound file must release its nested parent pin");
        assert_eq!(fs::read(moved.join("init.mp4")).unwrap(), b"updated!");
    }
}
