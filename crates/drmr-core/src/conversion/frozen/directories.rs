//! Prepare each private parent once and retain its identity through the final census.
use super::{
    ConversionError, FileIdentity, Path, PathBuf, Stop, cancelled, directory_identity, files,
    relative, unsafe_link,
};
use std::collections::BTreeMap;

const UNSAFE: ConversionError = ConversionError("temporary-file-unavailable");

pub(super) struct Directories {
    root: PathBuf,
    identities: BTreeMap<PathBuf, FileIdentity>,
}

impl Directories {
    pub fn new(root: &Path) -> Result<Self, ConversionError> {
        let metadata = std::fs::symlink_metadata(root).map_err(|_| UNSAFE)?;
        if !metadata.is_dir() || unsafe_link(&metadata) {
            return Err(UNSAFE);
        }
        // Windows stages can use a normal drive path while canonicalize returns
        // its verbatim form. Original-source roots still require canonical paths.
        let root = root.canonicalize().map_err(|_| UNSAFE)?;
        let identity = directory_identity(&root).map_err(|_| UNSAFE)?;
        Ok(Self {
            identities: BTreeMap::from([(root.clone(), identity)]),
            root,
        })
    }

    pub fn prepare(&mut self, relative_path: &Path) -> Result<PathBuf, ConversionError> {
        relative(relative_path)?;
        let destination = self.root.join(relative_path);
        let parent = destination.parent().ok_or(UNSAFE)?;
        if !self.identities.contains_key(parent) {
            files::create_dir(parent)?;
            self.identities.insert(
                parent.to_path_buf(),
                directory_identity(parent).map_err(|_| UNSAFE)?,
            );
        }
        if Some(&directory_identity(parent).map_err(|_| UNSAFE)?) != self.identities.get(parent) {
            return Err(UNSAFE);
        }
        Ok(destination)
    }

    pub fn finish(&self, stop: &Stop) -> Result<(), ConversionError> {
        for (path, identity) in &self.identities {
            cancelled(stop)?;
            if directory_identity(path).map_err(|_| UNSAFE)? != *identity {
                return Err(UNSAFE);
            }
        }
        // Reusing a parent never bypasses the complete private-file census.
        // Every writer has closed before the caller reaches this barrier.
        files::seal(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn absolute_stage_roots_are_stored_canonically() {
        let stage = files::Stage::new(None).unwrap();
        assert!(stage.path().is_absolute());
        let mut directories = Directories::new(stage.path()).unwrap();
        assert_eq!(directories.root, stage.path().canonicalize().unwrap());
        let file = directories.prepare(Path::new("variant/first.ts")).unwrap();
        files::create_new(&file)
            .unwrap()
            .write_all(b"first")
            .unwrap();
        directories.finish(&Stop::new()).unwrap();
        assert_eq!(std::fs::read(file).unwrap(), b"first");
    }

    #[test]
    #[cfg(windows)]
    fn normal_windows_stage_paths_accept_their_verbatim_identity() {
        let stage = files::Stage::new(None).unwrap();
        let canonical = stage.path().canonicalize().unwrap();
        let normal = PathBuf::from(canonical.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
        assert!(normal.is_absolute());
        assert_ne!(normal, canonical);
        assert_eq!(
            directory_identity(&normal).err().unwrap(),
            super::super::CHANGED
        );
        let mut directories = Directories::new(&normal).unwrap();
        assert_eq!(directories.root, canonical);
        let file = directories.prepare(Path::new("first.ts")).unwrap();
        files::create_new(&file)
            .unwrap()
            .write_all(b"first")
            .unwrap();
        directories.finish(&Stop::new()).unwrap();
        assert_eq!(std::fs::read(file).unwrap(), b"first");
    }

    #[test]
    fn parent_replacement_is_not_accepted_by_a_preparation_cache() {
        let stage = files::Stage::new(None).unwrap();
        let mut directories = Directories::new(stage.path()).unwrap();
        directories.prepare(Path::new("variant/first.ts")).unwrap();
        std::fs::rename(stage.path().join("variant"), stage.path().join("retained")).unwrap();
        files::create_dir(&stage.path().join("variant")).unwrap();
        assert_eq!(
            directories
                .prepare(Path::new("variant/second.ts"))
                .unwrap_err(),
            UNSAFE
        );
        assert!(!stage.path().join("variant/second.ts").exists());
    }

    #[test]
    #[cfg(unix)]
    fn cached_parent_still_requires_a_complete_final_census() {
        use std::os::unix::fs::symlink;
        let stage = files::Stage::new(None).unwrap();
        let mut directories = Directories::new(stage.path()).unwrap();
        let first = directories.prepare(Path::new("variant/first.ts")).unwrap();
        files::create_new(&first)
            .unwrap()
            .write_all(b"first")
            .unwrap();
        let link = stage.path().join("variant/unexpected.ts");
        symlink(&first, &link).unwrap();
        let second = directories.prepare(Path::new("variant/second.ts")).unwrap();
        files::create_new(&second)
            .unwrap()
            .write_all(b"second")
            .unwrap();
        assert!(directories.finish(&Stop::new()).is_err());
        assert_eq!(std::fs::read(&first).unwrap(), b"first");
        assert_eq!(std::fs::read(&second).unwrap(), b"second");
        std::fs::remove_file(link).unwrap();
        directories.finish(&Stop::new()).unwrap();
    }
}
