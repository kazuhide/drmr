//! Unpublished, creation-bound copies retained until the validator tree is reaped.

use super::{ConversionError, digest, files};
use crate::native::{self, FileIdentity};
use std::{
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
};

const FAILED: ConversionError = ConversionError("manifest-validation-workspace-failed");
const CHANGED: ConversionError = ConversionError("manifest-validation-workspace-changed");

pub(super) struct Workspace {
    stage: files::UnpublishedStage,
    copies: Vec<Copy>,
}

struct Copy {
    relative: PathBuf,
    identity: FileIdentity,
    bytes: u64,
    sha256: String,
}

impl Workspace {
    pub(super) fn new() -> Result<Self, ConversionError> {
        Ok(Self {
            stage: files::UnpublishedStage::new(None).map_err(|_| FAILED)?,
            copies: Vec::new(),
        })
    }

    pub(super) fn path(&self) -> &Path {
        self.stage.path()
    }

    pub(super) fn write(
        &mut self,
        relative: &Path,
        bytes: &[u8],
    ) -> Result<PathBuf, ConversionError> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(FAILED);
        }
        self.check_root()?;
        let path = self.path().join(relative);
        if let Some(parent) = path.parent() {
            native::create_private_dir_all(parent).map_err(|_| FAILED)?;
        }
        let mut file = native::create_private_new(&path).map_err(|_| FAILED)?;
        let identity = FileIdentity::read(&file).map_err(|_| FAILED)?;
        file.write_all(bytes).map_err(|_| FAILED)?;
        self.copies.push(Copy {
            relative: relative.to_owned(),
            identity,
            bytes: bytes.len() as u64,
            sha256: digest(bytes),
        });
        self.check_root()?;
        Ok(path)
    }

    fn check_root(&self) -> Result<(), ConversionError> {
        // This only checks the retained stage's creation identities. The stage
        // remains unpublished; validator copies are never committed to output.
        self.stage.directory_source().map_err(|_| CHANGED)?;
        Ok(())
    }

    pub(super) fn check(&self) -> Result<(), ConversionError> {
        self.check_root()?;
        for copy in &self.copies {
            self.check_copy(copy)?;
        }
        self.check_root()
    }

    pub(super) fn check_file(&self, relative: &Path) -> Result<(), ConversionError> {
        self.check_root()?;
        let copy = self
            .copies
            .iter()
            .find(|copy| copy.relative == relative)
            .ok_or(CHANGED)?;
        self.check_copy(copy)?;
        self.check_root()
    }

    fn check_copy(&self, copy: &Copy) -> Result<(), ConversionError> {
        let mut path = self.path().to_owned();
        for component in copy.relative.components() {
            path.push(component);
            let metadata = std::fs::symlink_metadata(&path).map_err(|_| CHANGED)?;
            if metadata.file_type().is_symlink() {
                return Err(CHANGED);
            }
        }
        let file = native::open_private_read(&path).map_err(|_| CHANGED)?;
        if FileIdentity::read(&file).map_err(|_| CHANGED)? != copy.identity {
            return Err(CHANGED);
        }
        let mut bytes = Vec::new();
        file.take(copy.bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| CHANGED)?;
        if bytes.len() as u64 != copy.bytes || digest(&bytes) != copy.sha256 {
            return Err(CHANGED);
        }
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn first_copy_failure_preserves_a_replaced_private_root() {
        let temp = tempfile::tempdir().unwrap();
        let stage = files::UnpublishedStage::new(Some(temp.path())).unwrap();
        let mut workspace = Workspace {
            stage,
            copies: Vec::new(),
        };
        let path = workspace.path().to_owned();
        let moved = temp.path().join("original-payload");
        std::fs::write(path.join("owned"), b"original helper bytes").unwrap();
        std::fs::rename(&path, &moved).unwrap();
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(path.join("foreign-sentinel"), b"must remain").unwrap();

        let error = workspace
            .write(Path::new("manifest.mpd"), b"not written")
            .unwrap_err();
        assert_eq!(error.0, CHANGED.0);
        assert!(workspace.copies.is_empty());
        drop(workspace);

        assert_eq!(
            std::fs::read(path.join("foreign-sentinel")).unwrap(),
            b"must remain"
        );
        assert!(!path.join("manifest.mpd").exists());
        assert_eq!(
            std::fs::read(moved.join("owned")).unwrap(),
            b"original helper bytes"
        );
    }
}
