//! Keep platform-specific Packager track-ID compatibility outside native admission.
use super::{Backend, ConversionError};
use crate::{
    conversion::{audio, files::UnpublishedStage, native_media},
    native::{self, FileIdentity},
    toolrun::CleanupReceipt,
};
use std::{
    fs::File,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

const INVALID: ConversionError = ConversionError("native-source-changed");

pub(super) struct ToolInput {
    // Close the Windows reader before deleting its exclusively owned stage.
    reader: File,
    stage: UnpublishedStage,
    path: PathBuf,
    identity: FileIdentity,
    length: u64,
    modified: SystemTime,
    digest: [u8; 32],
    pub cleanup: Option<CleanupReceipt>,
    activity: Option<Arc<dyn crate::events::Sink>>,
}

impl ToolInput {
    pub fn prepare(input: &Path, backend: &Backend) -> Result<Option<Self>, ConversionError> {
        backend.check_cancelled()?;
        let ids = {
            let metadata = audio::movie(input)?;
            if !metadata.fragmented {
                return Ok(None);
            }
            native_media::metadata_track_ids(&metadata.bytes)?
        };
        if ids
            .iter()
            .enumerate()
            .all(|(i, id)| u32::try_from(i + 1).ok() == Some(*id))
        {
            return Ok(None);
        }
        let _permit = backend
            .runner
            .native_permit(
                crate::resources::Claim {
                    cpu_threads: 1,
                    memory_bytes: 128 * 1024 * 1024,
                },
                &backend.stop,
                backend.timeout,
            )
            .map_err(|error| match error {
                crate::resources::ReservationError::Insufficient => {
                    ConversionError("resource-budget")
                }
                crate::resources::ReservationError::Cancelled => ConversionError("cancelled"),
                crate::resources::ReservationError::Timeout => {
                    ConversionError("media-tool-timeout")
                }
            })?;
        let stage = UnpublishedStage::new(None)?;
        let path = stage.path().join("source.mp4");
        let mut writer = stage.create_file(&path)?;
        let identity = FileIdentity::read(&writer).map_err(|_| INVALID)?;
        copy(input, &mut writer, backend)?;
        drop(stage.created_file_source(&path, &writer)?);
        // Reopening after closing the writer is required by Windows sharing rules.
        drop(writer);
        let mut expected = native_media::inspect(&path, &backend.stop)?;
        if expected
            .tracks
            .iter()
            .map(|t| t.track_id)
            .collect::<Vec<_>>()
            != ids
        {
            return Err(INVALID);
        }
        let mut writer = native::open_media_update(&path).map_err(|_| INVALID)?;
        drop(stage.created_file_source(&path, &writer)?);
        if FileIdentity::read(&writer).map_err(|_| INVALID)? != identity {
            return Err(INVALID);
        }
        native_media::canonicalize_tool_ids(&mut writer, &expected, &backend.stop)?;
        drop(writer);
        for (i, track) in expected.tracks.iter_mut().enumerate() {
            track.track_id = u32::try_from(i + 1).map_err(|_| INVALID)?;
        }
        if native_media::inspect(&path, &backend.stop)? != expected {
            return Err(INVALID);
        }
        let source = stage.file_source(&path)?;
        let reader = source.reader()?;
        let private = native::open_private_read(&path).map_err(|_| INVALID)?;
        let metadata = reader.metadata().map_err(|_| INVALID)?;
        if FileIdentity::read(&reader).map_err(|_| INVALID)? != identity
            || FileIdentity::read(&private).map_err(|_| INVALID)? != identity
        {
            return Err(INVALID);
        }
        let digest = fingerprint(&reader, metadata.len(), &backend.stop)?;
        drop(private);
        drop(source);
        let result = Self {
            reader,
            stage,
            path,
            identity,
            length: metadata.len(),
            modified: metadata.modified().map_err(|_| INVALID)?,
            digest,
            cleanup: None,
            activity: backend.activity.clone(),
        };
        Ok(Some(result))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn check(&self, stop: &crate::concurrency::Stop) -> Result<(), ConversionError> {
        let source = self.stage.file_source(&self.path)?;
        let reader = source.reader()?;
        let private = native::open_private_read(&self.path).map_err(|_| INVALID)?;
        for file in [&self.reader, &reader, &private] {
            let metadata = file.metadata().map_err(|_| INVALID)?;
            if FileIdentity::read(file).map_err(|_| INVALID)? != self.identity
                || metadata.len() != self.length
                || metadata.modified().map_err(|_| INVALID)? != self.modified
            {
                return Err(INVALID);
            }
        }
        if fingerprint(&reader, self.length, stop)? != self.digest {
            return Err(INVALID);
        }
        Ok(())
    }

    pub fn wait_for_cleanup(&self) {
        let Some(cleanup) = &self.cleanup else { return };
        let mut notified = false;
        while cleanup.has_pending() {
            let _ = cleanup.retry_finish();
            if !cleanup.has_pending() {
                break;
            }
            if !notified {
                if !std::thread::panicking()
                    && let Some(activity) = &self.activity
                {
                    activity.emit(
                        serde_json::json!({"stage":"packaging", "event":"cleanup-pending"})
                            .as_object()
                            .unwrap(),
                    );
                }
                notified = true;
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    }
}
impl Drop for ToolInput {
    fn drop(&mut self) {
        self.wait_for_cleanup();
    }
}

fn fingerprint(
    mut reader: &File,
    length: u64,
    stop: &crate::concurrency::Stop,
) -> Result<[u8; 32], ConversionError> {
    reader.seek(SeekFrom::Start(0)).map_err(|_| INVALID)?;
    let mut bytes = zeroize::Zeroizing::new(vec![0; native_media::CHUNK_BYTES]);
    let mut digest = crate::sha256::Sha256::new();
    let mut total = 0_u64;
    loop {
        if stop.is_set() {
            return Err(ConversionError("cancelled"));
        }
        let count = reader.read(&mut bytes).map_err(|_| INVALID)?;
        if count == 0 {
            if total != length {
                return Err(INVALID);
            }
            return Ok(digest.finalize());
        }
        total = total
            .checked_add(count as u64)
            .filter(|n| *n <= length && *n <= crate::conversion::MAX_MEDIA_BYTES)
            .ok_or(INVALID)?;
        digest.update(&bytes[..count]);
    }
}

fn copy(input: &Path, writer: &mut File, backend: &Backend) -> Result<(), ConversionError> {
    let mut reader = crate::conversion::files::regular_input(input)?;
    crate::conversion::validate_held_input(&reader)?;
    let identity = FileIdentity::read(&reader).map_err(|_| INVALID)?;
    let before = reader.metadata().map_err(|_| INVALID)?;
    let mut bytes = zeroize::Zeroizing::new(vec![0; native_media::CHUNK_BYTES]);
    let mut total = 0_u64;
    loop {
        backend.check_cancelled()?;
        let count = reader
            .read(&mut bytes)
            .map_err(|_| ConversionError("media-copy-failed"))?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .filter(|n| *n <= before.len() && *n <= crate::conversion::MAX_MEDIA_BYTES)
            .ok_or(INVALID)?;
        writer
            .write_all(&bytes[..count])
            .map_err(|_| ConversionError("media-copy-failed"))?;
    }
    writer
        .sync_all()
        .map_err(|_| ConversionError("media-copy-failed"))?;
    let current = crate::conversion::files::regular_input(input)?;
    let after = current.metadata().map_err(|_| INVALID)?;
    if total != before.len()
        || after.len() != before.len()
        || before.modified().map_err(|_| INVALID)? != after.modified().map_err(|_| INVALID)?
        || FileIdentity::read(&reader).map_err(|_| INVALID)? != identity
        || FileIdentity::read(&current).map_err(|_| INVALID)? != identity
    {
        return Err(INVALID);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
