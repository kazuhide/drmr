//! Content detection and retained entrypoints share the sealed acquisition transaction.
use super::{
    CHANGED, ConversionError, Expected, Graph, INVALID, IO, bytes_digest, dash, failure, files,
    source, valid_url, write_private,
};
use std::{
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
};

pub(super) struct Acquired {
    pub(super) entrypoint: &'static str,
    pub(super) contents: Entrypoint,
    pub(super) files: Expected,
}
pub(super) enum Entrypoint {
    Projection(Vec<u8>),
    Streamed(Box<PendingEntrypoint>),
}
pub(super) struct PendingEntrypoint {
    _stage: files::Stage,
    pub(super) path: PathBuf,
    pub(super) writer: Option<std::fs::File>,
    pub(super) created: crate::native::FileIdentity,
    pub(super) digest: ([u8; 32], u64),
}
impl Entrypoint {
    pub(super) fn prepare(
        self,
        parent: Option<&Path>,
    ) -> Result<PendingEntrypoint, ConversionError> {
        match self {
            Self::Streamed(pending) => Ok(*pending),
            Self::Projection(bytes) => {
                let stage = files::Stage::new(parent)?;
                let path = stage.private_root().join("entrypoint");
                let writer = write_private(&path, &bytes)?;
                let created = crate::native::FileIdentity::read(&writer).map_err(|_| IO)?;
                Ok(PendingEntrypoint {
                    _stage: stage,
                    path,
                    writer: Some(writer),
                    created,
                    digest: bytes_digest(&bytes),
                })
            }
        }
    }
}
impl Graph<'_> {
    pub(super) fn entrypoint(
        &mut self,
        url: &str,
    ) -> Result<(&'static str, Entrypoint), ConversionError> {
        self.admit_node()?;
        let stage = files::Stage::new(self.owned.path().parent())?;
        let path = stage.private_root().join("entrypoint");
        let (received, created) =
            self.fetcher
                .fetch_entrypoint_created(url, &path)
                .map_err(|error| {
                    if self.fetcher.stop().is_set() {
                        ConversionError("cancelled")
                    } else {
                        failure(&error)
                    }
                })?;
        valid_url(&received.effective)?;
        let mut input = files::regular_input(&path).map_err(|_| CHANGED)?;
        if crate::native::FileIdentity::read(&input).map_err(|_| CHANGED)? != created {
            return Err(CHANGED);
        }
        let mut prefix = [0; 64];
        let count = input.read(&mut prefix).map_err(|_| IO)?;
        let prefix = &prefix[..count];
        let text = prefix
            .strip_prefix(b"\xef\xbb\xbf")
            .unwrap_or(prefix)
            .trim_ascii_start();
        if !text.starts_with(b"#EXTM3U") && !text.starts_with(b"<") {
            if !prefix.get(4..8).is_some_and(|kind| {
                matches!(
                    kind,
                    b"ftyp" | b"moov" | b"styp" | b"free" | b"wide" | b"mdat" | b"sidx"
                )
            }) {
                return Err(INVALID);
            }
            self.text_charge(url.len().saturating_add(received.effective.len()))?;
            self.records.push(serde_json::json!({
                "kind":"mp4", "requested":url, "effective":received.effective,
                "sha256":received.sha256, "bytes":received.size,
            }));
            drop(input);
            return Ok((
                "input.mp4",
                Entrypoint::Streamed(Box::new(PendingEntrypoint {
                    _stage: stage,
                    path,
                    writer: None,
                    created,
                    digest: (
                        hex::decode(received.sha256)
                            .map_err(|_| IO)?
                            .try_into()
                            .map_err(|_| IO)?,
                        received.size,
                    ),
                })),
            ));
        }
        if received.size > self.policy.max_playlist_bytes {
            return Err(INVALID);
        }
        input.seek(SeekFrom::Start(0)).map_err(|_| IO)?;
        let mut body = Vec::new();
        input
            .take(self.policy.max_playlist_bytes + 1)
            .read_to_end(&mut body)
            .map_err(|_| IO)?;
        let (hash, size) = bytes_digest(&body);
        if size != received.size || hex::encode(hash) != received.sha256 {
            return Err(CHANGED);
        }
        if text.starts_with(b"#EXTM3U") {
            self.playlist_loaded(url, true, Some((body, received.effective)))?;
            Ok((
                "input.m3u8",
                Entrypoint::Projection(std::mem::take(&mut self.root_projection)),
            ))
        } else {
            self.dash_entrypoint(url, &received, &body)
        }
    }
    fn dash_entrypoint(
        &mut self,
        url: &str,
        received: &source::Fetched,
        body: &[u8],
    ) -> Result<(&'static str, Entrypoint), ConversionError> {
        self.playlist_count = 1;
        self.text_charge(body.len() + url.len() + received.effective.len())?;
        self.charge(body.len().saturating_mul(8) + 1024)?;
        let max_resources = self
            .policy
            .max_total_resources
            .unwrap_or(100_000)
            .saturating_sub(1)
            .min(99_999);
        let plan = dash::plan(
            body,
            &received.effective,
            self.fetcher.stop(),
            usize::try_from(max_resources).map_err(|_| INVALID)?,
            usize::try_from(self.policy.max_segments_per_track.min(100_000))
                .map_err(|_| INVALID)?,
        )?;
        self.save("raw/r000000.mpd", body)?;
        self.records.push(serde_json::json!({
            "kind":"manifest", "requested":url, "effective":received.effective,
            "sha256":received.sha256, "bytes":received.size,
        }));
        self.text_charge(plan.projection.len())?;
        self.charge(plan.projection.len().saturating_mul(8))?;
        for resource in plan.resources {
            self.admit_node()?;
            self.text_charge(resource.url.len())?;
            self.charge(resource.url.len().saturating_mul(8) + 1024)?;
            self.resources.push(resource);
        }
        Ok(("input.mpd", Entrypoint::Projection(plan.projection)))
    }
}
