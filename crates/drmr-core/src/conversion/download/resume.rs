//! Reuse complete original resources without weakening current fetch policy or source identity.

use super::{ConversionError, failure, resume_cache, source, valid_url};
use crate::native::FileIdentity;
use resume_cache::{CompletedResource, ResourceKind, ResumeCache};
use sha2::{Digest as _, Sha256};
use std::path::Path;

pub(super) fn open(
    enabled: bool,
    directory: &Path,
    url: &str,
    output: Option<&Path>,
    policy: &source::FetchPolicy,
    owned: &mut super::OwnedDownload,
    stop: &crate::concurrency::Stop,
) -> Result<Option<ResumeCache>, ConversionError> {
    if !enabled {
        return Ok(None);
    }
    match ResumeCache::open(directory, url, output, policy, stop) {
        Ok(cache) => Ok(Some(cache)),
        Err(error) => {
            owned.cleanup()?;
            Err(error)
        }
    }
}

pub(super) struct RootFetch {
    pub(super) fetched: source::Fetched,
    pub(super) created: FileIdentity,
    pub(super) etag: Option<source::StrongEtag>,
    pub(super) checkpointed: bool,
}

pub(super) fn entrypoint(
    cache: Option<&ResumeCache>,
    client: &source::Fetcher,
    url: &str,
    destination: &Path,
) -> Result<RootFetch, ConversionError> {
    if let Some(cache) = cache {
        if cache
            .lookup(url, None)?
            .is_some_and(|prior| prior.kind == ResourceKind::Mp4)
        {
            let (fetched, created) =
                fetch(cache, client, url, destination, ResourceKind::Mp4, |_| {
                    Ok(())
                })?;
            return Ok(RootFetch {
                fetched,
                created,
                etag: None,
                checkpointed: true,
            });
        }
        match client
            .fetch_entrypoint_revalidated(url, destination, None)
            .map_err(|e| failure(&e))?
        {
            source::RevalidatedFetch::Fresh {
                fetched,
                created,
                etag,
            } => Ok(RootFetch {
                fetched,
                created,
                etag,
                checkpointed: false,
            }),
            source::RevalidatedFetch::NotModified { .. } => Err(super::CHANGED),
        }
    } else {
        let (fetched, created) =
            client
                .fetch_entrypoint_created(url, destination)
                .map_err(|e| {
                    if client.stop().is_set() {
                        ConversionError("cancelled")
                    } else {
                        failure(&e)
                    }
                })?;
        Ok(RootFetch {
            fetched,
            created,
            etag: None,
            checkpointed: false,
        })
    }
}

pub(super) fn fetch(
    cache: &ResumeCache,
    client: &source::Fetcher,
    url: &str,
    destination: &Path,
    kind: ResourceKind,
    register: impl FnOnce(FileIdentity) -> Result<(), ConversionError>,
) -> Result<(source::Fetched, FileIdentity), ConversionError> {
    let candidate = cache.lookup(url, None)?;
    if candidate.as_ref().is_some_and(|prior| prior.kind != kind) {
        return Err(super::CHANGED);
    }
    let prior = candidate.as_ref().and_then(|prior| {
        Some(source::Revalidation {
            effective_url_sha256: prior.effective_url_sha256.clone(),
            strong_etag: source::StrongEtag::parse(prior.etag.as_deref()?)?,
            cached_size: prior.size,
            byte_range: None,
        })
    });
    let received = if kind == ResourceKind::Mp4 {
        client.fetch_entrypoint_revalidated(url, destination, prior.as_ref())
    } else {
        client.fetch_revalidated(url, destination, None, prior.as_ref())
    }
    .map_err(|error| failure(&error))?;
    match received {
        source::RevalidatedFetch::Fresh {
            fetched,
            created,
            etag,
        } => {
            // Register ownership before checking bytes or writing a checkpoint, so
            // any refusal can remove only the writer this operation actually created.
            register(created)?;
            valid_url(&fetched.effective)?;
            let _disk = client.reserve_copy(fetched.size).map_err(|e| failure(&e))?;
            cache.store_file(
                CompletedResource {
                    requested: url,
                    range: None,
                    kind,
                    fetched: &fetched,
                    etag: etag.as_ref().map(source::StrongEtag::as_str),
                    path: destination,
                    created,
                },
                client.stop(),
            )?;
            Ok((fetched, created))
        }
        source::RevalidatedFetch::NotModified { effective } => {
            valid_url(&effective)?;
            let candidate = candidate.ok_or(super::CHANGED)?;
            if hex::encode(Sha256::digest(effective.as_bytes())) != candidate.effective_url_sha256 {
                return Err(super::CHANGED);
            }
            let _disk = client
                .reserve_copy(candidate.size)
                .map_err(|e| failure(&e))?;
            let created = cache.materialize(&candidate, destination, client.stop())?;
            register(created)?;
            Ok((
                source::Fetched {
                    effective,
                    sha256: candidate.sha256,
                    size: candidate.size,
                },
                created,
            ))
        }
    }
}

pub(super) fn manifest(
    cache: Option<&ResumeCache>,
    client: &source::Fetcher,
    requested: &str,
    effective: &str,
    body: &[u8],
) -> Result<(), ConversionError> {
    if let Some(cache) = cache {
        let _disk = client
            .reserve_copy(body.len() as u64)
            .map_err(|e| failure(&e))?;
        cache.store_manifest(requested, effective, body, client.stop())?;
    }
    Ok(())
}
