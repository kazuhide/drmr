//! Discover copied manifests in order, then copy independently reserved leaf resources.
use super::{
    CHANGED, ConversionError, FileIdentity, FrozenSource, INVALID, LIMIT, MAX_FILES,
    MAX_GRAPH_BYTES, MAX_PATH_BYTES, OriginalGuard, Path, PathBuf, SourceFile, SourceInventory,
    Stop, cancelled,
    directories::Directories,
    directory_identity,
    execution::{Control, Execution},
    files,
    graph::{self, Need},
    graph_digest,
    io::Object,
    path_alias, unsafe_link, validate,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

type Copied = (SourceFile, FileIdentity);

struct Capture<'a, O> {
    root: &'a Path,
    expected: Option<&'a SourceInventory>,
    directories: Directories,
    execution: &'a Execution,
    stop: &'a Stop,
    observer: &'a O,
    seen: BTreeMap<PathBuf, Option<usize>>,
    aliases: BTreeSet<String>,
    objects: Vec<Option<Object>>,
    copied: BTreeMap<PathBuf, Copied>,
    bytes: u64,
    path_bytes: usize,
}

impl<'a, O: Fn(&Path, u64) + Sync> Capture<'a, O> {
    fn new(
        root: &'a Path,
        stage: &'a Path,
        expected: Option<&'a SourceInventory>,
        execution: &'a Execution,
        stop: &'a Stop,
        observer: &'a O,
    ) -> Result<Self, ConversionError> {
        Ok(Self {
            root,
            expected,
            directories: Directories::new(stage)?,
            execution,
            stop,
            observer,
            seen: BTreeMap::new(),
            aliases: BTreeSet::new(),
            objects: Vec::new(),
            copied: BTreeMap::new(),
            bytes: 0,
            path_bytes: 0,
        })
    }

    fn expected(&self, path: &Path) -> Result<Option<&'a SourceFile>, ConversionError> {
        let Some(inventory) = self.expected else {
            return Ok(None);
        };
        inventory
            .files
            .binary_search_by(|file| file.path.as_path().cmp(path))
            .map(|index| Some(&inventory.files[index]))
            .map_err(|_| INVALID)
    }

    fn reference(&mut self, path: &Path, need: Need) -> Result<(), ConversionError> {
        cancelled(self.stop)?;
        if let Some(pending) = self.seen.get(path).copied() {
            if need == Need::DiscoveryNow
                && let Some(index) = pending
            {
                let object = self
                    .objects
                    .get_mut(index)
                    .and_then(Option::take)
                    .ok_or(INVALID)?;
                self.copy_now(&object)?;
                self.seen.insert(path.to_path_buf(), None);
            }
            return Ok(());
        }
        if self.seen.len() >= MAX_FILES {
            return Err(LIMIT);
        }
        self.path_bytes = self
            .path_bytes
            .checked_add(path.as_os_str().len())
            .filter(|bytes| *bytes <= MAX_PATH_BYTES)
            .ok_or(LIMIT)?;
        if !self.aliases.insert(path_alias(path)?) {
            return Err(INVALID);
        }
        let object = Object::inspect(
            self.root,
            path,
            self.expected(path)?,
            MAX_GRAPH_BYTES - self.bytes,
        )?;
        self.bytes = self
            .bytes
            .checked_add(object.bytes)
            .filter(|bytes| *bytes <= MAX_GRAPH_BYTES)
            .ok_or(LIMIT)?;
        if need == Need::DiscoveryNow {
            self.copy_now(&object)?;
            self.seen.insert(path.to_path_buf(), None);
        } else {
            self.seen
                .insert(path.to_path_buf(), Some(self.objects.len()));
            self.objects.push(Some(object));
        }
        Ok(())
    }

    fn copy_now(&mut self, object: &Object) -> Result<(), ConversionError> {
        let expected = self.expected(&object.path)?;
        let destination = self.directories.prepare(&object.path)?;
        let control = Control::new(self.stop);
        let copied = self.execution.one(control, || {
            object.copy_to(self.root, &destination, expected, control, &mut |bytes| {
                (self.observer)(&object.path, bytes);
            })
        })?;
        self.copied.insert(object.path.clone(), copied);
        Ok(())
    }

    fn finish(
        mut self,
    ) -> Result<(Vec<SourceFile>, BTreeMap<PathBuf, FileIdentity>), ConversionError> {
        if self
            .expected
            .is_some_and(|inventory| self.seen.len() != inventory.files.len())
        {
            return Err(INVALID);
        }
        // Discovery is closed. Release its duplicate path indexes before opening
        // worker streams; only preflighted objects remain queued.
        self.seen.clear();
        self.aliases.clear();
        let objects = std::mem::take(&mut self.objects)
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        for object in &objects {
            cancelled(self.stop)?;
            self.directories.prepare(&object.path)?;
        }
        let copied = self
            .execution
            .run(self.stop, objects.len(), |index, control| {
                let object = &objects[index];
                let destination = self.directories.prepared(&object.path)?;
                object.copy_to(
                    self.root,
                    &destination,
                    self.expected(&object.path)?,
                    control,
                    &mut |bytes| (self.observer)(&object.path, bytes),
                )
            })?;
        drop(objects);
        for (file, identity) in copied {
            self.copied.insert(file.path.clone(), (file, identity));
        }
        self.directories.finish(self.stop)?;
        let mut files = Vec::with_capacity(self.copied.len());
        let mut identities = BTreeMap::new();
        for (path, (file, identity)) in self.copied {
            files.push(file);
            identities.insert(path, identity);
        }
        Ok((files, identities))
    }
}

pub(super) fn capture<O: Fn(&Path, u64) + Sync>(
    input: &Path,
    stop: &Stop,
    execution: Execution,
    observer: &O,
) -> Result<FrozenSource, ConversionError> {
    cancelled(stop)?;
    execution.check_budget()?;
    let metadata = std::fs::symlink_metadata(input).map_err(|_| CHANGED)?;
    if !metadata.is_file() || unsafe_link(&metadata) {
        return Err(INVALID);
    }
    let input = input.canonicalize().map_err(|_| CHANGED)?;
    let root = input.parent().ok_or(INVALID)?.to_path_buf();
    let entrypoint = PathBuf::from(input.file_name().ok_or(INVALID)?);
    let root_identity = directory_identity(&root)?;
    let stage = files::Stage::new(None)?;
    let mut capture = Capture::new(&root, stage.path(), None, &execution, stop, observer)?;
    graph::walk(stage.path(), &entrypoint, stop, |path, need| {
        capture.reference(path, need)
    })?;
    let (files, identities) = capture.finish()?;
    let mut inventory = SourceInventory {
        schema_version: 1,
        root,
        entrypoint,
        files,
        graph_digest: String::new(),
    };
    inventory.graph_digest = graph_digest(&inventory)?;
    validate(&inventory)?;
    complete(stage, inventory, root_identity, identities, stop, execution)
}

pub(super) fn from_inventory<O: Fn(&Path, u64) + Sync>(
    inventory: &SourceInventory,
    stop: &Stop,
    execution: Execution,
    observer: &O,
) -> Result<FrozenSource, ConversionError> {
    cancelled(stop)?;
    validate(inventory)?;
    execution.check_budget()?;
    let root_identity = directory_identity(&inventory.root)?;
    let stage = files::Stage::new(None)?;
    let mut capture = Capture::new(
        &inventory.root,
        stage.path(),
        Some(inventory),
        &execution,
        stop,
        observer,
    )?;
    graph::walk(stage.path(), &inventory.entrypoint, stop, |path, need| {
        capture.reference(path, need)
    })?;
    let (_, identities) = capture.finish()?;
    complete(
        stage,
        inventory.clone(),
        root_identity,
        identities,
        stop,
        execution,
    )
}

fn complete(
    stage: files::Stage,
    inventory: SourceInventory,
    root_identity: FileIdentity,
    identities: BTreeMap<PathBuf, FileIdentity>,
    stop: &Stop,
    execution: Execution,
) -> Result<FrozenSource, ConversionError> {
    let original = Arc::new(OriginalGuard {
        inventory,
        root_identity,
        identities,
        stop: stop.clone(),
        execution,
    });
    original.check()?;
    let entrypoint = stage.path().join(&original.inventory.entrypoint);
    Ok(FrozenSource {
        _stage: stage,
        entrypoint,
        original,
    })
}
