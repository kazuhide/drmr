//! Internal catalog-run generation receipts. This is not a media verification API.

use crate::{native, pipeline::AssetResult};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

/// Private server-to-producer request; never included in public reports or tool arguments.
pub const REQUEST_ENV: &str = "DRMR_CATALOG_RECEIPT";
const MAX_JSON: u64 = 16 * 1024 * 1024;
const MAX_FILES: usize = 100_000;
const MAX_BYTES: u64 = 1 << 40;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u8,
    nonce: String,
    asset_id: String,
    manifest: String,
    files: BTreeMap<String, Fingerprint>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Fingerprint {
    size: u64,
    sha256: String,
}

/// A validated receipt's current output membership. No source paths or key material.
pub struct Verified {
    /// The local DASH manifest relative to the asset output directory.
    pub manifest: String,
    files: BTreeMap<String, Fingerprint>,
    bindings: BTreeMap<String, Stamp>,
}

#[derive(PartialEq, Eq)]
struct Stamp {
    identity: native::FileIdentity,
    size: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    changed: (i64, i64),
}

impl Verified {
    /// Whether this exact resource belongs to the completed generation.
    pub fn contains(&self, relative: &str) -> bool {
        self.files.contains_key(relative)
    }
    /// Check the qualified objects without rehashing media on ordinary polls.
    /// This detects object/size/change-metadata replacement, not a malicious same-user
    /// actor able to restore every filesystem change indicator.
    pub fn current(&self, root: &Path) -> bool {
        self.current_with(root, &|| false)
    }

    /// Current-binding check with the owned server shutdown signal.
    pub fn current_with(&self, root: &Path, cancelled: &dyn Fn() -> bool) -> bool {
        self.bindings.iter().all(|(name, old)| {
            if cancelled() {
                return false;
            }
            reader(root, name)
                .and_then(|f| stamp(&f))
                .is_ok_and(|now| &now == old)
        })
    }
}

fn invalid() -> io::Error {
    io::Error::other("catalog-receipt-unavailable")
}

/// Validate a private receipt against the expected operation and every current output byte.
pub fn verify(path: &Path, nonce: &str, asset_id: &str, root: &Path) -> io::Result<Verified> {
    verify_with(path, nonce, asset_id, root, &|| false)
}

/// Internal server shutdown hook; cancellation never yields a verified result.
pub fn verify_with(
    path: &Path,
    nonce: &str,
    asset_id: &str,
    root: &Path,
    cancelled: &dyn Fn() -> bool,
) -> io::Result<Verified> {
    let budget = ReadControl { cancelled };
    budget.check()?;
    let bytes = bounded(native::open_private_read(path)?, MAX_JSON)?;
    let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if receipt.version != 1
        || receipt.nonce != nonce
        || receipt.asset_id != asset_id
        || receipt.files.is_empty()
        || receipt.files.len() > MAX_FILES
        || !receipt.manifest.starts_with("manifests/local/")
        || Path::new(&receipt.manifest)
            .extension()
            .is_none_or(|e| e != "mpd")
        || !receipt.files.contains_key(&receipt.manifest)
        || !receipt.files.contains_key("reports/phase1-summary.json")
    {
        return Err(invalid());
    }
    let mut total = 0u64;
    let mut bindings = BTreeMap::new();
    for (relative, expected) in &receipt.files {
        let (actual, binding) = fingerprint_bound(root, relative, &budget)?;
        bindings.insert(relative.clone(), binding);
        total = total
            .checked_add(actual.size)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(invalid)?;
        if &actual != expected {
            return Err(invalid());
        }
    }
    budget.check()?;
    validate_report(root, asset_id)?;
    Ok(Verified {
        manifest: receipt.manifest,
        files: receipt.files,
        bindings,
    })
}

/// Emit the optional private receipt after the actual successful pipeline and report writes.
/// A missing request preserves ordinary CLI behavior. An invalid request fails closed.
pub fn complete(request: Option<&str>, result: &AssetResult, root: &Path) -> io::Result<()> {
    let Some(request) = request else {
        return Ok(());
    };
    let (path, nonce): (PathBuf, String) = serde_json::from_str(request).map_err(|_| invalid())?;
    if nonce.len() != 32 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    if !result.all_pass
        || !result.full_enumeration
        || !result.round_trip_verified
        || result.renditions.is_empty()
    {
        return Ok(());
    }
    let canonical_root = root.canonicalize()?;
    let written: serde_json::Value = serde_json::from_slice(&bounded(
        reader(&canonical_root, "reports/phase1-summary.json")?,
        MAX_JSON,
    )?)
    .map_err(|_| invalid())?;
    if written != crate::pipeline::asset_result_to_json(result) {
        return Err(invalid());
    }
    let (manifest, names) = output_names(result, root)?;
    issue(
        &path,
        &nonce,
        &result.asset_id,
        &canonical_root,
        manifest,
        names,
    )
}

fn output_names(result: &AssetResult, root: &Path) -> io::Result<(String, Vec<String>)> {
    let manifests = result.manifests.as_ref().ok_or_else(invalid)?;
    let dash = relative(root, &manifests.dash.output_mpd_path)?;
    let manifest = format!(
        "manifests/local/{}",
        dash.strip_prefix("manifests/").ok_or_else(invalid)?
    );
    let mut names = Vec::new();
    let mut bytes = 0usize;
    let mut add = |path: &Path| -> io::Result<()> {
        if names.len() >= MAX_FILES {
            return Err(invalid());
        }
        let name = relative(root, path)?;
        bytes = bytes
            .checked_add(name.len())
            .filter(|n| *n <= 4 * 1024 * 1024)
            .ok_or_else(invalid)?;
        names.push(name);
        Ok(())
    };
    add(&root.join("reports/phase1-summary.json"))?;
    for rendition in &result.renditions {
        add(&rendition.cbcs_init_path)?;
        for segment in &rendition.cbcs_segment_paths {
            add(segment)?;
        }
    }
    for path in std::iter::once(&manifests.dash.output_mpd_path)
        .chain(std::iter::once(&manifests.hls.output_master_path))
        .chain(manifests.hls.output_media_playlist_paths.iter())
    {
        let name = relative(root, path)?;
        add(path)?;
        add(&root.join(format!(
            "manifests/local/{}",
            name.strip_prefix("manifests/").ok_or_else(invalid)?
        )))?;
    }
    Ok((manifest, names))
}

fn issue(
    path: &Path,
    nonce: &str,
    asset_id: &str,
    root: &Path,
    manifest: String,
    names: Vec<String>,
) -> io::Result<()> {
    if names.len() > MAX_FILES {
        return Err(invalid());
    }
    validate_report(root, asset_id)?;
    if names
        .iter()
        .try_fold(0usize, |n, s| n.checked_add(s.len()))
        .is_none_or(|n| n > 4 * 1024 * 1024)
    {
        return Err(invalid());
    }
    let mut files = BTreeMap::new();
    let budget = ReadControl {
        cancelled: &|| false,
    };
    let mut total = 0u64;
    for name in names {
        let value = fingerprint_bound(root, &name, &budget)?.0;
        total = total
            .checked_add(value.size)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(invalid)?;
        files.insert(name, value);
    }
    let receipt = Receipt {
        version: 1,
        nonce: nonce.to_string(),
        asset_id: asset_id.to_string(),
        manifest,
        files,
    };
    let bytes = serde_json::to_vec(&receipt).map_err(|_| invalid())?;
    if bytes.len() as u64 > MAX_JSON {
        return Err(invalid());
    }
    let mut file = native::create_private_new(path)?;
    file.write_all(&bytes)?;
    file.sync_all()
}

fn relative(root: &Path, path: &Path) -> io::Result<String> {
    let original_root = std::path::absolute(root)?;
    let path = std::path::absolute(path)?;
    let canonical_root = root.canonicalize()?;
    let relative = path
        .strip_prefix(&original_root)
        .or_else(|_| path.strip_prefix(&canonical_root))
        .map_err(|_| invalid())?;
    let name = relative
        .iter()
        .map(|s| s.to_str().ok_or_else(invalid))
        .collect::<io::Result<Vec<_>>>()?
        .join("/");
    let root = root.canonicalize()?;
    checked(&root, &name)?;
    Ok(name)
}

fn checked(root: &Path, relative: &str) -> io::Result<PathBuf> {
    if relative.len() > 4096
        || relative.contains('\\')
        || relative.contains(':')
        || relative.chars().any(char::is_control)
    {
        return Err(invalid());
    }
    let path = Path::new(relative);
    if !path.components().all(|p| matches!(p, Component::Normal(_)))
        || !(relative.starts_with("cbcs/")
            || relative.starts_with("manifests/")
            || relative == "reports/phase1-summary.json")
    {
        return Err(invalid());
    }
    let mut full = root.to_path_buf();
    if !std::fs::symlink_metadata(root)?.is_dir()
        || std::fs::symlink_metadata(root)?.file_type().is_symlink()
    {
        return Err(invalid());
    }
    for part in path.components() {
        full.push(part);
        let metadata = std::fs::symlink_metadata(&full)?;
        if metadata.file_type().is_symlink() {
            return Err(invalid());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(invalid());
            }
        }
    }
    Ok(full)
}

struct Reader {
    file: File,
    #[cfg(windows)]
    _pin: native::PrivateDirectoryGuard,
}
impl std::ops::Deref for Reader {
    type Target = File;
    fn deref(&self) -> &File {
        &self.file
    }
}
impl Read for Reader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.file.read(bytes)
    }
}

fn reader(root: &Path, name: &str) -> io::Result<Reader> {
    let path = checked(root, name)?;
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags};
        use std::os::unix::fs::MetadataExt as _;
        let file = File::from(rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )?);
        let m = file.metadata()?;
        if !m.is_file() || m.nlink() != 1 || m.uid() != rustix::process::geteuid().as_raw() {
            return Err(invalid());
        }
        Ok(Reader { file })
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
        let pin = native::pin_media_directory(path.parent().ok_or_else(invalid)?)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .custom_flags(0x0020_0000)
            .open(path)?;
        let m = file.metadata()?;
        if !m.is_file()
            || m.file_attributes() & 0x400 != 0
            || winapi_util::file::information(&file)?.number_of_links() != 1
        {
            return Err(invalid());
        }
        Ok(Reader { file, _pin: pin })
    }
}

fn stamp(file: &File) -> io::Result<Stamp> {
    let metadata = file.metadata()?;
    Ok(Stamp {
        identity: native::FileIdentity::read(file)?,
        size: metadata.len(),
        modified: metadata.modified()?,
        #[cfg(unix)]
        changed: {
            use std::os::unix::fs::MetadataExt as _;
            (metadata.ctime(), metadata.ctime_nsec())
        },
    })
}

struct ReadControl<'a> {
    cancelled: &'a dyn Fn() -> bool,
}
impl ReadControl<'_> {
    fn check(&self) -> io::Result<()> {
        if (self.cancelled)() {
            return Err(invalid());
        }
        Ok(())
    }
}
fn fingerprint_bound(
    root: &Path,
    name: &str,
    budget: &ReadControl<'_>,
) -> io::Result<(Fingerprint, Stamp)> {
    budget.check()?;
    let mut file = reader(root, name)?;
    let before = stamp(&file)?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; 65536];
    loop {
        budget.check()?;
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size = size
            .checked_add(n as u64)
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(invalid)?;
        hash.update(&buf[..n]);
    }
    let after = stamp(&file)?;
    if before != after || after.size != size {
        return Err(invalid());
    }
    Ok((
        Fingerprint {
            size,
            sha256: hex::encode(hash.finalize()),
        },
        after,
    ))
}

fn bounded(file: impl Read, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid());
    }
    Ok(bytes)
}

/// Read the bounded regular report used by the server's existing public scrubber.
/// This does not qualify its checks or promote an output generation.
pub fn read_report(root: &Path) -> io::Result<serde_json::Value> {
    serde_json::from_slice(&bounded(
        reader(root, "reports/phase1-summary.json")?,
        MAX_JSON,
    )?)
    .map_err(|_| invalid())
}

fn validate_report(root: &Path, asset_id: &str) -> io::Result<()> {
    let value = read_report(root)?;
    if value["schema_version"] != "1"
        || value["asset_id"] != asset_id
        || ["all_pass", "full_enumeration", "round_trip_verified"]
            .iter()
            .any(|f| value[*f] != true)
    {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const NONCE: &str = "00112233445566778899aabbccddeeff";
    fn fixture() -> (native::PrivateTempDir, PathBuf, PathBuf, Vec<String>) {
        let temp = native::private_tempdir().unwrap();
        let root = temp.path().join("asset");
        let names = [
            "reports/phase1-summary.json",
            "manifests/local/source.mpd",
            "cbcs/segment.m4s",
        ]
        .map(str::to_string)
        .to_vec();
        for name in &names {
            let path = root.join(name);
            native::create_media_dir_all(path.parent().unwrap()).unwrap();
            native::create_media_new(&path).unwrap().write_all(if name.starts_with("reports/") { br#"{"schema_version":"1","asset_id":"asset","all_pass":true,"full_enumeration":true,"round_trip_verified":true}"# } else { b"qualified-output" }).unwrap();
        }
        let receipt = temp.path().join("receipt.json");
        (temp, root, receipt, names)
    }
    #[test]
    fn actual_bytes_and_operation_nonce_are_both_required() {
        let (_temp, root, path, names) = fixture();
        assert!(verify(&path, NONCE, "asset", &root).is_err());
        issue(&path, NONCE, "asset", &root, names[1].clone(), names).unwrap();
        let verified = verify(&path, NONCE, "asset", &root).unwrap();
        assert!(verified.contains("cbcs/segment.m4s"));
        assert!(!verified.contains("cbcs/stale.m4s"));
        assert!(verify(&path, "ffeeddccbbaa99887766554433221100", "asset", &root).is_err());
        native::create_media_or_truncate(&root.join("cbcs/segment.m4s"))
            .unwrap()
            .write_all(b"partial")
            .unwrap();
        assert!(verify(&path, NONCE, "asset", &root).is_err());
    }
    #[test]
    fn shutdown_cancels_an_inventory_hash_before_it_can_verify() {
        let (_temp, root, path, names) = fixture();
        native::create_media_or_truncate(&root.join("cbcs/segment.m4s"))
            .unwrap()
            .write_all(&vec![7; 131_072])
            .unwrap();
        issue(&path, NONCE, "asset", &root, names[1].clone(), names).unwrap();
        let reads = std::cell::Cell::new(0);
        assert!(
            verify_with(&path, NONCE, "asset", &root, &|| {
                reads.set(reads.get() + 1);
                reads.get() > 3
            })
            .is_err()
        );
        assert!(reads.get() >= 4);
        assert!(verify(&path, NONCE, "asset", &root).is_ok());
    }
    #[test]
    fn failed_or_incomplete_report_cannot_issue_success() {
        for flag in ["all_pass", "full_enumeration", "round_trip_verified"] {
            let (_temp, root, path, names) = fixture();
            let report = root.join("reports/phase1-summary.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
            value[flag] = false.into();
            native::create_media_or_truncate(&report)
                .unwrap()
                .write_all(&serde_json::to_vec(&value).unwrap())
                .unwrap();
            assert!(issue(&path, NONCE, "asset", &root, names[1].clone(), names).is_err());
            assert!(!path.exists());
        }
    }
    #[test]
    fn inventory_cannot_authorize_paths_outside_the_generated_media() {
        let (_temp, root, path, mut names) = fixture();
        for bad in [
            "../outside",
            "clear/source.mp4",
            "cbcs/../reports/phase1-summary.json",
        ] {
            names.push(bad.to_string());
            assert!(
                issue(
                    &path,
                    NONCE,
                    "asset",
                    &root,
                    names[1].clone(),
                    names.clone()
                )
                .is_err()
            );
            names.pop();
        }
    }
}
