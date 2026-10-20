//! Byte-bound private input graphs for saved conversion intent.
mod graph;

use super::{ConversionError, files};
use crate::{concurrency::Stop, native::FileIdentity};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read as _,
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use unicode_normalization::UnicodeNormalization as _;

const INVALID: ConversionError = ConversionError("source-inventory-invalid");
const CHANGED: ConversionError = ConversionError("planned-source-changed");
const LIMIT: ConversionError = ConversionError("source-graph-limit");
const MAX_FILES: usize = 100_130;
const MAX_DEPTH: usize = 128;
const MAX_GRAPH_BYTES: u64 = super::MAX_MEDIA_BYTES;
const MAX_PATH_BYTES: usize = 16 * 1024 * 1024;

/// One source object. Paths are confined to the inventory root and contain no URL.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceFile {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
}
impl std::fmt::Debug for SourceFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceFile")
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

/// A local source graph, not cleanup ownership or authority to run an executable.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInventory {
    pub schema_version: u32,
    pub root: PathBuf,
    pub entrypoint: PathBuf,
    pub files: Vec<SourceFile>,
    pub graph_digest: String,
}
impl std::fmt::Debug for SourceInventory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceInventory")
            .field("schema_version", &self.schema_version)
            .field("files", &self.files.len())
            .finish_non_exhaustive()
    }
}

/// Private copies of exactly the referenced originals. No source file is ever removed.
pub struct FrozenSource {
    _stage: files::Stage,
    entrypoint: PathBuf,
    original: Arc<OriginalGuard>,
}
impl std::fmt::Debug for FrozenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrozenSource")
            .field("inventory", self.inventory())
            .finish_non_exhaustive()
    }
}
impl FrozenSource {
    /// Capture only the input's finite local resource graph, leaving unrelated files alone.
    pub fn capture(input: &Path, stop: &Stop) -> Result<Self, ConversionError> {
        cancelled(stop)?;
        let metadata = std::fs::symlink_metadata(input).map_err(|_| CHANGED)?;
        if !metadata.is_file() || unsafe_link(&metadata) {
            return Err(INVALID);
        }
        let input = input.canonicalize().map_err(|_| CHANGED)?;
        let root = input.parent().ok_or(INVALID)?.to_path_buf();
        let entrypoint = PathBuf::from(input.file_name().ok_or(INVALID)?);
        let root_identity = directory_identity(&root)?;
        let stage = files::Stage::new(None)?;
        let mut copied = BTreeMap::new();
        let mut identities = BTreeMap::new();
        let mut total = 0u64;
        let mut path_bytes = 0usize;
        graph::walk(stage.path(), &entrypoint, stop, |path| {
            if !copied.contains_key(path) {
                if copied.len() >= MAX_FILES {
                    return Err(LIMIT);
                }
                path_bytes = path_bytes
                    .checked_add(path.as_os_str().len())
                    .filter(|bytes| *bytes <= MAX_PATH_BYTES)
                    .ok_or(LIMIT)?;
                let (file, identity) = copy_object(
                    &root,
                    stage.path(),
                    path,
                    None,
                    MAX_GRAPH_BYTES - total,
                    stop,
                    &mut |_, _| {},
                )?;
                total = total
                    .checked_add(file.bytes)
                    .filter(|size| *size <= MAX_GRAPH_BYTES)
                    .ok_or(LIMIT)?;
                identities.insert(path.to_path_buf(), identity);
                copied.insert(path.to_path_buf(), file);
            }
            Ok(())
        })?;
        let mut inventory = SourceInventory {
            schema_version: 1,
            root,
            entrypoint,
            files: copied.into_values().collect(),
            graph_digest: String::new(),
        };
        inventory.graph_digest = graph_digest(&inventory)?;
        validate(&inventory)?;
        let original = Arc::new(OriginalGuard {
            inventory,
            root_identity,
            identities,
            stop: stop.clone(),
        });
        original.check()?;
        let entrypoint = stage.path().join(&original.inventory.entrypoint);
        Ok(Self {
            _stage: stage,
            entrypoint,
            original,
        })
    }

    /// Reopen untrusted saved inventory into fresh copies, checking the copied bytes before use.
    pub fn from_inventory(
        inventory: &SourceInventory,
        stop: &Stop,
    ) -> Result<Self, ConversionError> {
        Self::from_inventory_observed(inventory, stop, |_, _| {})
    }

    fn from_inventory_observed(
        inventory: &SourceInventory,
        stop: &Stop,
        mut after_chunk: impl FnMut(&Path, u64),
    ) -> Result<Self, ConversionError> {
        cancelled(stop)?;
        validate(inventory)?;
        let root_identity = directory_identity(&inventory.root)?;
        let stage = files::Stage::new(None)?;
        let mut identities = BTreeMap::new();
        for file in &inventory.files {
            let (_, identity) = copy_object(
                &inventory.root,
                stage.path(),
                &file.path,
                Some(file),
                file.bytes,
                stop,
                &mut after_chunk,
            )?;
            identities.insert(file.path.clone(), identity);
        }
        let mut referenced = std::collections::BTreeSet::new();
        graph::walk(stage.path(), &inventory.entrypoint, stop, |path| {
            if !identities.contains_key(path) {
                return Err(INVALID);
            }
            referenced.insert(path.to_path_buf());
            Ok(())
        })?;
        if referenced.len() != inventory.files.len() {
            return Err(INVALID);
        }
        let original = Arc::new(OriginalGuard {
            inventory: inventory.clone(),
            root_identity,
            identities,
            stop: stop.clone(),
        });
        original.check()?;
        let entrypoint = stage.path().join(&original.inventory.entrypoint);
        Ok(Self {
            _stage: stage,
            entrypoint,
            original,
        })
    }

    pub fn entrypoint(&self) -> &Path {
        &self.entrypoint
    }
    pub fn inventory(&self) -> &SourceInventory {
        &self.original.inventory
    }
    /// Revalidate the retained originals; this never grants permission to delete them.
    pub fn check_original(&self) -> Result<(), ConversionError> {
        self.original.check()
    }
    pub(super) fn guard(&self) -> Arc<OriginalGuard> {
        self.original.clone()
    }
}

impl super::Converter {
    /// Carry original-source checks through measurements and final atomic publication.
    pub(super) fn using_frozen_source(&self, source: &FrozenSource) -> Self {
        Self {
            backend: self.backend.clone(),
            maximum_workers: self.maximum_workers,
            downloaded: self.downloaded.clone(),
            frozen_source: Some(source.guard()),
            planned_destination: self.planned_destination.clone(),
            output_signaling: self.output_signaling.clone(),
        }
    }
    pub(super) fn check_frozen_source(&self) -> Result<(), ConversionError> {
        self.frozen_source
            .as_ref()
            .map_or(Ok(()), |guard| guard.check())
    }
}

pub(super) struct OriginalGuard {
    inventory: SourceInventory,
    root_identity: FileIdentity,
    identities: BTreeMap<PathBuf, FileIdentity>,
    stop: Stop,
}
impl OriginalGuard {
    pub(super) fn check(&self) -> Result<(), ConversionError> {
        cancelled(&self.stop)?;
        if directory_identity(&self.inventory.root)? != self.root_identity {
            return Err(CHANGED);
        }
        for expected in &self.inventory.files {
            let mut file = open_confined(&self.inventory.root, &expected.path)?;
            if FileIdentity::read(&file).map_err(|_| CHANGED)? != self.identities[&expected.path] {
                return Err(CHANGED);
            }
            let (hash, bytes) = transfer(
                &mut file,
                &mut std::io::sink(),
                &self.stop,
                expected.bytes,
                &mut |_| {},
            )?;
            if bytes != expected.bytes || hash != expected.sha256 {
                return Err(CHANGED);
            }
        }
        if directory_identity(&self.inventory.root)? != self.root_identity {
            return Err(CHANGED);
        }
        Ok(())
    }
}

fn cancelled(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}
fn relative(path: &Path) -> Result<(), ConversionError> {
    let text = path.to_str().ok_or(INVALID)?;
    if text.is_empty()
        || text.len() > 4096
        || text.contains([':', '?'])
        || text.chars().any(char::is_control)
        || path.components().count() > MAX_DEPTH
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(INVALID);
    }
    #[cfg(not(windows))]
    if text.contains('\\') {
        return Err(INVALID);
    }
    // Windows aliases are invalid even when an inventory is produced on another platform.
    for part in path.components() {
        if let Component::Normal(name) = part {
            #[cfg(windows)]
            crate::native::validate_windows_relative_path(name.to_str().ok_or(INVALID)?)
                .map_err(|_| INVALID)?;
            #[cfg(not(windows))]
            {
                let name = name.to_str().ok_or(INVALID)?;
                let stem = name
                    .split('.')
                    .next()
                    .unwrap_or("")
                    .trim_end_matches(' ')
                    .to_ascii_uppercase();
                if name.ends_with(['.', ' '])
                    || name.contains(['<', '>', '"', '|', '*'])
                    || matches!(
                        stem.as_str(),
                        "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$"
                    )
                    || stem
                        .strip_prefix("COM")
                        .or_else(|| stem.strip_prefix("LPT"))
                        .is_some_and(|suffix| {
                            matches!(
                                suffix,
                                "1" | "2"
                                    | "3"
                                    | "4"
                                    | "5"
                                    | "6"
                                    | "7"
                                    | "8"
                                    | "9"
                                    | "¹"
                                    | "²"
                                    | "³"
                            )
                        })
                {
                    return Err(INVALID);
                }
            }
        }
    }
    Ok(())
}
pub(super) fn validate(inventory: &SourceInventory) -> Result<(), ConversionError> {
    if inventory.schema_version != 1
        || !inventory.root.is_absolute()
        || inventory.files.is_empty()
        || inventory.files.len() > MAX_FILES
        || inventory.root.as_os_str().len() > 32_768
        || inventory
            .root
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(INVALID);
    }
    relative(&inventory.entrypoint)?;
    let mut total = 0u64;
    let mut path_bytes = 0usize;
    let mut previous: Option<&Path> = None;
    let mut aliases = std::collections::BTreeSet::new();
    for file in &inventory.files {
        relative(&file.path)?;
        path_bytes = path_bytes
            .checked_add(file.path.as_os_str().len())
            .filter(|bytes| *bytes <= MAX_PATH_BYTES)
            .ok_or(LIMIT)?;
        if previous.is_some_and(|previous| previous >= file.path.as_path())
            || file.bytes > super::MAX_MEDIA_BYTES
            || file.sha256.len() != 64
            || !file
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(INVALID);
        }
        let alias: String = file
            .path
            .to_str()
            .ok_or(INVALID)?
            .nfc()
            .flat_map(char::to_lowercase)
            .collect();
        if !aliases.insert(alias) {
            return Err(INVALID);
        }
        total = total
            .checked_add(file.bytes)
            .filter(|size| *size <= MAX_GRAPH_BYTES)
            .ok_or(LIMIT)?;
        previous = Some(&file.path);
    }
    if !inventory
        .files
        .iter()
        .any(|file| file.path == inventory.entrypoint)
        || graph_digest(inventory)? != inventory.graph_digest
    {
        return Err(INVALID);
    }
    Ok(())
}
fn graph_digest(inventory: &SourceInventory) -> Result<String, ConversionError> {
    let mut hash = Sha256::new();
    hash.update(b"drmr-source-inventory-v1\0");
    for path in [&inventory.root, &inventory.entrypoint] {
        let text = path.to_str().ok_or(INVALID)?.as_bytes();
        hash.update((text.len() as u64).to_be_bytes());
        hash.update(text);
    }
    for file in &inventory.files {
        let text = file.path.to_str().ok_or(INVALID)?.as_bytes();
        hash.update((text.len() as u64).to_be_bytes());
        hash.update(text);
        hash.update(file.bytes.to_be_bytes());
        hash.update(file.sha256.as_bytes());
    }
    Ok(hex::encode(hash.finalize()))
}

fn unsafe_link(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
fn directory_identity(path: &Path) -> Result<FileIdentity, ConversionError> {
    if path.canonicalize().map_err(|_| CHANGED)? != path {
        return Err(CHANGED);
    }
    let metadata = std::fs::symlink_metadata(path).map_err(|_| CHANGED)?;
    if !metadata.is_dir() || unsafe_link(&metadata) {
        return Err(CHANGED);
    }
    let mut options = std::fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.read(true).custom_flags(
            (rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK)
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
    }
    let file = options.open(path).map_err(|_| CHANGED)?;
    FileIdentity::read(&file).map_err(|_| CHANGED)
}
fn open_confined(root: &Path, relative_path: &Path) -> Result<File, ConversionError> {
    relative(relative_path)?;
    directory_identity(root)?;
    let mut current = root.to_path_buf();
    for (index, part) in relative_path.components().enumerate() {
        current.push(part);
        let metadata = std::fs::symlink_metadata(&current).map_err(|_| CHANGED)?;
        if unsafe_link(&metadata)
            || (index + 1 < relative_path.components().count() && !metadata.is_dir())
        {
            return Err(CHANGED);
        }
    }
    let file = files::regular_input(&current).map_err(|_| CHANGED)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if file.metadata().map_err(|_| CHANGED)?.nlink() != 1 {
            return Err(CHANGED);
        }
    }
    Ok(file)
}
fn copy_object(
    root: &Path,
    stage: &Path,
    relative_path: &Path,
    expected: Option<&SourceFile>,
    remaining: u64,
    stop: &Stop,
    after_chunk: &mut impl FnMut(&Path, u64),
) -> Result<(SourceFile, FileIdentity), ConversionError> {
    cancelled(stop)?;
    let mut input = open_confined(root, relative_path)?;
    let identity = FileIdentity::read(&input).map_err(|_| CHANGED)?;
    let size = input.metadata().map_err(|_| CHANGED)?.len();
    if size > remaining {
        return Err(LIMIT);
    }
    if expected.is_some_and(|file| file.bytes != size) {
        return Err(CHANGED);
    }
    let destination = stage.join(relative_path);
    files::create_dir(destination.parent().ok_or(INVALID)?)?;
    let mut output = files::create_new(&destination)?;
    let (sha256, bytes) = transfer(&mut input, &mut output, stop, size, &mut |bytes| {
        after_chunk(relative_path, bytes);
    })?;
    output.sync_all().map_err(|_| CHANGED)?;
    if bytes != size || expected.is_some_and(|file| file.sha256 != sha256) {
        return Err(CHANGED);
    }
    if FileIdentity::read(&open_confined(root, relative_path)?).map_err(|_| CHANGED)? != identity {
        return Err(CHANGED);
    }
    Ok((
        SourceFile {
            path: relative_path.to_path_buf(),
            bytes,
            sha256,
        },
        identity,
    ))
}
fn transfer(
    input: &mut File,
    output: &mut impl std::io::Write,
    stop: &Stop,
    maximum: u64,
    after_chunk: &mut impl FnMut(u64),
) -> Result<(String, u64), ConversionError> {
    let mut bytes = 0u64;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    loop {
        cancelled(stop)?;
        let count = input.read(&mut buffer).map_err(|_| CHANGED)?;
        if count == 0 {
            break;
        }
        bytes = bytes
            .checked_add(count as u64)
            .filter(|bytes| *bytes <= maximum && *bytes <= super::MAX_MEDIA_BYTES)
            .ok_or(CHANGED)?;
        output.write_all(&buffer[..count]).map_err(|_| CHANGED)?;
        hash.update(&buffer[..count]);
        after_chunk(bytes);
    }
    Ok((hex::encode(hash.finalize()), bytes))
}

#[cfg(test)]
mod tests;
