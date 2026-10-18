//! Local and S3 object storage for staging inputs and publishing outputs.
//!
//! Storage errors expose classification tokens rather than object keys, paths, or
//! provider messages. Storage operations contain no encryption or packaging logic.

use crate::batch_support::{is_relative_to, resolve};
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

mod s3;

pub use s3::{REGION, S3Access, S3StorageBackend};

/// `StorageError`: a classification token (`storage-transient`, `object-key-invalid`, …) and
/// whether the failure is worth another attempt.
#[derive(Clone, PartialEq, Eq, thiserror::Error)]
#[error("{classification}")]
pub struct StorageError {
    classification: String,
    retryable: bool,
}

impl fmt::Debug for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StorageError")
            .field("classification", &self.classification)
            .field("retryable", &self.retryable)
            .finish()
    }
}

impl StorageError {
    /// A failure that another attempt will not fix (`retryable=False`).
    pub fn new(classification: &str) -> Self {
        StorageError {
            classification: classification.to_string(),
            retryable: false,
        }
    }

    /// A transient failure (`retryable=True`).
    pub fn retryable(classification: &str) -> Self {
        StorageError {
            classification: classification.to_string(),
            retryable: true,
        }
    }

    /// The classification token (`str(exc)` in Python).
    pub fn token(&self) -> &str {
        &self.classification
    }

    /// Whether another attempt may succeed.
    pub fn is_retryable(&self) -> bool {
        self.retryable
    }
}

/// `ObjectRef`: an object key — a location only, never key material. `Debug` hides the key
/// (Python's `field(repr=False)`): keys are asset locations an error must not echo.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectRef {
    key: String,
}

impl fmt::Debug for ObjectRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ObjectRef()")
    }
}

impl ObjectRef {
    /// A validated key: relative, `/`-separated, no empty, `.` or `..` segment, no backslash,
    /// no control character, no `?` or `#`; otherwise `object-key-invalid`.
    pub fn new(key: impl Into<String>) -> Result<ObjectRef, StorageError> {
        let key = key.into();
        let invalid = key.is_empty()
            || key.starts_with('/')
            || key.contains('\\')
            || key.split('/').any(|part| matches!(part, "" | "." | ".."))
            || key.chars().any(|character| u32::from(character) < 32)
            || key.contains('?')
            || key.contains('#');
        if invalid {
            return Err(StorageError::new("object-key-invalid"));
        }
        Ok(ObjectRef { key })
    }

    /// The key.
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// `TransferPolicy`: the batching, concurrency, timeout, retry and byte budgets of object
/// transfers. Physical tuning uses bounded transfers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransferPolicy {
    /// Objects per batch (1–1000: "1000 reads → 1000 writes").
    pub batch_size: i64,
    /// Transfers at once (1–32).
    pub max_concurrency: i64,
    /// Connect and per-read timeout of one request, seconds.
    pub per_object_timeout_s: f64,
    /// Attempts per request (1–10).
    pub max_attempts: i64,
    /// Exponential backoff base, seconds.
    pub backoff_base_s: f64,
    /// Backoff cap, seconds.
    pub backoff_max_s: f64,
    /// Largest object.
    pub max_object_bytes: i64,
    /// Most objects in one listing or transfer.
    pub max_objects: i64,
    /// Most bytes staged for one asset.
    pub max_asset_bytes: i64,
}

impl Default for TransferPolicy {
    fn default() -> Self {
        TransferPolicy {
            batch_size: 1000,
            max_concurrency: 8,
            per_object_timeout_s: 30.0,
            max_attempts: 4,
            backoff_base_s: 0.5,
            backoff_max_s: 30.0,
            max_object_bytes: 1024 * 1024 * 1024,
            max_objects: 100_000,
            max_asset_bytes: 100 * 1024 * 1024 * 1024,
        }
    }
}

impl TransferPolicy {
    /// The policy if every bound holds, else `transfer-policy-invalid`.
    pub fn validate(self) -> Result<TransferPolicy, StorageError> {
        let finite = [
            self.per_object_timeout_s,
            self.backoff_base_s,
            self.backoff_max_s,
        ]
        .iter()
        .all(|value| value.is_finite());
        if !finite
            || !(1..=1000).contains(&self.batch_size)
            || !(1..=32).contains(&self.max_concurrency)
            || !(1..=10).contains(&self.max_attempts)
            || self.per_object_timeout_s <= 0.0
            || self.backoff_base_s.min(self.backoff_max_s) < 0.0
            || self
                .max_object_bytes
                .min(self.max_objects)
                .min(self.max_asset_bytes)
                <= 0
        {
            return Err(StorageError::new("transfer-policy-invalid"));
        }
        Ok(self)
    }

    pub(crate) fn chunk(&self) -> usize {
        usize::try_from(self.batch_size).expect("validated 1..=1000")
    }

    pub(crate) fn workers(&self) -> usize {
        usize::try_from(self.max_concurrency).expect("validated 1..=32")
    }

    pub(crate) fn object_cap(&self) -> u64 {
        u64::try_from(self.max_object_bytes).expect("validated positive")
    }

    pub(crate) fn asset_cap(&self) -> u64 {
        u64::try_from(self.max_asset_bytes).expect("validated positive")
    }

    pub(crate) fn object_budget(&self) -> usize {
        usize::try_from(self.max_objects).unwrap_or(usize::MAX)
    }
}

/// Where a backend lives, as the batch worker rebuilds it (`_worker_payload`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageLocation {
    /// A [`LocalStorageBackend`] rooted here.
    Local {
        /// The (resolved) root.
        root: PathBuf,
    },
    /// An [`S3StorageBackend`].
    S3 {
        /// The bucket.
        bucket: String,
        /// The region (always `ap-northeast-1`).
        region: String,
        /// An endpoint given explicitly (not from the environment), if any.
        endpoint: Option<String>,
    },
}

/// `StorageBackend`: the seam `batch` drives. Sources are only read; only output prefixes are
/// written or deleted.
pub trait StorageBackend: Send + Sync {
    /// Whether the object exists.
    fn exists(&self, object: &ObjectRef) -> Result<bool, StorageError>;
    /// Download `refs` under `dest_root` (`dest_root/<key>`), in order; byte-exact.
    fn get_many(&self, refs: &[ObjectRef], dest_root: &Path) -> Result<Vec<PathBuf>, StorageError>;
    /// Upload each local file to its key (overwriting; output prefix only).
    fn put_many(&self, items: &[(PathBuf, ObjectRef)]) -> Result<(), StorageError>;
    /// Delete every object under an output prefix.
    fn delete_prefix(&self, prefix: &ObjectRef) -> Result<(), StorageError>;
    /// Every object under a prefix.
    fn list_prefix(&self, prefix: &ObjectRef) -> Result<Vec<ObjectRef>, StorageError>;
    /// The object's size in bytes.
    fn object_size(&self, object: &ObjectRef) -> Result<u64, StorageError>;
    /// The local root (`getattr(storage, "root", None)`).
    fn root(&self) -> Option<&Path> {
        None
    }
    /// The bucket (`getattr(storage, "bucket", None)`).
    fn bucket(&self) -> Option<&str> {
        None
    }
    /// The transfer policy the backend runs with (`storage.policy`).
    fn policy(&self) -> Option<TransferPolicy> {
        None
    }
    /// How a fresh worker process rebuilds this backend; `None` for a backend it cannot
    /// rebuild (`storage-config-invalid`).
    fn location(&self) -> Option<StorageLocation> {
        None
    }
}

/// `_owned`: `object` lies strictly below the output `prefix`, else `output-prefix-invalid`.
pub(crate) fn owned(object: &ObjectRef, prefix: &str) -> Result<(), StorageError> {
    let parts: Vec<&str> = object.key().split('/').collect();
    let base: Vec<&str> = prefix.split('/').collect();
    if parts.len() <= base.len() || parts[..base.len()] != base[..] {
        return Err(StorageError::new("output-prefix-invalid"));
    }
    Ok(())
}

fn is_redirect(path: &Path) -> bool {
    #[cfg(unix)]
    {
        path.is_symlink()
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        std::fs::symlink_metadata(path)
            .is_ok_and(|metadata| metadata.file_attributes() & 0x400 != 0)
    }
}

/// `_confined`: `root/<key>` resolved inside the resolved root, with no symbolic link between
/// the root and the object; else `storage-path-invalid`.
pub(crate) fn confined(root: &Path, object: &ObjectRef) -> Result<PathBuf, StorageError> {
    #[cfg(windows)]
    crate::native::validate_windows_relative_path(object.key())
        .map_err(|_| StorageError::new("storage-path-invalid"))?;
    let root = resolve(root).map_err(|_| StorageError::new("storage-path-invalid"))?;
    let path = root.join(object.key());
    #[cfg(unix)]
    let resolved = resolve(&path).map_err(|_| StorageError::new("storage-path-invalid"))?;
    #[cfg(windows)]
    let resolved = match resolve(&path) {
        Ok(resolved) => resolved,
        // A regular-file ancestor is a failed transfer, not a path escape. The
        // lexical object key is already confined; the redirect census below
        // still rejects a link that led to that file before copying begins.
        Err(error) if error.kind() == io::ErrorKind::NotADirectory => path.clone(),
        Err(_) => return Err(StorageError::new("storage-path-invalid")),
    };
    if !is_relative_to(&resolved, &root) {
        return Err(StorageError::new("storage-path-invalid"));
    }
    let mut current = path.as_path();
    loop {
        if current == root || !is_relative_to(current, &root) {
            break;
        }
        if is_redirect(current) {
            return Err(StorageError::new("storage-path-invalid"));
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }
    Ok(path)
}

/// A local copy failure: a classification of its own or an OS error the caller classifies.
pub(crate) enum CopyError {
    Storage(StorageError),
    Io,
}

impl From<io::Error> for CopyError {
    fn from(_: io::Error) -> Self {
        CopyError::Io
    }
}

/// `_atomic_copy`: a regular, non-link `source` of at most `cap` bytes copied to a temporary
/// file beside `target`, synced, checked against `cap` again and renamed over `target`.
pub(crate) fn atomic_copy(source: &Path, target: &Path, cap: u64) -> Result<(), CopyError> {
    atomic_copy_observed(source, target, cap, &|_, _| {})
}

fn copy_step<T>(
    phase: &'static str,
    result: io::Result<T>,
    observer: &dyn Fn(&'static str, &io::Error),
) -> Result<T, CopyError> {
    result.map_err(|error| {
        observer(phase, &error);
        CopyError::Io
    })
}

fn atomic_copy_observed(
    source: &Path,
    target: &Path,
    cap: u64,
    observer: &dyn Fn(&'static str, &io::Error),
) -> Result<(), CopyError> {
    if is_redirect(source)
        || !source.is_file()
        || copy_step("source-metadata", std::fs::metadata(source), observer)?.len() > cap
    {
        return Err(CopyError::Storage(StorageError::new(
            "storage-object-invalid",
        )));
    }
    let parent = target.parent().unwrap_or_else(|| Path::new("."));
    #[cfg(unix)]
    copy_step("prepare-parent", std::fs::create_dir_all(parent), observer)?;
    #[cfg(windows)]
    copy_step(
        "prepare-parent",
        crate::native::create_private_dir_all(parent),
        observer,
    )?;
    let mut temporary = copy_step("create-temporary", transfer_file(parent), observer)?;
    let mut reader = copy_step("open-source", File::open(source), observer)?;
    copy_step(
        "copy-bytes",
        copy_chunks(&mut reader, temporary.as_file_mut()),
        observer,
    )?;
    copy_step("flush", temporary.as_file_mut().flush(), observer)?;
    copy_step("sync", temporary.as_file().sync_all(), observer)?;
    if copy_step(
        "copied-metadata",
        std::fs::metadata(temporary.path()),
        observer,
    )?
    .len()
        > cap
    {
        return Err(CopyError::Storage(StorageError::new(
            "storage-object-invalid",
        )));
    }
    copy_step(
        "persist",
        temporary.persist(target).map_err(|error| error.error),
        observer,
    )?;
    Ok(())
}

/// `tempfile.mkstemp(prefix=".transfer-", dir=parent)`: removed unless persisted.
pub(crate) fn transfer_file(parent: &Path) -> io::Result<tempfile::NamedTempFile> {
    let mut builder = tempfile::Builder::new();
    builder.prefix(".transfer-").rand_bytes(8);
    #[cfg(unix)]
    {
        builder.tempfile_in(parent)
    }
    #[cfg(windows)]
    {
        builder.make_in(parent, crate::native::create_private_new)
    }
}

/// `shutil.copyfileobj(reader, writer, length=1 MiB)`.
fn copy_chunks(reader: &mut impl Read, writer: &mut impl Write) -> io::Result<()> {
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let count = match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        writer.write_all(&buffer[..count])?;
    }
}

/// `_parallel`: at most `max_objects` items, in chunks of `batch_size`, each chunk on a pool
/// of `max_concurrency` workers; the results in item order. After a failure no new item of
/// the chunk starts (items already running finish) and no later chunk starts; the error is
/// the earliest failure in item order, as `executor.map` raises it.
pub(crate) fn parallel<T, R, F>(
    items: &[T],
    function: F,
    policy: &TransferPolicy,
) -> Result<Vec<R>, StorageError>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> Result<R, StorageError> + Sync,
{
    if items.len() > policy.object_budget() {
        return Err(StorageError::new("storage-object-budget"));
    }
    let mut results = Vec::with_capacity(items.len());
    for chunk in items.chunks(policy.chunk()) {
        results.extend(run_chunk(chunk, &function, policy.workers())?);
    }
    Ok(results)
}

fn run_chunk<T, R, F>(chunk: &[T], function: &F, workers: usize) -> Result<Vec<R>, StorageError>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> Result<R, StorageError> + Sync,
{
    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let slots: Vec<Mutex<Option<Result<R, StorageError>>>> =
        chunk.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|scope| {
        for _ in 0..workers.min(chunk.len()) {
            scope.spawn(|| {
                while !stop.load(Ordering::SeqCst) {
                    let index = next.fetch_add(1, Ordering::SeqCst);
                    let Some(item) = chunk.get(index) else {
                        break;
                    };
                    let result = function(item);
                    if result.is_err() {
                        stop.store(true, Ordering::SeqCst);
                    }
                    *slots[index].lock().expect("slot lock") = Some(result);
                }
            });
        }
    });
    let mut results = Vec::with_capacity(chunk.len());
    for slot in slots {
        match slot.into_inner().expect("slot lock") {
            Some(Ok(result)) => results.push(result),
            Some(Err(error)) => return Err(error),
            // Items are handed out in order, so every item before the first failure ran.
            None => unreachable!("an item after the first failure is never awaited"),
        }
    }
    Ok(results)
}

/// `LocalStorageBackend`: `root/<key>` is the object store. Staging copies (never links);
/// "upload" copies into `root/<output prefix>`. `Debug` hides the root.
#[derive(Clone)]
pub struct LocalStorageBackend {
    root: PathBuf,
    policy: TransferPolicy,
    output_prefix: String,
}

impl fmt::Debug for LocalStorageBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LocalStorageBackend")
            .field("policy", &self.policy)
            .field("output_prefix", &self.output_prefix)
            .finish_non_exhaustive()
    }
}

impl LocalStorageBackend {
    /// `LocalStorageBackend(root)` with the default policy and the `output` prefix.
    pub fn new(root: &Path) -> Result<LocalStorageBackend, StorageError> {
        LocalStorageBackend::with(root, TransferPolicy::default(), "output")
    }

    /// `LocalStorageBackend(root, policy, output_prefix)`.
    pub fn with(
        root: &Path,
        policy: TransferPolicy,
        output_prefix: &str,
    ) -> Result<LocalStorageBackend, StorageError> {
        let policy = policy.validate()?;
        ObjectRef::new(output_prefix)?;
        Ok(LocalStorageBackend {
            root: resolve(root).map_err(|_| StorageError::new("storage-path-invalid"))?,
            policy,
            output_prefix: output_prefix.to_string(),
        })
    }

    /// The output prefix.
    pub fn output_prefix(&self) -> &str {
        &self.output_prefix
    }

    fn walk(&self, directory: &Path, paths: &mut Vec<ObjectRef>) -> Result<(), StorageError> {
        let Ok(entries) = std::fs::read_dir(directory) else {
            // pathlib's rglob skips a directory it may not list.
            return Ok(());
        };
        let mut entries: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        entries.sort();
        let mut directories = Vec::new();
        for path in entries {
            if is_redirect(&path) {
                return Err(StorageError::new("storage-path-invalid"));
            }
            if path.is_file() {
                let relative = path
                    .strip_prefix(&self.root)
                    .map_err(|_| StorageError::new("storage-path-invalid"))?;
                let key = relative
                    .to_str()
                    .ok_or_else(|| StorageError::new("object-key-invalid"))?;
                #[cfg(windows)]
                let key = key.replace('\\', "/");
                paths.push(ObjectRef::new(key)?);
                if paths.len() > self.policy.object_budget() {
                    return Err(StorageError::new("storage-object-budget"));
                }
            } else if path.is_dir() {
                directories.push(path);
            }
        }
        for directory in directories {
            self.walk(&directory, paths)?;
        }
        Ok(())
    }
}

impl StorageBackend for LocalStorageBackend {
    fn exists(&self, object: &ObjectRef) -> Result<bool, StorageError> {
        Ok(confined(&self.root, object)?.is_file())
    }

    fn object_size(&self, object: &ObjectRef) -> Result<u64, StorageError> {
        std::fs::metadata(confined(&self.root, object)?)
            .map(|metadata| metadata.len())
            .map_err(|_| StorageError::new("storage-read-failed"))
    }

    fn list_prefix(&self, prefix: &ObjectRef) -> Result<Vec<ObjectRef>, StorageError> {
        let base = confined(&self.root, prefix)?;
        if base.is_file() {
            return Ok(vec![prefix.clone()]);
        }
        let mut paths = Vec::new();
        if base.is_dir() {
            self.walk(&base, &mut paths)?;
        }
        paths.sort_by(|left, right| left.key().cmp(right.key()));
        Ok(paths)
    }

    fn get_many(&self, refs: &[ObjectRef], dest_root: &Path) -> Result<Vec<PathBuf>, StorageError> {
        let dest_root =
            resolve(dest_root).map_err(|_| StorageError::new("storage-path-invalid"))?;
        if is_relative_to(&dest_root, &self.root) || is_relative_to(&self.root, &dest_root) {
            return Err(StorageError::new("storage-stage-overlap"));
        }
        let mut total: u64 = 0;
        for object in refs {
            let size = std::fs::metadata(confined(&self.root, object)?)
                .map_err(|_| StorageError::new("storage-read-failed"))?
                .len();
            total = total.saturating_add(size);
        }
        if total > self.policy.asset_cap() {
            return Err(StorageError::new("storage-byte-budget"));
        }
        let copy = |object: &ObjectRef| -> Result<PathBuf, StorageError> {
            let target = confined(&dest_root, object)?;
            let source = confined(&self.root, object)?;
            match atomic_copy(&source, &target, self.policy.object_cap()) {
                Ok(()) => Ok(target),
                Err(CopyError::Storage(error)) => Err(error),
                Err(CopyError::Io) => Err(StorageError::new("storage-read-failed")),
            }
        };
        parallel(refs, copy, &self.policy)
    }

    fn put_many(&self, items: &[(PathBuf, ObjectRef)]) -> Result<(), StorageError> {
        let copy = |(source, object): &(PathBuf, ObjectRef)| -> Result<(), StorageError> {
            owned(object, &self.output_prefix)?;
            let target = confined(&self.root, object)?;
            match atomic_copy(source, &target, self.policy.object_cap()) {
                Ok(()) => Ok(()),
                Err(CopyError::Storage(error)) => Err(error),
                Err(CopyError::Io) => Err(StorageError::new("storage-write-failed")),
            }
        };
        parallel(items, copy, &self.policy).map(|_| ())
    }

    fn delete_prefix(&self, prefix: &ObjectRef) -> Result<(), StorageError> {
        owned(prefix, &self.output_prefix)?;
        let path = confined(&self.root, prefix)?;
        let result = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else if path.exists() {
            std::fs::remove_file(&path)
        } else {
            Ok(())
        };
        result.map_err(|_| StorageError::new("storage-delete-failed"))
    }

    fn root(&self) -> Option<&Path> {
        Some(&self.root)
    }

    fn policy(&self) -> Option<TransferPolicy> {
        Some(self.policy)
    }

    fn location(&self) -> Option<StorageLocation> {
        Some(StorageLocation::Local {
            root: self.root.clone(),
        })
    }
}

#[cfg(all(test, windows))]
mod native_copy_tests {
    use super::*;
    use std::sync::Mutex;

    #[test]
    fn a_live_directory_creator_conflicts_with_publication_but_a_reader_does_not() {
        let temporary = crate::native::private_tempdir().unwrap();
        let source = temporary.path().join("source");
        std::fs::write(&source, b"unchanged synthetic source").unwrap();
        let parent = temporary.path().join("destination");
        // Hold the same DELETE-capable handle that ensure retains while it
        // validates a newly visible directory. This control forces the former
        // setup/publication overlap without relying on thread scheduling.
        let creator = private_fs::PrivateDir::ensure(&parent).unwrap();
        let target = parent.join("target");
        let diagnostics = Mutex::new(Vec::new());
        let result = atomic_copy_observed(&source, &target, 1024, &|phase, error| {
            diagnostics
                .lock()
                .unwrap()
                .push((phase, error.raw_os_error()));
        });
        assert!(result.is_err());
        assert_eq!(diagnostics.into_inner().unwrap(), [("persist", Some(32))]);
        assert!(!target.exists());
        assert_eq!(
            std::fs::read(&source).unwrap(),
            b"unchanged synthetic source"
        );
        drop(creator);

        // Once setup is complete, ordinary parent verification carries no
        // DELETE access and can coexist with the same atomic publication.
        let reader = private_fs::PrivateDir::open(&parent).unwrap();
        assert!(atomic_copy(&source, &target, 1024).is_ok());
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"unchanged synthetic source"
        );
        drop(reader);
    }

    #[test]
    fn parallel_nested_private_copies_preserve_every_source_and_target() {
        let temporary = crate::native::private_tempdir().unwrap();
        let input = temporary.path().join("input");
        let output = temporary.path().join("output");
        crate::native::create_private_dir_all(&output).unwrap();
        let _output_guard = crate::native::pin_private_directory(&output).unwrap();
        let mut transfers = Vec::new();
        for track in ["audio/a", "video/v"] {
            for index in 0..8 {
                let key = format!("cdn.example.test/title/{track}/segment_{index}.m4s");
                let source = input.join(&key);
                std::fs::create_dir_all(source.parent().unwrap()).unwrap();
                let bytes = format!("synthetic-media:{track}:{index}").into_bytes();
                std::fs::write(&source, &bytes).unwrap();
                transfers.push((source, output.join(key), bytes));
            }
        }
        std::thread::scope(|scope| {
            let handles: Vec<_> = transfers
                .iter()
                .map(|(source, target, bytes)| {
                    scope.spawn(move || {
                        let diagnostics = Mutex::new(Vec::new());
                        let result = atomic_copy_observed(source, target, 1024, &|phase, error| {
                            diagnostics.lock().unwrap().push((
                                phase,
                                error.kind(),
                                error.raw_os_error(),
                            ));
                        });
                        assert!(
                            result.is_ok(),
                            "copy phases: {:?}",
                            diagnostics.into_inner().unwrap()
                        );
                        assert_eq!(std::fs::read(source).unwrap(), *bytes);
                        assert_eq!(std::fs::read(target).unwrap(), *bytes);
                    })
                })
                .collect();
            for handle in handles {
                handle.join().unwrap();
            }
        });
        // Private reads pin their ancestors without write/delete sharing. Wait
        // for all legitimate directory-creation handles before validating them.
        for (_, target, _) in &transfers {
            let held = crate::native::open_private_read(target).unwrap();
            private_fs::verify(&held).unwrap();
        }
    }
}
