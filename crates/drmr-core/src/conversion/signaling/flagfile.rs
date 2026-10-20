//! Keep explicit PSSH metadata outside command-line size limits and published media.
use super::{ConversionError, LIMIT};
use crate::{
    conversion::{
        backend::Backend,
        files::{Stage, UnpublishedStage},
    },
    native::{self, FileIdentity},
    toolrun::CleanupReceipt,
};
use std::{
    fs::File,
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
    sync::Arc,
};
use zeroize::Zeroizing;

const INVALID: ConversionError = ConversionError("output-signaling-private-file-invalid");

struct PrivatePssh {
    // Close the Windows read-only guard before deleting the strictly owned stage.
    reader: File,
    stage: UnpublishedStage,
    path: PathBuf,
    identity: FileIdentity,
    cleanup: Option<CleanupReceipt>,
    activity: Option<Arc<dyn crate::events::Sink>>,
}
impl PrivatePssh {
    fn new(
        hex: &str,
        parent: Option<&Path>,
        activity: Option<Arc<dyn crate::events::Sink>>,
    ) -> Result<Self, ConversionError> {
        Self::new_observed(hex, parent, activity, |_| {})
    }
    fn new_observed(
        hex: &str,
        parent: Option<&Path>,
        activity: Option<Arc<dyn crate::events::Sink>>,
        created: impl FnOnce(&Stage),
    ) -> Result<Self, ConversionError> {
        if hex.is_empty()
            || hex.len() > LIMIT - 8
            || !hex.len().is_multiple_of(2)
            || !hex.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(INVALID);
        }
        // This separate stage is never a publication source.
        let stage = UnpublishedStage::new(parent)?;
        let path = stage.path().join("protection.flags");
        argument(&path)?;
        let mut writer = stage.create_file(&path).map_err(|_| INVALID)?;
        let identity = FileIdentity::read(&writer).map_err(|_| INVALID)?;
        writer.write_all(b"--pssh=").map_err(|_| INVALID)?;
        writer.write_all(hex.as_bytes()).map_err(|_| INVALID)?;
        writer.write_all(b"\n").map_err(|_| INVALID)?;
        writer.sync_all().map_err(|_| INVALID)?;
        created(&stage);
        drop(
            stage
                .created_file_source(&path, &writer)
                .map_err(|_| INVALID)?,
        );
        // Shaka opens the file for reading. Windows must not retain the writer's access.
        drop(writer);
        let reader = native::open_private_read(&path).map_err(|_| INVALID)?;
        if FileIdentity::read(&reader).map_err(|_| INVALID)? != identity {
            return Err(INVALID);
        }
        let flags = Self {
            reader,
            stage,
            path,
            identity,
            cleanup: None,
            activity,
        };
        flags.check(hex)?;
        Ok(flags)
    }
    fn check(&self, hex: &str) -> Result<(), ConversionError> {
        let source = self.stage.file_source(&self.path).map_err(|_| INVALID)?;
        let mut reader = source.reader().map_err(|_| INVALID)?;
        let private = native::open_private_read(&self.path).map_err(|_| INVALID)?;
        if FileIdentity::read(&self.reader).map_err(|_| INVALID)? != self.identity
            || FileIdentity::read(&reader).map_err(|_| INVALID)? != self.identity
            || FileIdentity::read(&private).map_err(|_| INVALID)? != self.identity
            || reader.metadata().map_err(|_| INVALID)?.len() != (hex.len() + 8) as u64
        {
            return Err(INVALID);
        }
        let mut bytes = Zeroizing::new(vec![0; hex.len() + 8]);
        reader.read_exact(&mut bytes).map_err(|_| INVALID)?;
        let mut extra = [0; 1];
        if reader.read(&mut extra).map_err(|_| INVALID)? != 0
            || &bytes[..7] != b"--pssh="
            || &bytes[7..bytes.len() - 1] != hex.as_bytes()
            || bytes.last() != Some(&b'\n')
        {
            return Err(INVALID);
        }
        Ok(())
    }
    fn wait_for_cleanup(&self) {
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
impl Drop for PrivatePssh {
    fn drop(&mut self) {
        // A retained child may still read the flags, including after cancellation.
        self.wait_for_cleanup();
    }
}
fn argument(path: &Path) -> Result<String, ConversionError> {
    let path = super::super::files::tool_path(path)?;
    // Abseil splits --flagfile values on commas even within one OS argument.
    if path.contains([',', '\n', '\r']) || path.len() > 4096 {
        return Err(ConversionError("unsupported-path"));
    }
    Ok(format!("--flagfile={path}"))
}
pub(in crate::conversion) fn run(
    backend: &Backend,
    argv: &[String],
    hex: &str,
    secrets: &[&str],
) -> Result<Vec<u8>, ConversionError> {
    run_with(backend, argv, hex, |backend, arguments, receipt| {
        backend.run_with_cleanup_receipt(arguments, "packager", secrets, receipt)
    })
}
fn run_with(
    backend: &Backend,
    argv: &[String],
    hex: &str,
    operation: impl FnOnce(
        &Backend,
        &[String],
        &mut Option<CleanupReceipt>,
    ) -> Result<Vec<u8>, ConversionError>,
) -> Result<Vec<u8>, ConversionError> {
    backend.check_cancelled()?;
    let (runner, _) = backend.runner.ensuring_cleanup_owner();
    let mut flags = PrivatePssh::new(hex, None, backend.activity.clone())?;
    let mut protected = backend.clone();
    protected.runner = Arc::new(runner);
    let mut arguments = Zeroizing::new(argv.to_vec());
    arguments.push(argument(&flags.path)?);
    let result = operation(&protected, &arguments, &mut flags.cleanup);
    flags.wait_for_cleanup();
    flags.check(hex)?;
    result
}

#[cfg(test)]
mod tests;
