//! File moves stay relative to the stage's creation-bound directory descriptors.
use super::{ConversionError, File, PUBLISH_ERROR, Path, PathBuf, Stage, native, publication};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::{Read as _, Write as _},
};

struct Directory {
    held: File,
    identity: native::FileIdentity,
}

pub(in crate::conversion) struct StageRelocation<'a> {
    stage: &'a Stage,
    directories: BTreeMap<PathBuf, Directory>,
    files: BTreeMap<PathBuf, native::FileIdentity>,
}

impl<'a> StageRelocation<'a> {
    pub(in crate::conversion) fn new(stage: &'a Stage) -> Result<Self, ConversionError> {
        stage.directory_source()?;
        #[cfg(unix)]
        let root = stage.media.try_clone().map_err(|_| PUBLISH_ERROR)?;
        #[cfg(windows)]
        let root = publication::open_directory(stage.path(), false)?;
        if native::FileIdentity::read(&root).map_err(|_| PUBLISH_ERROR)? != stage.media_identity {
            return Err(PUBLISH_ERROR);
        }
        Ok(Self {
            stage,
            directories: BTreeMap::from([(
                PathBuf::new(),
                Directory {
                    held: root,
                    identity: stage.media_identity,
                },
            )]),
            files: BTreeMap::new(),
        })
    }

    pub(in crate::conversion) fn entries(
        &self,
        path: &Path,
        maximum: usize,
    ) -> Result<Vec<PathBuf>, ConversionError> {
        self.check_directory(path)?;
        if maximum > 100_000 {
            return Err(PUBLISH_ERROR);
        }
        let mut names = Vec::new();
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt as _;
            for entry in
                rustix::fs::Dir::read_from(self.directory(path)?).map_err(|_| PUBLISH_ERROR)?
            {
                let entry = entry.map_err(|_| PUBLISH_ERROR)?;
                let name = entry.file_name().to_bytes();
                if matches!(name, b"." | b"..") {
                    continue;
                }
                if names.len() == maximum {
                    return Err(PUBLISH_ERROR);
                }
                names.push(PathBuf::from(std::ffi::OsString::from_vec(name.to_vec())));
            }
        }
        #[cfg(windows)]
        {
            for entry in
                std::fs::read_dir(self.stage.path().join(path)).map_err(|_| PUBLISH_ERROR)?
            {
                let entry = entry.map_err(|_| PUBLISH_ERROR)?;
                if names.len() == maximum {
                    return Err(PUBLISH_ERROR);
                }
                names.push(PathBuf::from(entry.file_name()));
            }
        }
        self.check_directory(path)?;
        Ok(names)
    }

    pub(in crate::conversion) fn capture_directory(
        &mut self,
        relative: &Path,
    ) -> Result<(), ConversionError> {
        crate::conversion::layout::path_text(relative)?;
        if self.directories.contains_key(relative) {
            return Err(PUBLISH_ERROR);
        }
        let parent = relative.parent().ok_or(PUBLISH_ERROR)?;
        self.check_directory(parent)?;
        let held = self.open_directory(parent, relative.file_name().ok_or(PUBLISH_ERROR)?)?;
        let identity = native::FileIdentity::read(&held).map_err(|_| PUBLISH_ERROR)?;
        self.directories
            .insert(relative.into(), Directory { held, identity });
        Ok(())
    }

    pub(in crate::conversion) fn capture_file(
        &mut self,
        relative: &Path,
    ) -> Result<(), ConversionError> {
        crate::conversion::layout::path_text(relative)?;
        self.check_directory(relative.parent().ok_or(PUBLISH_ERROR)?)?;
        let file = self.open_file(relative)?;
        let identity = native::FileIdentity::read(&file).map_err(|_| PUBLISH_ERROR)?;
        if self.files.insert(relative.into(), identity).is_some() {
            return Err(PUBLISH_ERROR);
        }
        Ok(())
    }

    pub(in crate::conversion) fn check(&self) -> Result<(), ConversionError> {
        self.stage.directory_source()?;
        for (path, directory) in &self.directories {
            if native::FileIdentity::read(&directory.held).map_err(|_| PUBLISH_ERROR)?
                != directory.identity
            {
                return Err(PUBLISH_ERROR);
            }
            if !path.as_os_str().is_empty() {
                let current = self.open_directory(
                    path.parent().ok_or(PUBLISH_ERROR)?,
                    path.file_name().ok_or(PUBLISH_ERROR)?,
                )?;
                if native::FileIdentity::read(&current).map_err(|_| PUBLISH_ERROR)?
                    != directory.identity
                {
                    return Err(PUBLISH_ERROR);
                }
            }
        }
        Ok(())
    }

    pub(in crate::conversion) fn prepare_parents(
        &mut self,
        relative: &Path,
    ) -> Result<(), ConversionError> {
        crate::conversion::layout::path_text(relative)?;
        let parents = relative
            .ancestors()
            .skip(1)
            .filter(|p| !p.as_os_str().is_empty())
            .collect::<Vec<_>>();
        for path in parents.into_iter().rev() {
            self.check_directory(path.parent().ok_or(PUBLISH_ERROR)?)?;
            if !self.directories.contains_key(path) {
                #[cfg(unix)]
                let parent = self.directory(path.parent().ok_or(PUBLISH_ERROR)?)?;
                #[cfg(unix)]
                rustix::fs::mkdirat(
                    parent,
                    path.file_name().ok_or(PUBLISH_ERROR)?,
                    rustix::fs::Mode::RWXU,
                )
                .map_err(|_| PUBLISH_ERROR)?;
                #[cfg(windows)]
                native::create_private_dir_new(&self.stage.path().join(path))
                    .map_err(|_| PUBLISH_ERROR)?;
                self.capture_directory(path)?;
            }
        }
        self.check_directory(relative.parent().ok_or(PUBLISH_ERROR)?)
    }

    pub(in crate::conversion) fn read_manifest(
        &self,
        path: &Path,
    ) -> Result<Vec<u8>, ConversionError> {
        self.check_file(path)?;
        let mut bytes = Vec::new();
        self.open_file(path)?
            .take(crate::inspection::MAX_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| PUBLISH_ERROR)?;
        if bytes.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
            return Err(ConversionError("manifest-size-limit"));
        }
        self.check_file(path)?;
        Ok(bytes)
    }

    pub(in crate::conversion) fn move_file(
        &mut self,
        source: &Path,
        destination: &Path,
    ) -> Result<(), ConversionError> {
        self.check_file(source)?;
        if source == destination {
            return Ok(());
        }
        crate::conversion::layout::path_text(destination)?;
        self.check_directory(destination.parent().ok_or(PUBLISH_ERROR)?)?;
        self.rename(source, destination)?;
        let expected = self.files.remove(source).ok_or(PUBLISH_ERROR)?;
        self.files.insert(destination.into(), expected);
        self.check_file(destination)?;
        self.check_directory(source.parent().ok_or(PUBLISH_ERROR)?)
    }

    pub(in crate::conversion) fn replace_manifest(
        &mut self,
        path: &Path,
        bytes: &[u8],
    ) -> Result<(), ConversionError> {
        if bytes.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
            return Err(ConversionError("manifest-size-limit"));
        }
        self.check_file(path)?;
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| ConversionError("secure-random-unavailable"))?;
        let pending = PathBuf::from(format!(".drmr-layout-{}.tmp", hex::encode(nonce)));
        let backup = pending.with_extension("old");
        let mut writer = self.stage.create_file(&self.stage.path().join(&pending))?;
        let identity = native::FileIdentity::read(&writer).map_err(|_| PUBLISH_ERROR)?;
        writer
            .write_all(bytes)
            .and_then(|()| writer.flush())
            .map_err(|_| PUBLISH_ERROR)?;
        drop(writer);
        self.files.insert(pending.clone(), identity);
        self.move_file(path, &backup)?;
        self.move_file(&pending, path)?;
        self.remove_file(&backup)
    }

    pub(in crate::conversion) fn remove_empty_directory(
        &mut self,
        path: &Path,
    ) -> Result<(), ConversionError> {
        self.check()?;
        if path.as_os_str().is_empty()
            || self.files.keys().any(|file| file.starts_with(path))
            || self
                .directories
                .keys()
                .any(|dir| dir != path && dir.starts_with(path))
        {
            return Ok(());
        }
        let directory = self.directories.remove(path).ok_or(PUBLISH_ERROR)?;
        drop(directory);
        #[cfg(unix)]
        rustix::fs::unlinkat(
            self.directory(path.parent().ok_or(PUBLISH_ERROR)?)?,
            path.file_name().ok_or(PUBLISH_ERROR)?,
            rustix::fs::AtFlags::REMOVEDIR,
        )
        .map_err(|_| PUBLISH_ERROR)?;
        #[cfg(windows)]
        std::fs::remove_dir(self.stage.path().join(path)).map_err(|_| PUBLISH_ERROR)?;
        self.check()
    }

    fn remove_file(&mut self, path: &Path) -> Result<(), ConversionError> {
        self.check_file(path)?;
        #[cfg(unix)]
        rustix::fs::unlinkat(
            self.directory(path.parent().ok_or(PUBLISH_ERROR)?)?,
            path.file_name().ok_or(PUBLISH_ERROR)?,
            rustix::fs::AtFlags::empty(),
        )
        .map_err(|_| PUBLISH_ERROR)?;
        #[cfg(windows)]
        std::fs::remove_file(self.stage.path().join(path)).map_err(|_| PUBLISH_ERROR)?;
        self.files.remove(path).ok_or(PUBLISH_ERROR)?;
        Ok(())
    }
    fn check_file(&self, path: &Path) -> Result<(), ConversionError> {
        self.check_directory(path.parent().ok_or(PUBLISH_ERROR)?)?;
        if native::FileIdentity::read(&self.open_file(path)?).map_err(|_| PUBLISH_ERROR)?
            != *self.files.get(path).ok_or(PUBLISH_ERROR)?
        {
            return Err(PUBLISH_ERROR);
        }
        Ok(())
    }
    fn check_directory(&self, path: &Path) -> Result<(), ConversionError> {
        self.stage.directory_source()?;
        for ancestor in path.ancestors() {
            let directory = self.directories.get(ancestor).ok_or(PUBLISH_ERROR)?;
            if native::FileIdentity::read(&directory.held).map_err(|_| PUBLISH_ERROR)?
                != directory.identity
            {
                return Err(PUBLISH_ERROR);
            }
            if !ancestor.as_os_str().is_empty() {
                let current = self.open_directory(
                    ancestor.parent().ok_or(PUBLISH_ERROR)?,
                    ancestor.file_name().ok_or(PUBLISH_ERROR)?,
                )?;
                if native::FileIdentity::read(&current).map_err(|_| PUBLISH_ERROR)?
                    != directory.identity
                {
                    return Err(PUBLISH_ERROR);
                }
            }
        }
        Ok(())
    }
    fn directory(&self, path: &Path) -> Result<&File, ConversionError> {
        Ok(&self.directories.get(path).ok_or(PUBLISH_ERROR)?.held)
    }
    fn open_directory(&self, parent: &Path, name: &OsStr) -> Result<File, ConversionError> {
        let path = self.stage.path().join(parent).join(name);
        #[cfg(unix)]
        {
            publication::open_entry(self.directory(parent)?, name, &path, true)
        }
        #[cfg(windows)]
        {
            publication::open_directory(&path, false)
        }
    }
    fn open_file(&self, path: &Path) -> Result<File, ConversionError> {
        publication::open_entry(
            self.directory(path.parent().ok_or(PUBLISH_ERROR)?)?,
            path.file_name().ok_or(PUBLISH_ERROR)?,
            &self.stage.path().join(path),
            false,
        )
    }
    fn rename(&self, source: &Path, destination: &Path) -> Result<(), ConversionError> {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            rustix::fs::renameat_with(
                self.directory(source.parent().ok_or(PUBLISH_ERROR)?)?,
                source.file_name().ok_or(PUBLISH_ERROR)?,
                self.directory(destination.parent().ok_or(PUBLISH_ERROR)?)?,
                destination.file_name().ok_or(PUBLISH_ERROR)?,
                rustix::fs::RenameFlags::NOREPLACE,
            )
            .map_err(|_| PUBLISH_ERROR)
        }
        #[cfg(windows)]
        {
            super::atomic_move(
                &self.stage.path().join(source),
                &self.stage.path().join(destination),
            )
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = (source, destination);
            Err(PUBLISH_ERROR)
        }
    }
}
