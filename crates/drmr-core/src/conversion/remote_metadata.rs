//! Policy-bound metadata requests never follow media, key, or license references.
use super::{ConversionError, Converter, download::stream::CancellablePlatform};
use crate::{
    concurrency::Stop,
    inspection::{self, InputFormat, Inspection, InspectionError},
    resources::Claim,
    source::{self, ByteRange, FetchPolicy, Fetcher, MetadataRead},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
const INVALID: ConversionError = ConversionError("remote-metadata-not-supported");
const IO: ConversionError = ConversionError("remote-metadata-unavailable");
const MAX_METADATA: u64 = 16 * 1024 * 1024;
fn failure(error: source::Failure) -> ConversionError {
    match error {
        source::Failure::Cancelled => ConversionError("cancelled"),
        source::Failure::Source(error)
            if matches!(
                error.token(),
                "metadata-range-required" | "invalid-byte-range"
            ) =>
        {
            INVALID
        }
        _ => IO,
    }
}
impl Converter {
    /// Inspect bounded URL metadata without retaining the source or reading media samples.
    /// HLS follows playlist references only; DASH reads declarations only. MP4 requires
    /// exact byte-range responses and reads top-level headers plus the movie box.
    pub fn inspect_url_metadata(
        &self,
        url: &str,
        policy: &FetchPolicy,
    ) -> Result<Inspection, ConversionError> {
        let _scope = self
            .backend
            .scope(super::measurement::Phase::InputPreparation);
        self.backend.check_cancelled()?;
        let progress = self
            .backend
            .activity
            .as_ref()
            .map(|sink| source::progress::Progress::new(sink.clone()));
        if url.len() > 4096
            || url.chars().any(char::is_control)
            || !matches!(
                source::pyurl::urlsplit(url, "")
                    .map_err(|_| INVALID)?
                    .scheme
                    .as_str(),
                "http" | "https"
            )
        {
            return Err(INVALID);
        }
        let origin = source::origin(url).map_err(|_| INVALID)?;
        let mut policy = policy.clone();
        if !policy.allowed_local_roots.is_empty() {
            return Err(INVALID);
        }
        if policy.allowed_origins.is_empty() {
            policy.allowed_origins.push(origin);
        }
        policy
            .validate()
            .map_err(|_| ConversionError("invalid-fetch-policy"))?;
        policy.max_playlist_bytes = policy
            .max_playlist_bytes
            .min(inspection::MAX_MANIFEST_BYTES);
        self.backend.runner.secrets().register([url]);
        let _permit = self
            .backend
            .runner
            .native_permit(
                Claim {
                    cpu_threads: 1,
                    memory_bytes: 64 * 1024 * 1024,
                },
                &self.backend.stop,
                self.backend.timeout,
            )
            .map_err(|error| match error {
                crate::resources::ReservationError::Cancelled => ConversionError("cancelled"),
                _ => ConversionError("resource-budget"),
            })?;
        let directory = crate::native::private_tempdir().map_err(|_| IO)?;
        let fetcher = Fetcher::new(
            &policy,
            directory.path(),
            Arc::new(CancellablePlatform {
                stop: self.backend.stop.clone(),
                abort: Stop::new(),
            }),
        )
        .with_stop(self.backend.stop.clone())
        .with_progress(progress.clone())
        .with_url_limit(4096);
        let mut remote = Remote {
            fetcher,
            policy: &policy,
            requests: 0,
            references: 0,
            playlists: 0,
            text_bytes: 0,
            active: BTreeSet::new(),
            cache: BTreeMap::new(),
        };
        let report = remote.inspect(url)?;
        self.backend.check_cancelled()?;
        Ok(report)
    }
}
struct Remote<'a> {
    fetcher: Fetcher,
    policy: &'a FetchPolicy,
    requests: u64,
    references: usize,
    playlists: u64,
    text_bytes: u64,
    active: BTreeSet<String>,
    cache: BTreeMap<String, Inspection>,
}
impl Drop for Remote<'_> {
    fn drop(&mut self) {
        self.fetcher.close_all();
    }
}
impl Remote<'_> {
    fn inspect(&mut self, url: &str) -> Result<Inspection, ConversionError> {
        self.request()?;
        let mut probe = self
            .fetcher
            .metadata_probe(url)
            .map_err(|error| self.failure(error))?;
        if inspection::format_hint(&probe.bytes).is_none()
            && let Some(total) = probe.total
            && total > 8
        {
            self.request()?;
            let prefix = self
                .fetcher
                .metadata_prefix(&probe.effective, total.min(64))
                .map_err(|error| self.failure(error))?;
            if prefix.total != Some(total) || prefix.effective != probe.effective {
                return Err(INVALID);
            }
            probe = prefix;
        }
        let format = inspection::format_hint(&probe.bytes).ok_or(INVALID)?;
        if format == InputFormat::Mp4 {
            self.movie(probe)
        } else {
            let (bytes, effective) = if probe.total.is_some() {
                self.manifest(&probe.effective)?
            } else {
                (probe.bytes, probe.effective)
            };
            if inspection::format_hint(&bytes) != Some(format) {
                return Err(INVALID);
            }
            self.charge(bytes.len())?;
            if format == InputFormat::Dash {
                inspection::inspect_dash_metadata(&bytes).map_err(|e| ConversionError(e.0))
            } else {
                self.hls(&bytes, &effective)
                    .map_err(|e| ConversionError(e.0))
            }
        }
    }
    fn failure(&self, error: source::Failure) -> ConversionError {
        // The cancellable stream interrupts a blocked final attempt as an I/O error.
        // Preserve Stop as the public result after that connection has been closed.
        if self.fetcher.stop().is_set() {
            ConversionError("cancelled")
        } else {
            failure(error)
        }
    }
    fn request(&mut self) -> Result<(), ConversionError> {
        if self.fetcher.stop().is_set() {
            return Err(ConversionError("cancelled"));
        }
        self.requests = self
            .requests
            .checked_add(1)
            .filter(|n| {
                *n <= self
                    .policy
                    .max_total_resources
                    .unwrap_or(100_000)
                    .min(100_000)
            })
            .ok_or(INVALID)?;
        Ok(())
    }
    fn charge(&mut self, count: usize) -> Result<(), ConversionError> {
        self.text_bytes = self
            .text_bytes
            .checked_add(count as u64)
            .filter(|n| *n <= MAX_METADATA)
            .ok_or(INVALID)?;
        Ok(())
    }
    fn manifest(&mut self, url: &str) -> Result<(Vec<u8>, String), ConversionError> {
        self.request()?;
        self.fetcher
            .read(url, true, None)
            .map_err(|error| self.failure(error))
    }
    fn hls(&mut self, bytes: &[u8], effective: &str) -> Result<Inspection, InspectionError> {
        if self.fetcher.stop().is_set() {
            return Err(InspectionError("cancelled"));
        }
        self.playlists += 1;
        if self.playlists > self.policy.max_playlists.min(32)
            || !self.active.insert(effective.to_owned())
        {
            return Err(InspectionError("hls-reference-limit"));
        }
        let result = inspection::inspect_hls_metadata(bytes, &mut |reference| {
            self.references += 1;
            if self.references > 128 || reference.len() > 4096 {
                return Err(InspectionError("hls-reference-limit"));
            }
            let url = source::child_url(effective, reference)
                .map_err(|e| InspectionError(self.failure(e).0))?;
            if let Some(cached) = self.cache.get(&url) {
                return Ok(cached.clone());
            }
            if self.active.contains(&url) {
                return Err(InspectionError("hls-reference-cycle"));
            }
            if self.playlists >= self.policy.max_playlists.min(32) {
                return Err(InspectionError("hls-playlist-limit"));
            }
            let (bytes, effective) = self.manifest(&url).map_err(|e| InspectionError(e.0))?;
            self.charge(bytes.len()).map_err(|e| InspectionError(e.0))?;
            let child = self.hls(&bytes, &effective)?;
            self.cache.insert(url, child.clone());
            Ok(child)
        });
        self.active.remove(effective);
        result
    }
    fn range(
        &mut self,
        url: &str,
        total: u64,
        offset: u64,
        length: u64,
    ) -> Result<Vec<u8>, ConversionError> {
        self.request()?;
        let response = self
            .fetcher
            .metadata_range(url, ByteRange { offset, length })
            .map_err(|error| self.failure(error))?;
        if response.total != Some(total) || response.effective != url {
            return Err(INVALID);
        }
        Ok(response.bytes)
    }
    fn movie(&mut self, probe: MetadataRead) -> Result<Inspection, ConversionError> {
        let total = probe.total.ok_or(INVALID)?;
        let url = probe.effective;
        let mut position = 0_u64;
        let mut movie = None;
        let mut boxes = 0;
        while position < total {
            boxes += 1;
            if boxes > 100_000 || total - position < 8 {
                return Err(INVALID);
            }
            let fixed = if position == 0 {
                probe.bytes.clone()
            } else {
                self.range(&url, total, position, 8)?
            };
            let size32 = u32::from_be_bytes(fixed[..4].try_into().map_err(|_| INVALID)?);
            let header = if size32 == 1 { 16 } else { 8 };
            let size = match size32 {
                0 => total - position,
                1 => {
                    if total - position < 16 {
                        return Err(INVALID);
                    }
                    let large = self.range(&url, total, position + 8, 8)?;
                    u64::from_be_bytes(large.try_into().map_err(|_| INVALID)?)
                }
                n => u64::from(n),
            };
            if size < header || size > total - position {
                return Err(INVALID);
            }
            if &fixed[4..8] == b"moov" {
                if movie.is_some() || size > MAX_METADATA {
                    return Err(INVALID);
                }
                let mut bytes = self.range(&url, total, position, size)?;
                if bytes[..8] != fixed[..8] {
                    return Err(INVALID);
                }
                if size32 == 1
                    && u64::from_be_bytes(bytes[8..16].try_into().map_err(|_| INVALID)?) != size
                {
                    return Err(INVALID);
                }
                if size32 == 0 {
                    bytes[..4]
                        .copy_from_slice(&u32::try_from(size).map_err(|_| INVALID)?.to_be_bytes());
                }
                movie = Some(bytes);
            }
            position = position.checked_add(size).ok_or(INVALID)?;
        }
        inspection::inspect_mp4_metadata(&movie.ok_or(INVALID)?).map_err(|e| ConversionError(e.0))
    }
}
