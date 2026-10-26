//! Creation-bound sources for publication from an exclusively owned stage.
use super::{ConversionError, File, MAX_MEDIA_BYTES, PUBLISH_ERROR, Path, PathBuf, Stage, native};
use std::{ffi::OsString, path::Component};

/// A source entry beneath a retained stage directory, never adopted by pathname at commit.
pub(in crate::conversion) struct PublicationSource<'a> {
    path: PathBuf,
    parent: File,
    name: OsString,
    object: File,
    identity: native::FileIdentity,
    directory: bool,
    _stage: &'a Stage,
}

impl Stage {
    fn file_name<'a>(&self, path: &'a Path) -> Result<&'a std::ffi::OsStr, ConversionError> {
        let mut relative = path
            .strip_prefix(&self.path)
            .map_err(|_| PUBLISH_ERROR)?
            .components();
        let Some(Component::Normal(name)) = relative.next() else {
            return Err(PUBLISH_ERROR);
        };
        if relative.next().is_some() {
            return Err(PUBLISH_ERROR);
        }
        Ok(name)
    }

    fn publication_root(&self) -> &File {
        #[cfg(unix)]
        {
            &self.outer.held
        }
        #[cfg(windows)]
        {
            &self.root
        }
    }

    fn check_media(&self) -> Result<(), ConversionError> {
        #[cfg(unix)]
        self.outer.check()?;
        let entry = open_entry(self.publication_root(), "media".as_ref(), &self.path, true)?;
        if native::FileIdentity::read(&self.media).map_err(|_| PUBLISH_ERROR)?
            != self.media_identity
            || native::FileIdentity::read(&entry).map_err(|_| PUBLISH_ERROR)? != self.media_identity
        {
            return Err(PUBLISH_ERROR);
        }
        Ok(())
    }

    pub(in crate::conversion) fn directory_source(
        &self,
    ) -> Result<PublicationSource<'_>, ConversionError> {
        self.check_media()?;
        Ok(PublicationSource {
            path: self.path.clone(),
            parent: self
                .publication_root()
                .try_clone()
                .map_err(|_| PUBLISH_ERROR)?,
            name: "media".into(),
            object: self.media.try_clone().map_err(|_| PUBLISH_ERROR)?,
            identity: self.media_identity,
            directory: true,
            _stage: self,
        })
    }

    pub(in crate::conversion) fn file_source(
        &self,
        path: &Path,
    ) -> Result<PublicationSource<'_>, ConversionError> {
        let name = self.file_name(path)?;
        self.check_media()?;
        #[cfg(unix)]
        let parent = self.media.try_clone().map_err(|_| PUBLISH_ERROR)?;
        #[cfg(windows)]
        let parent = {
            // A file publication pins its immediate parent against rename too.
            let parent = open_directory(&self.path, false)?;
            if native::FileIdentity::read(&parent).map_err(|_| PUBLISH_ERROR)?
                != self.media_identity
            {
                return Err(PUBLISH_ERROR);
            }
            parent
        };
        let object = open_entry(&parent, name, path, false)?;
        let identity = native::FileIdentity::read(&object).map_err(|_| PUBLISH_ERROR)?;
        Ok(PublicationSource {
            path: path.to_path_buf(),
            parent,
            name: name.to_owned(),
            object,
            identity,
            directory: false,
            _stage: self,
        })
    }

    pub(in crate::conversion) fn created_file_source(
        &self,
        path: &Path,
        writer: &File,
    ) -> Result<PublicationSource<'_>, ConversionError> {
        let source = self.file_source(path)?;
        if native::FileIdentity::read(writer).map_err(|_| PUBLISH_ERROR)? != source.identity {
            return Err(PUBLISH_ERROR);
        }
        Ok(source)
    }

    pub(in crate::conversion) fn create_file(&self, path: &Path) -> Result<File, ConversionError> {
        let name = self.file_name(path)?;
        self.check_media()?;
        #[cfg(unix)]
        {
            Ok(File::from(
                rustix::fs::openat(
                    &self.media,
                    name,
                    rustix::fs::OFlags::RDWR
                        | rustix::fs::OFlags::CREATE
                        | rustix::fs::OFlags::EXCL
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
                )
                .map_err(|_| PUBLISH_ERROR)?,
            ))
        }
        #[cfg(windows)]
        {
            let _ = name;
            super::create_new(path)
        }
    }
}

impl PublicationSource<'_> {
    pub(in crate::conversion) fn path(&self) -> &Path {
        &self.path
    }

    fn open(&self) -> Result<File, ConversionError> {
        let entry = open_entry(&self.parent, &self.name, &self.path, self.directory)?;
        if native::FileIdentity::read(&self.object).map_err(|_| PUBLISH_ERROR)? != self.identity
            || native::FileIdentity::read(&entry).map_err(|_| PUBLISH_ERROR)? != self.identity
        {
            return Err(PUBLISH_ERROR);
        }
        Ok(entry)
    }

    pub(in crate::conversion) fn reader(&self) -> Result<File, ConversionError> {
        if self.directory {
            return Err(PUBLISH_ERROR);
        }
        self.open()
    }

    pub(in crate::conversion) fn commit(
        &self,
        parent: &File,
        destination: &Path,
    ) -> Result<(), ConversionError> {
        drop(self.open()?);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            rustix::fs::renameat_with(
                &self.parent,
                &self.name,
                parent,
                destination.file_name().ok_or(PUBLISH_ERROR)?,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|_| PUBLISH_ERROR)
        }
        #[cfg(windows)]
        {
            let _ = parent;
            super::atomic_move(&self.path, destination)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = (parent, destination);
            Err(PUBLISH_ERROR)
        }
    }

    pub(in crate::conversion) fn commit_to_path(
        &self,
        destination: &Path,
    ) -> Result<(), ConversionError> {
        drop(self.open()?);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            rustix::fs::renameat_with(
                &self.parent,
                &self.name,
                rustix::fs::CWD,
                destination,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|_| PUBLISH_ERROR)
        }
        #[cfg(windows)]
        {
            super::atomic_move(&self.path, destination)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = destination;
            Err(PUBLISH_ERROR)
        }
    }
}

#[cfg(unix)]
pub(super) fn create_media(parent: &File) -> Result<File, ConversionError> {
    rustix::fs::mkdirat(parent, "media", rustix::fs::Mode::RWXU).map_err(|_| PUBLISH_ERROR)?;
    open_entry(parent, "media".as_ref(), Path::new("media"), true)
}

pub(super) fn open_entry(
    parent: &File,
    name: &std::ffi::OsStr,
    path: &Path,
    directory: bool,
) -> Result<File, ConversionError> {
    #[cfg(unix)]
    let file = {
        let _ = path;
        let mut flags = rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC;
        if directory {
            flags |= rustix::fs::OFlags::DIRECTORY;
        }
        File::from(
            rustix::fs::openat(parent, name, flags, rustix::fs::Mode::empty())
                .map_err(|_| PUBLISH_ERROR)?,
        )
    };
    #[cfg(windows)]
    let file = {
        use std::os::windows::fs::OpenOptionsExt as _;
        let _ = (parent, name);
        if directory {
            open_directory(path, true)?
        } else {
            super::OpenOptions::new()
                .read(true)
                .share_mode(7)
                .custom_flags(0x0020_0000)
                .security_qos_flags(0x0001_0000)
                .open(path)
                .map_err(|_| PUBLISH_ERROR)?
        }
    };
    let metadata = file.metadata().map_err(|_| PUBLISH_ERROR)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(PUBLISH_ERROR);
        }
    }
    if (directory && !metadata.is_dir())
        || (!directory && (!metadata.is_file() || metadata.len() > MAX_MEDIA_BYTES))
    {
        return Err(PUBLISH_ERROR);
    }
    Ok(file)
}

#[cfg(windows)]
pub(super) fn open_directory(path: &Path, allow_delete: bool) -> Result<File, ConversionError> {
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    let file = super::OpenOptions::new()
        .access_mode(0x81)
        .share_mode(if allow_delete { 7 } else { 3 })
        .custom_flags(0x0220_0000)
        .security_qos_flags(0x0001_0000)
        .open(path)
        .map_err(|_| PUBLISH_ERROR)?;
    let metadata = file.metadata().map_err(|_| PUBLISH_ERROR)?;
    if !metadata.is_dir() || metadata.file_attributes() & 0x400 != 0 {
        return Err(PUBLISH_ERROR);
    }
    Ok(file)
}
