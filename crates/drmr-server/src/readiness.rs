//! Durable catalog-attempt fences, separate from exit status and output existence.
use drmr_core::{catalog_receipt, native};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) struct Attempt {
    pub job_id: String,
    pub asset_id: String,
    pub root: PathBuf,
    pub receipt: PathBuf,
    cancelled: Arc<AtomicBool>,
}

/// A completed generation owns every read authorized by its receipt.
/// The lock covers file access only, never HTTP backpressure or a whole stream.
pub struct OutputGeneration {
    job_id: String,
    verified: catalog_receipt::Verified,
    live: Mutex<bool>,
}

impl std::ops::Deref for OutputGeneration {
    type Target = catalog_receipt::Verified;
    fn deref(&self) -> &Self::Target {
        &self.verified
    }
}

impl OutputGeneration {
    fn new(job_id: String, verified: catalog_receipt::Verified) -> Self {
        Self {
            job_id,
            verified,
            live: Mutex::new(true),
        }
    }

    pub(crate) fn read<T>(&self, action: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let live = self.live.lock().map_err(|_| unavailable())?;
        if !*live {
            return Err(unavailable());
        }
        action()
    }

    pub(crate) fn description(&self) -> io::Result<Value> {
        self.read(|| Ok(self.public_value()))
    }

    fn public_value(&self) -> Value {
        json!({"state":"verified", "reason":"qualified-generation", "job_id":self.job_id})
    }

    pub(crate) fn manifest_and_description(&self) -> io::Result<(String, Value)> {
        self.read(|| Ok((self.manifest.clone(), self.public_value())))
    }

    pub(crate) fn report(&self, root: &Path) -> io::Result<(Value, Value)> {
        self.read(|| Ok((catalog_receipt::read_report(root)?, self.public_value())))
    }
}

#[derive(Default)]
pub(crate) struct CatalogReadiness {
    inner: Mutex<Inner>,
    closing: AtomicBool,
}
#[derive(Default)]
struct Inner {
    pending: HashMap<String, Attempt>,
    cached: HashMap<String, Arc<OutputGeneration>>,
    loading: HashSet<String>,
    active: HashMap<String, Arc<AtomicBool>>,
    rejected: HashSet<(String, String)>,
}

fn unavailable() -> io::Error {
    io::Error::other("catalog-readiness-unavailable")
}
fn record_path(jobs: &Path, asset: &str) -> PathBuf {
    jobs.join(format!(
        "catalog-{}.json",
        hex::encode(Sha256::digest(asset.as_bytes()))
    ))
}
fn read(jobs: &Path, asset: &str) -> io::Result<Value> {
    let mut bytes = Vec::new();
    native::open_private_read(&record_path(jobs, asset))?
        .take(8193)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 8192 {
        return Err(unavailable());
    }
    let record: Value = serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    let job = record["job_id"].as_str().ok_or_else(unavailable)?;
    if record["version"] != 1
        || record["asset_id"] != asset
        || job.len() != 32
        || !job.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err(unavailable());
    }
    Ok(record)
}
fn write(jobs: &Path, asset: &str, record: &Value) -> io::Result<()> {
    let mut file = native::create_private_or_truncate(&record_path(jobs, asset))?;
    file.write_all(&serde_json::to_vec(record).map_err(|_| unavailable())?)?;
    file.sync_all()
}

impl CatalogReadiness {
    pub fn cancel(&self) {
        self.closing.store(true, Ordering::Release);
    }
    fn closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    pub fn fenced(jobs: &Path, asset: &str) -> bool {
        std::fs::symlink_metadata(record_path(jobs, asset)).is_ok()
    }

    pub fn begin(&self, jobs: &Path, job: &str, asset: &str, root: &Path) -> io::Result<String> {
        let mut inner = self.inner.lock().map_err(|_| unavailable())?;
        if self.closing() {
            return Err(unavailable());
        }
        // Exclude a bounded active read before fencing the next producer. The
        // old authority cannot open or read after this lock is released.
        let old = inner.cached.get(asset).cloned();
        let mut live = old
            .as_ref()
            .map(|v| v.live.lock().map_err(|_| unavailable()))
            .transpose()?;
        write(
            jobs,
            asset,
            &json!({"version":1,"asset_id":asset,"job_id":job,"state":"pending"}),
        )?;
        if let Some(live) = live.as_mut() {
            **live = false;
        }
        inner.cached.remove(asset);
        inner.pending.retain(|_, attempt| attempt.asset_id != asset);
        inner.rejected.retain(|(id, _)| id != asset);
        let cancelled = Arc::new(AtomicBool::new(false));
        if let Some(old) = inner.active.insert(asset.to_string(), cancelled.clone()) {
            old.store(true, Ordering::Release);
        }
        let receipt = jobs.join(format!("{job}.receipt.json"));
        let request = serde_json::to_string(&(receipt.clone(), job)).map_err(|_| unavailable())?;
        inner.pending.insert(
            job.to_string(),
            Attempt {
                job_id: job.to_string(),
                asset_id: asset.to_string(),
                root: root.to_path_buf(),
                receipt,
                cancelled,
            },
        );
        Ok(request)
    }

    /// Single-flight qualification. Called outside manager/service locks after native completion.
    pub fn finish(&self, jobs: &Path, job: &str, code: i32) {
        let attempt = self
            .inner
            .lock()
            .ok()
            .and_then(|mut i| i.pending.remove(job));
        let Some(attempt) = attempt else {
            return;
        };
        let verified = (code == 0)
            .then(|| {
                catalog_receipt::verify_with(
                    &attempt.receipt,
                    job,
                    &attempt.asset_id,
                    &attempt.root,
                    &|| self.closing() || attempt.cancelled.load(Ordering::Acquire),
                )
                .ok()
            })
            .flatten();
        self.promote(jobs, &attempt, verified);
    }

    fn promote(&self, jobs: &Path, attempt: &Attempt, verified: Option<catalog_receipt::Verified>) {
        let job = &attempt.job_id;
        let verified =
            verified.filter(|_| !self.closing() && !attempt.cancelled.load(Ordering::Acquire));
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        // Another attempt may have started while the bounded inventory was hashed.
        if !read(jobs, &attempt.asset_id).is_ok_and(|r| r["job_id"] == attempt.job_id) {
            return;
        }
        let state = if verified.is_some() {
            "verified"
        } else {
            "unverified"
        };
        if write(
            jobs,
            &attempt.asset_id,
            &json!({"version":1,"asset_id":attempt.asset_id,"job_id":job,"state":state}),
        )
        .is_err()
        {
            return;
        }
        if let Some(verified) = verified {
            inner.cached.insert(
                attempt.asset_id.clone(),
                Arc::new(OutputGeneration::new(job.clone(), verified)),
            );
        }
    }

    /// Read current authority; generation hashing happens once, never under a global lock.
    pub fn output(&self, jobs: &Path, asset: &str, root: &Path) -> Option<Arc<OutputGeneration>> {
        if self.closing() {
            return None;
        }
        let record = read(jobs, asset).ok()?;
        if record["state"] != "verified" {
            return None;
        }
        let job = record["job_id"].as_str()?.to_string();
        let cached = {
            let mut inner = self.inner.lock().ok()?;
            if inner.rejected.contains(&(asset.to_string(), job.clone())) {
                return None;
            }
            let cached = inner.cached.get(asset).filter(|v| v.job_id == job).cloned();
            if cached.is_none() && !inner.loading.insert(asset.to_string()) {
                return None;
            }
            cached
        };
        let verified = if let Some(v) = cached {
            v
        } else {
            let result = catalog_receipt::verify_with(
                &jobs.join(format!("{job}.receipt.json")),
                &job,
                asset,
                root,
                &|| self.closing(),
            );
            let mut inner = self.inner.lock().ok()?;
            inner.loading.remove(asset);
            if !read(jobs, asset).is_ok_and(|r| r["job_id"] == job && r["state"] == "verified") {
                return None;
            }
            let Ok(v) = result else {
                Self::reject(&mut inner, jobs, asset, &job);
                return None;
            };
            let v = Arc::new(OutputGeneration::new(job.clone(), v));
            inner.cached.insert(asset.to_string(), v.clone());
            v
        };
        if !verified
            .read(|| Ok(verified.current_with(root, &|| self.closing())))
            .unwrap_or(false)
        {
            if self.closing() {
                return None;
            }
            if let Ok(mut inner) = self.inner.lock()
                && read(jobs, asset).is_ok_and(|r| r["job_id"] == job)
            {
                Self::reject(&mut inner, jobs, asset, &job);
            }
            return None;
        }
        if self.closing()
            || !read(jobs, asset).is_ok_and(|r| r["job_id"] == job && r["state"] == "verified")
        {
            return None;
        }
        Some(verified)
    }

    fn reject(inner: &mut Inner, jobs: &Path, asset: &str, job: &str) {
        if let Some(generation) = inner.cached.remove(asset)
            && let Ok(mut live) = generation.live.lock()
        {
            *live = false;
        }
        inner.rejected.insert((asset.to_string(), job.to_string()));
        let _ = write(
            jobs,
            asset,
            &json!({"version":1,"asset_id":asset,"job_id":job,"state":"unverified"}),
        );
    }

    pub fn public(&self, jobs: &Path, asset: &str, root: &Path) -> Value {
        if let Some(generation) = self.output(jobs, asset, root)
            && let Ok(value) = generation.description()
        {
            return value;
        }
        Self::unverified(jobs, asset)
    }

    pub(crate) fn unverified(jobs: &Path, asset: &str) -> Value {
        let record = read(jobs, asset).ok();
        json!({"state":"unverified",
            "reason":if record.is_none() && !Self::fenced(jobs,asset) {"legacy-unfenced"} else {"generation-unconfirmed"},
            "job_id":record.and_then(|r| r["job_id"].as_str().map(str::to_string))})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIRST: &str = "00112233445566778899aabbccddeeff";
    const NEXT: &str = "ffeeddccbbaa99887766554433221100";
    #[test]
    fn receipt_absence_exit_zero_and_restart_never_promote_old_success() {
        let temp = native::private_tempdir().unwrap();
        let store = CatalogReadiness::default();
        store
            .begin(temp.path(), FIRST, "asset", &temp.path().join("asset"))
            .unwrap();
        let restarted = CatalogReadiness::default();
        assert_eq!(
            restarted.public(temp.path(), "asset", &temp.path().join("asset"))["state"],
            "unverified"
        );
        store.finish(temp.path(), FIRST, 0);
        assert_eq!(read(temp.path(), "asset").unwrap()["state"], "unverified");
        assert!(
            store
                .output(temp.path(), "asset", &temp.path().join("asset"))
                .is_none()
        );
    }
    #[test]
    fn an_older_completion_cannot_overwrite_a_new_attempt_fence() {
        let temp = native::private_tempdir().unwrap();
        let store = CatalogReadiness::default();
        store
            .begin(temp.path(), FIRST, "asset", temp.path())
            .unwrap();
        // The old request already claimed its work before the new attempt starts.
        let old = store.inner.lock().unwrap().pending.remove(FIRST).unwrap();
        store
            .begin(temp.path(), NEXT, "asset", temp.path())
            .unwrap();
        store.promote(temp.path(), &old, None);
        let record = read(temp.path(), "asset").unwrap();
        assert_eq!(record["job_id"], NEXT);
        assert_eq!(record["state"], "pending");
    }
    #[test]
    fn closing_cancels_readiness_without_starting_another_attempt() {
        let temp = native::private_tempdir().unwrap();
        let store = CatalogReadiness::default();
        store.cancel();
        assert!(
            store
                .begin(temp.path(), FIRST, "asset", temp.path())
                .is_err()
        );
        assert_eq!(store.inner.lock().unwrap().pending.len(), 0);
        assert!(!CatalogReadiness::fenced(temp.path(), "asset"));
    }
    #[test]
    fn attempt_record_write_failure_prevents_launch_authority() {
        let temp = native::private_tempdir().unwrap();
        std::fs::create_dir(record_path(temp.path(), "asset")).unwrap();
        let store = CatalogReadiness::default();
        assert!(
            store
                .begin(temp.path(), FIRST, "asset", temp.path())
                .is_err()
        );
        assert_eq!(store.inner.lock().unwrap().pending.len(), 0);
        assert_eq!(
            store.public(temp.path(), "asset", temp.path())["state"],
            "unverified"
        );
    }
}
