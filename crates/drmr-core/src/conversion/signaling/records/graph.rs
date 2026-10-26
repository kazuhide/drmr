//! Resource roles come from the captured manifest graph, never from filename extensions.
use super::{ConversionError, MISMATCH};
use crate::{concurrency::Stop, conversion::FrozenSource};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    Manifest,
    Initialization,
    Media,
    /// Explicit DASH initialization/media slices may share one indexed object.
    DashObject,
}

pub(super) struct Resources {
    files: BTreeMap<PathBuf, Option<Role>>,
}

impl Resources {
    pub fn new(source: &FrozenSource, stop: &Stop) -> Result<Self, ConversionError> {
        let mut root = source.entrypoint().canonicalize().map_err(|_| MISMATCH)?;
        for _ in source.inventory().entrypoint.components() {
            root.pop();
        }
        let root = root.canonicalize().map_err(|_| MISMATCH)?;
        let mut files = BTreeMap::new();
        for file in &source.inventory().files {
            super::super::cancelled(stop)?;
            let path = root.join(&file.path).canonicalize().map_err(|_| MISMATCH)?;
            if !path.starts_with(&root) || files.insert(path, None).is_some() {
                return Err(MISMATCH);
            }
        }
        Ok(Self { files })
    }

    pub fn assign(&mut self, path: &Path, role: Role) -> Result<(), ConversionError> {
        let path = path.canonicalize().map_err(|_| MISMATCH)?;
        let previous = self.files.get_mut(&path).ok_or(MISMATCH)?;
        if previous.is_some_and(|previous| previous != role) {
            return Err(MISMATCH);
        }
        *previous = Some(role);
        Ok(())
    }

    pub fn media(&self) -> impl Iterator<Item = &Path> {
        self.files
            .iter()
            .filter_map(|(path, role)| (*role == Some(Role::Media)).then_some(path.as_path()))
    }

    pub fn complete(&self) -> Result<(), ConversionError> {
        if self.files.values().any(Option::is_none) {
            Err(MISMATCH)
        } else {
            Ok(())
        }
    }
}
