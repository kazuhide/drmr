//! Explicit, owner-private storage of completed original resources between attempts.
//! Receipts detect changes under a trusted account; they are not authentication.
use super::super::{ConversionError, files};
use crate::{
    concurrency::Stop,
    native::FileIdentity,
    source::{self, ByteRange, FetchPolicy},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fmt,
    fs::File,
    io::{Cursor, Read, Write},
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

mod filesystem;
mod provenance;
use filesystem::Directory;
#[cfg(test)]
mod tests;

const UNSAFE: ConversionError = ConversionError("download-resume-cache-unsafe");
const CHANGED: ConversionError = ConversionError("download-source-changed");
const CLEANUP: ConversionError = ConversionError("download-resume-cleanup-unconfirmed");
const IO: ConversionError = ConversionError("download-source-unavailable");
const LIMIT: ConversionError = ConversionError("download-graph-limit");
const MAX_RECEIPT: u64 = 8192;
const MAX_RECORDS: u64 = 100_000;
const CHUNK: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ResourceKind {
    Manifest,
    Media,
    Mp4,
}

#[derive(Clone, Copy)]
pub(super) struct CompletedResource<'a> {
    pub(super) requested: &'a str,
    pub(super) range: Option<ByteRange>,
    pub(super) kind: ResourceKind,
    pub(super) fetched: &'a source::Fetched,
    pub(super) etag: Option<&'a str>,
    pub(super) path: &'a Path,
    pub(super) created: FileIdentity,
}

#[derive(Clone)]
pub(super) struct CachedResource {
    pub(super) kind: ResourceKind,
    pub(super) sha256: String,
    pub(super) size: u64,
    pub(super) effective_url_sha256: String,
    pub(super) etag: Option<String>,
    path: PathBuf,
    record: Record,
    cache_identity: FileIdentity,
    directory_identity: FileIdentity,
    blob_identity: FileIdentity,
    receipt_identity: FileIdentity,
}
impl fmt::Debug for CachedResource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CachedResource")
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    version: u8,
    source_sha256: String,
    destination_sha256: String,
    parent_identity: String,
    root_identity: String,
    lock_identity: String,
    self_identity: String,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    version: u8,
    key: String,
    context_sha256: String,
    requested_url_sha256: String,
    range: Option<[u64; 2]>,
    kind: ResourceKind,
    sha256: String,
    size: u64,
    effective_url_sha256: String,
    etag: Option<String>,
    directory_identity: String,
    blob_identity: String,
    receipt_identity: String,
}
#[derive(Default)]
struct Totals {
    records: u64,
    bytes: u64,
    manifests: u64,
}
struct Limits {
    resource: u64,
    playlist: u64,
    total: u64,
    records: u64,
    playlists: u64,
}

pub(super) struct ResumeCache {
    parent: Directory,
    root: Directory,
    lock: Option<File>,
    context: Context,
    context_sha256: String,
    limits: Limits,
    totals: Mutex<Totals>,
    cleaned: bool,
}
impl fmt::Debug for ResumeCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ResumeCache")
    }
}

impl ResumeCache {
    pub(super) fn open(
        download: &Path,
        input_url: &str,
        output: Option<&Path>,
        policy: &FetchPolicy,
        stop: &Stop,
    ) -> Result<Self, ConversionError> {
        stopped(stop)?;
        bounded_url(input_url)?;
        let download = destination(download)?;
        let parent_path = download.parent().ok_or(UNSAFE)?;
        let parent = Directory::open(parent_path, false)?;
        let mut leaf = OsString::from(".");
        leaf.push(download.file_name().ok_or(UNSAFE)?);
        leaf.push(".drmr-download");
        let path = parent_path.join(leaf);
        check_overlap(&path, output)?;
        let source_sha256 = fingerprint(input_url.as_bytes());
        let destination_sha256 = path_fingerprint(&download);
        let exists = match std::fs::symlink_metadata(&path) {
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => return Err(UNSAFE),
        };
        if !exists {
            stopped(stop)?;
            let stage = files::Stage::new(Some(parent_path))?;
            let pending = stage.private_root().join("cache");
            let root = Directory::create_new(&pending)?;
            let lock =
                crate::native::create_private_new(&pending.join("lock")).map_err(|_| UNSAFE)?;
            provenance::create(&lock)?;
            let mut context_file = crate::native::create_private_new(&pending.join("context.json"))
                .map_err(|_| UNSAFE)?;
            provenance::create(&context_file)?;
            let context = Context {
                version: 2,
                source_sha256: source_sha256.clone(),
                destination_sha256: destination_sha256.clone(),
                parent_identity: parent.identity().stable_token(),
                root_identity: root.token(),
                lock_identity: provenance::token(&lock)?,
                self_identity: provenance::token(&context_file)?,
            };
            write_json(&mut context_file, &context)?;
            lock.sync_all().map_err(|_| IO)?;
            drop(context_file);
            drop(lock);
            drop(root);
            parent.check()?;
            stopped(stop)?;
            // Every object already has the strict private-file policy. The
            // ordinary media publication seal would add directory inheritance.
            files::atomic_move(&pending, &path).map_err(|_| UNSAFE)?;
        }
        stopped(stop)?;
        let root = Directory::open(&path, true)?;
        // Existing roots never gain a missing marker or lock through admission.
        let context: Context = read_json(&root.file("context.json", false)?)?;
        let lock = root.file("lock", true)?;
        lock.try_lock()
            .map_err(|_| ConversionError("download-resume-in-use"))?;
        let context_again: Context = read_json(&root.file("context.json", false)?)?;
        if context != context_again
            || context.version != 2
            || context.source_sha256 != source_sha256
            || context.destination_sha256 != destination_sha256
            || context.parent_identity != parent.identity().stable_token()
            || context.root_identity != root.token()
            || context.lock_identity != provenance::token(&lock)?
            || context.self_identity != provenance::token(&root.file("context.json", false)?)?
            || lock.metadata().map_err(|_| UNSAFE)?.len() != 0
        {
            return Err(UNSAFE);
        }
        let context_sha256 = fingerprint(&serde_json::to_vec(&context).map_err(|_| IO)?);
        let limits = Limits {
            resource: policy.max_resource_bytes,
            playlist: policy.max_playlist_bytes.min(4 * 1024 * 1024),
            total: policy.max_total_bytes,
            records: policy
                .max_total_resources
                .unwrap_or(MAX_RECORDS)
                .min(MAX_RECORDS),
            playlists: policy.max_playlists.min(65),
        };
        let mut cache = Self {
            parent,
            root,
            lock: Some(lock),
            context,
            context_sha256,
            limits,
            totals: Mutex::new(Totals::default()),
            cleaned: false,
        };
        cache.check()?;
        cache.check_output(output)?;
        let totals = cache.scan(false, stop)?;
        *cache.totals.get_mut().map_err(|_| UNSAFE)? = totals;
        Ok(cache)
    }

    #[cfg(test)]
    pub(super) fn directory(&self) -> &Path {
        &self.root.path
    }
    pub(super) fn retained_bytes(&self) -> u64 {
        // One receipt/serde tree, a copy/hash buffer, and directory scan frames.
        256 * 1024 + self.parent.estimated_bytes() + self.root.estimated_bytes()
    }
    pub(super) fn check(&self) -> Result<(), ConversionError> {
        if self.cleaned {
            return Err(UNSAFE);
        }
        self.parent.check()?;
        self.root.check()?;
        let context_file = self.root.file("context.json", false)?;
        let lock_file = self.root.shared_file("lock")?;
        if read_json::<Context>(&context_file)? != self.context
            || provenance::token(&context_file)? != self.context.self_identity
            || provenance::token(&lock_file)? != self.context.lock_identity
            || provenance::token(self.lock.as_ref().ok_or(UNSAFE)?)? != self.context.lock_identity
            || lock_file.metadata().map_err(|_| UNSAFE)?.len() != 0
        {
            return Err(UNSAFE);
        }
        Ok(())
    }
    pub(super) fn lookup(
        &self,
        url: &str,
        range: Option<ByteRange>,
    ) -> Result<Option<CachedResource>, ConversionError> {
        self.check()?;
        bounded_url(url)?;
        let key = resource_key(url, range)?;
        let path = self.root.path.join(&key);
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(UNSAFE),
            Ok(_) => {
                let candidate = self.read_candidate(&key)?;
                if candidate.record.requested_url_sha256 != fingerprint(url.as_bytes())
                    || candidate.record.range != range.map(|range| [range.offset, range.length])
                {
                    return Err(UNSAFE);
                }
                Ok(Some(candidate))
            }
        }
    }

    pub(super) fn store_file(
        &self,
        completed: CompletedResource<'_>,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        let mut input = crate::native::open_private_read(completed.path).map_err(|_| CHANGED)?;
        if FileIdentity::read(&input).map_err(|_| CHANGED)? != completed.created
            || input.metadata().map_err(|_| CHANGED)?.len() != completed.fetched.size
        {
            return Err(CHANGED);
        }
        let metadata = self.record(
            completed.requested,
            completed.range,
            completed.kind,
            completed.fetched,
            completed.etag,
        )?;
        self.store(&metadata, &mut input, stop, || {
            let current = crate::native::open_private_read(completed.path).map_err(|_| CHANGED)?;
            if FileIdentity::read(&current).map_err(|_| CHANGED)? != completed.created {
                return Err(CHANGED);
            }
            Ok(())
        })
    }
    pub(super) fn store_manifest(
        &self,
        url: &str,
        effective_url: &str,
        bytes: &[u8],
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        let fetched = source::Fetched {
            effective: effective_url.to_owned(),
            sha256: fingerprint(bytes),
            size: bytes.len() as u64,
        };
        let metadata = self.record(url, None, ResourceKind::Manifest, &fetched, None)?;
        self.store(&metadata, &mut Cursor::new(bytes), stop, || Ok(()))
    }
    fn record(
        &self,
        url: &str,
        range: Option<ByteRange>,
        kind: ResourceKind,
        fetched: &source::Fetched,
        etag: Option<&str>,
    ) -> Result<Record, ConversionError> {
        bounded_url(url)?;
        bounded_url(&fetched.effective)?;
        if !hexadecimal(&fetched.sha256, 64) || !valid_etag(etag) {
            return Err(UNSAFE);
        }
        let record = Record {
            version: 2,
            key: resource_key(url, range)?,
            context_sha256: self.context_sha256.clone(),
            requested_url_sha256: fingerprint(url.as_bytes()),
            range: range.map(|range| [range.offset, range.length]),
            kind,
            sha256: fetched.sha256.clone(),
            size: fetched.size,
            effective_url_sha256: fingerprint(fetched.effective.as_bytes()),
            etag: etag.map(str::to_owned),
            directory_identity: String::new(),
            blob_identity: String::new(),
            receipt_identity: String::new(),
        };
        self.validate_record(&record, false)?;
        Ok(record)
    }
    fn store(
        &self,
        metadata: &Record,
        reader: &mut impl Read,
        stop: &Stop,
        source_check: impl FnOnce() -> Result<(), ConversionError>,
    ) -> Result<(), ConversionError> {
        let mut totals = self.totals.lock().map_err(|_| UNSAFE)?;
        self.check()?;
        stopped(stop)?;
        if let Some(previous) = self.lookup_by_record(metadata)? {
            if !same_representation(&previous.record, metadata) {
                return Err(CHANGED);
            }
            let digest = copy(reader, None, metadata.size, stop)?;
            if digest != metadata.sha256 {
                return Err(CHANGED);
            }
            source_check()?;
            self.verify_candidate(&previous, stop)?;
            return Ok(());
        }
        let next = self.add_totals(&totals, metadata)?;
        // Pending state is outside the stable cache; a killed attempt cannot leave
        // unreceipted partial files among reusable completed resources.
        let stage = files::Stage::new(Some(&self.parent.path))?;
        let pending = stage.private_root().join("resource");
        let pending_directory = Directory::create_new(&pending)?;
        let mut output =
            crate::native::create_private_new(&pending.join("data")).map_err(|_| IO)?;
        provenance::create(&output)?;
        let digest = copy(reader, Some(&mut output), metadata.size, stop)?;
        if digest != metadata.sha256 {
            return Err(CHANGED);
        }
        source_check()?;
        output.sync_all().map_err(|_| IO)?;
        let blob_identity = provenance::token(&output)?;
        let mut receipt =
            crate::native::create_private_new(&pending.join("receipt.json")).map_err(|_| IO)?;
        provenance::create(&receipt)?;
        let mut record = metadata.clone();
        record.directory_identity = pending_directory.token();
        record.blob_identity = blob_identity;
        record.receipt_identity = provenance::token(&receipt)?;
        write_json(&mut receipt, &record)?;
        drop(receipt);
        drop(output);
        drop(pending_directory);
        self.check()?;
        stopped(stop)?;
        files::atomic_move(&pending, &self.root.path.join(&record.key)).map_err(|_| IO)?;
        let committed = self.read_candidate(&record.key)?;
        if committed.record != record {
            return Err(CHANGED);
        }
        *totals = next;
        Ok(())
    }
    fn lookup_by_record(&self, record: &Record) -> Result<Option<CachedResource>, ConversionError> {
        match std::fs::symlink_metadata(self.root.path.join(&record.key)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(UNSAFE),
            Ok(_) => self.read_candidate(&record.key).map(Some),
        }
    }

    pub(super) fn materialize(
        &self,
        candidate: &CachedResource,
        destination: &Path,
        stop: &Stop,
    ) -> Result<FileIdentity, ConversionError> {
        self.check()?;
        stopped(stop)?;
        let directory = self.candidate_directory(candidate)?;
        let mut input = directory.file("data", false)?;
        let mut output = crate::native::create_private_new(destination).map_err(|_| IO)?;
        let created = FileIdentity::read(&output).map_err(|_| IO)?;
        let result = (|| {
            if copy(&mut input, Some(&mut output), candidate.size, stop)? != candidate.sha256 {
                return Err(CHANGED);
            }
            self.candidate_directory(candidate)?;
            output.sync_all().map_err(|_| IO)?;
            self.check()?;
            Ok(created)
        })();
        drop(output);
        if result.is_err() {
            let own = crate::native::open_private_read(destination)
                .and_then(|file| FileIdentity::read(&file))
                .is_ok_and(|current| current == created);
            if own {
                let _ = std::fs::remove_file(destination);
            }
        }
        result
    }

    /// Only after all acquisition workers have joined and the retained source seal passed.
    pub(super) fn cleanup(&mut self, stop: &Stop) -> Result<(), ConversionError> {
        if self.cleaned {
            return Ok(());
        }
        self.check().map_err(|_| CLEANUP)?;
        // Refuse before deleting anything if any unknown, replaced, or modified
        // object is present; repeat identity/hash checks at each deletion boundary.
        self.scan(true, stop).map_err(|error| {
            if error.0 == "cancelled" {
                error
            } else {
                CLEANUP
            }
        })?;
        for entry in std::fs::read_dir(&self.root.path).map_err(|_| CLEANUP)? {
            let entry = entry.map_err(|_| CLEANUP)?;
            let name = entry.file_name();
            let name = name.to_str().ok_or(CLEANUP)?;
            if matches!(name, "context.json" | "lock") {
                continue;
            }
            let candidate = self.read_candidate(name).map_err(|_| CLEANUP)?;
            self.verify_candidate(&candidate, stop).map_err(|error| {
                if error.0 == "cancelled" {
                    error
                } else {
                    CLEANUP
                }
            })?;
            let child = self.candidate_directory(&candidate).map_err(|_| CLEANUP)?;
            child.remove_file("data", &candidate.record.blob_identity)?;
            child.remove_file("receipt.json", &candidate.record.receipt_identity)?;
            drop(child);
            self.root
                .remove_child(OsStr::new(name), &candidate.record.directory_identity)?;
        }
        self.check().map_err(|_| CLEANUP)?;
        let context_id = provenance::token(&self.root.file("context.json", false)?)?;
        let lock_id = provenance::token(self.lock.as_ref().ok_or(CLEANUP)?)?;
        self.root.remove_file("context.json", &context_id)?;
        // Retain the lock through removal. On Windows, DELETE sharing must be
        // released only after every other protected cache object has gone.
        #[cfg(unix)]
        self.root.remove_file("lock", &lock_id)?;
        #[cfg(windows)]
        {
            self.lock.take();
            self.root.remove_file("lock", &lock_id)?;
        }
        let root_id = self.root.token();
        let name = self.root.path.file_name().ok_or(CLEANUP)?.to_owned();
        self.root.release();
        self.parent.remove_child(&name, &root_id)?;
        self.lock.take();
        self.cleaned = true;
        Ok(())
    }

    fn candidate_directory(
        &self,
        candidate: &CachedResource,
    ) -> Result<Directory, ConversionError> {
        if candidate.cache_identity != self.root.identity()
            || candidate.path != self.root.path.join(&candidate.record.key)
            || candidate.kind != candidate.record.kind
            || candidate.sha256 != candidate.record.sha256
            || candidate.size != candidate.record.size
            || candidate.effective_url_sha256 != candidate.record.effective_url_sha256
            || candidate.etag != candidate.record.etag
        {
            return Err(CHANGED);
        }
        let current = self.read_candidate(&candidate.record.key)?;
        if current.record != candidate.record
            || current.directory_identity != candidate.directory_identity
            || current.blob_identity != candidate.blob_identity
            || current.receipt_identity != candidate.receipt_identity
        {
            return Err(CHANGED);
        }
        self.root.child(&candidate.record.key)
    }
    fn verify_candidate(
        &self,
        candidate: &CachedResource,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        let directory = self.candidate_directory(candidate)?;
        let mut input = directory.file("data", false)?;
        if copy(&mut input, None, candidate.size, stop)? != candidate.sha256 {
            return Err(CHANGED);
        }
        self.candidate_directory(candidate)?;
        Ok(())
    }
    fn read_candidate(&self, key: &str) -> Result<CachedResource, ConversionError> {
        if !hexadecimal(key, 64) {
            return Err(UNSAFE);
        }
        let directory = self.root.child(key)?;
        let receipt = directory.file("receipt.json", false)?;
        let record: Record = read_json(&receipt)?;
        self.validate_record(&record, true)?;
        let blob = directory.file("data", false)?;
        let directory_identity = directory.identity();
        let blob_identity = FileIdentity::read(&blob).map_err(|_| UNSAFE)?;
        let receipt_identity = FileIdentity::read(&receipt).map_err(|_| UNSAFE)?;
        if record.key != key
            || record.directory_identity != directory.token()
            || record.blob_identity != provenance::token(&blob)?
            || record.receipt_identity != provenance::token(&receipt)?
            || blob.metadata().map_err(|_| UNSAFE)?.len() != record.size
        {
            return Err(CHANGED);
        }
        let mut count = 0;
        for entry in std::fs::read_dir(&directory.path).map_err(|_| UNSAFE)? {
            count += 1;
            if count > 2
                || !matches!(
                    entry.map_err(|_| UNSAFE)?.file_name().to_str(),
                    Some("data" | "receipt.json")
                )
            {
                return Err(UNSAFE);
            }
        }
        if count != 2 {
            return Err(UNSAFE);
        }
        Ok(CachedResource {
            kind: record.kind,
            sha256: record.sha256.clone(),
            size: record.size,
            effective_url_sha256: record.effective_url_sha256.clone(),
            etag: record.etag.clone(),
            path: directory.path.clone(),
            record,
            cache_identity: self.root.identity(),
            directory_identity,
            blob_identity,
            receipt_identity,
        })
    }
    fn validate_record(&self, record: &Record, persisted: bool) -> Result<(), ConversionError> {
        let cap = if record.kind == ResourceKind::Manifest {
            self.limits.playlist
        } else {
            self.limits.resource
        };
        if record.version != 2
            || record.context_sha256 != self.context_sha256
            || !hexadecimal(&record.key, 64)
            || !hexadecimal(&record.requested_url_sha256, 64)
            || !hexadecimal(&record.effective_url_sha256, 64)
            || !hexadecimal(&record.sha256, 64)
            || record.size == 0
            || record.size > cap
            || !valid_etag(record.etag.as_deref())
            || record
                .range
                .is_some_and(|range| range[1] == 0 || range[0].checked_add(range[1]).is_none())
            || persisted
                && [
                    &record.directory_identity,
                    &record.blob_identity,
                    &record.receipt_identity,
                ]
                .iter()
                .any(|value| !hexadecimal(value, provenance::TOKEN_HEX_LENGTH))
        {
            return Err(UNSAFE);
        }
        Ok(())
    }
    fn add_totals(&self, current: &Totals, record: &Record) -> Result<Totals, ConversionError> {
        let next = Totals {
            records: current.records.checked_add(1).ok_or(LIMIT)?,
            bytes: current.bytes.checked_add(record.size).ok_or(LIMIT)?,
            manifests: current
                .manifests
                .checked_add(u64::from(record.kind == ResourceKind::Manifest))
                .ok_or(LIMIT)?,
        };
        if next.records > self.limits.records
            || next.bytes > self.limits.total
            || next.manifests > self.limits.playlists
        {
            return Err(LIMIT);
        }
        Ok(next)
    }
    fn scan(&self, hashes: bool, stop: &Stop) -> Result<Totals, ConversionError> {
        let mut totals = Totals::default();
        let mut count = 0_u64;
        for entry in std::fs::read_dir(&self.root.path).map_err(|_| UNSAFE)? {
            stopped(stop)?;
            count = count
                .checked_add(1)
                .filter(|count| *count <= MAX_RECORDS + 2)
                .ok_or(LIMIT)?;
            let entry = entry.map_err(|_| UNSAFE)?;
            let name = entry.file_name();
            let name = name.to_str().ok_or(UNSAFE)?;
            if matches!(name, "context.json" | "lock") {
                continue;
            }
            let candidate = self.read_candidate(name)?;
            totals = self.add_totals(&totals, &candidate.record)?;
            if hashes {
                self.verify_candidate(&candidate, stop)?;
            }
        }
        self.check()?;
        Ok(totals)
    }
    fn check_output(&self, output: Option<&Path>) -> Result<(), ConversionError> {
        check_overlap(&self.root.path, output)?;
        if let Some(output) = output {
            let output = destination(output)?;
            if contains(self.root.identity(), &output)? {
                return Err(ConversionError("download-output-overlap"));
            }
            if let Ok(metadata) = std::fs::symlink_metadata(&output) {
                if metadata.is_dir() {
                    let output_id =
                        FileIdentity::read(&filesystem::open_directory(&output, false)?)
                            .map_err(|_| UNSAFE)?;
                    if contains(output_id, &self.root.path)? {
                        return Err(ConversionError("download-output-overlap"));
                    }
                } else {
                    return Err(ConversionError("output-exists-or-unsafe"));
                }
            }
        }
        Ok(())
    }
}

fn same_representation(left: &Record, right: &Record) -> bool {
    left.key == right.key
        && left.context_sha256 == right.context_sha256
        && left.requested_url_sha256 == right.requested_url_sha256
        && left.range == right.range
        && left.kind == right.kind
        && left.sha256 == right.sha256
        && left.size == right.size
        && left.effective_url_sha256 == right.effective_url_sha256
        && left.etag == right.etag
}
fn stopped(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}
fn copy(
    input: &mut impl Read,
    mut output: Option<&mut File>,
    expected: u64,
    stop: &Stop,
) -> Result<String, ConversionError> {
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = vec![0; CHUNK];
    loop {
        stopped(stop)?;
        let count = input.read(&mut buffer).map_err(|_| IO)?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .filter(|size| *size <= expected)
            .ok_or(CHANGED)?;
        hash.update(&buffer[..count]);
        if let Some(output) = output.as_deref_mut() {
            output.write_all(&buffer[..count]).map_err(|_| IO)?;
        }
    }
    if size != expected {
        return Err(CHANGED);
    }
    Ok(hex::encode(hash.finalize()))
}
fn write_json(file: &mut File, value: &impl Serialize) -> Result<(), ConversionError> {
    let bytes = serde_json::to_vec(value).map_err(|_| IO)?;
    if bytes.len() as u64 > MAX_RECEIPT {
        return Err(LIMIT);
    }
    file.write_all(&bytes).map_err(|_| IO)?;
    file.sync_all().map_err(|_| IO)
}
fn read_json<T: serde::de::DeserializeOwned>(file: &File) -> Result<T, ConversionError> {
    if file.metadata().map_err(|_| UNSAFE)?.len() > MAX_RECEIPT {
        return Err(UNSAFE);
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| UNSAFE)?;
    if bytes.len() as u64 > MAX_RECEIPT {
        return Err(UNSAFE);
    }
    serde_json::from_slice(&bytes).map_err(|_| UNSAFE)
}
fn fingerprint(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn hexadecimal(text: &str, length: usize) -> bool {
    text.len() == length
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn valid_etag(value: Option<&str>) -> bool {
    value.is_none_or(|value| source::StrongEtag::parse(value).is_some())
}
fn bounded_url(url: &str) -> Result<(), ConversionError> {
    if url.is_empty() || url.len() > 4096 || url.chars().any(char::is_control) {
        Err(UNSAFE)
    } else {
        Ok(())
    }
}
fn resource_key(url: &str, range: Option<ByteRange>) -> Result<String, ConversionError> {
    if range
        .is_some_and(|range| range.length == 0 || range.offset.checked_add(range.length).is_none())
    {
        return Err(UNSAFE);
    }
    let mut hash = Sha256::new();
    hash.update(b"drmr-download-resource-v1\0");
    hash.update(url.as_bytes());
    hash.update([u8::from(range.is_some())]);
    if let Some(range) = range {
        hash.update(range.offset.to_be_bytes());
        hash.update(range.length.to_be_bytes());
    }
    Ok(hex::encode(hash.finalize()))
}
fn path_fingerprint(path: &Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        fingerprint(path.as_os_str().as_bytes())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt as _;
        let mut hash = Sha256::new();
        for word in path.as_os_str().encode_wide() {
            hash.update(word.to_le_bytes());
        }
        hex::encode(hash.finalize())
    }
    #[cfg(not(any(unix, windows)))]
    fingerprint(path.to_string_lossy().as_bytes())
}
fn destination(path: &Path) -> Result<PathBuf, ConversionError> {
    let name = path.file_name().ok_or(UNSAFE)?;
    if Path::new(name)
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(UNSAFE);
    }
    #[cfg(windows)]
    crate::native::validate_windows_relative_path(name.to_str().ok_or(UNSAFE)?)
        .map_err(|_| UNSAFE)?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent.canonicalize().map_err(|_| UNSAFE)?;
    Ok(parent.join(name))
}
fn check_overlap(cache: &Path, output: Option<&Path>) -> Result<(), ConversionError> {
    if let Some(output) = output {
        let output = destination(output)?;
        if cache.starts_with(&output) || output.starts_with(cache) {
            return Err(ConversionError("download-output-overlap"));
        }
    }
    Ok(())
}
fn contains(identity: FileIdentity, path: &Path) -> Result<bool, ConversionError> {
    for (depth, ancestor) in path.ancestors().enumerate() {
        if depth >= 256 {
            return Err(UNSAFE);
        }
        match std::fs::symlink_metadata(ancestor) {
            Ok(info) if info.is_dir() => {
                if FileIdentity::read(&filesystem::open_directory(ancestor, false)?)
                    .map_err(|_| UNSAFE)?
                    == identity
                {
                    return Ok(true);
                }
            }
            Ok(info) if info.is_file() => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(UNSAFE),
        }
    }
    Ok(false)
}
