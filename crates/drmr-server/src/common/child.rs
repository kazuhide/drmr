//! Per-attempt private channels and copies, outside all generic HTTP delivery.
use super::{
    files::{BoundPath, read_private},
    plans,
    wire::{Error, Result, unavailable},
};
use crate::common_protocol::{
    self as protocol, ChildChannels, ChildOperation, ChildRequest, ChildRuntime, OperatorTools,
};
use drmr_core::native::{self, FileIdentity};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use zeroize::Zeroizing;

pub struct Directory {
    pub path: PathBuf,
    binding: Option<BoundPath>,
}
impl Directory {
    pub fn check(&self) -> Result<()> {
        self.binding.as_ref().ok_or_else(unavailable)?.check()
    }
}
pub struct Prepared {
    pub directory: Directory,
    pub request: PathBuf,
    pub result: Option<BoundPath>,
    pub progress: PathBuf,
    pub nonce: String,
    pub operation: ChildOperation,
    pub copies: Vec<(PathBuf, FileIdentity)>,
    channels: Vec<(PathBuf, FileIdentity)>,
    #[cfg(windows)]
    directory_pin: Option<native::PrivateDirectoryGuard>,
    owner: Option<native::PrivateTempDir>,
    cleanup_confirmed: AtomicBool,
}
#[derive(Clone, Copy)]
pub struct Preparation<'a> {
    pub keys: Option<&'a BoundPath>,
    pub fetch: Option<&'a BoundPath>,
    pub plan: Option<&'a plans::Plan>,
    pub outputs: Option<&'a [drmr_core::conversion::plan::bundle::BundleOutputIntent]>,
    pub maximum: u8,
    pub decode: bool,
}
impl Prepared {
    // Ownership exists before any file is created, including a failed partial configuration.
    pub fn allocate(root: &Path, nonce: String, operation: ChildOperation) -> Result<Self> {
        let owner = native::private_tempdir_in(root, "job-").map_err(|_| unavailable())?;
        let path = owner.path().to_path_buf();
        Ok(Self {
            request: path.join("request.json"),
            progress: path.join("progress.json"),
            directory: Directory {
                path,
                binding: None,
            },
            result: None,
            nonce,
            operation,
            copies: Vec::new(),
            channels: Vec::new(),
            owner: Some(owner),
            cleanup_confirmed: AtomicBool::new(false),
            #[cfg(windows)]
            directory_pin: None,
        })
    }
    pub fn configure(&mut self, settings: Preparation<'_>) -> Result<()> {
        self.directory.binding = Some(BoundPath::capture(&self.directory.path)?);
        #[cfg(windows)]
        {
            self.directory_pin = Some(
                native::pin_private_directory(&self.directory.path).map_err(|_| unavailable())?,
            );
        }
        let Preparation {
            keys,
            fetch,
            plan,
            outputs,
            maximum,
            decode,
        } = settings;
        let directory = &self.directory.path;
        let keys_file = copy(keys, &directory.join("keys.json"), 65_536, &mut self.copies)?;
        let fetch_policy = copy(
            fetch,
            &directory.join("fetch-policy.json"),
            65_536,
            &mut self.copies,
        )?;
        let operation = prepare_operation(
            self.operation.clone(),
            directory,
            plan,
            outputs,
            fetch_policy,
            &mut self.copies,
        )?;
        let result = directory.join("result.json");
        write_owned(&result, &[], &mut self.channels)?;
        self.result = Some(BoundPath::capture(&result)?);
        write_owned(&self.progress, &[], &mut self.channels)?;
        let body = ChildRequest {
            protocol_version: 1,
            nonce: self.nonce.clone(),
            operation: operation.clone(),
            runtime: ChildRuntime {
                keys_file,
                max_workers: Some(maximum),
                decode,
                tools: OperatorTools::default(),
            },
            channels: ChildChannels {
                result_file: result,
                progress_file: self.progress.clone(),
            },
        };
        write_request(&self.request, &body, &mut self.copies)?;
        self.operation = operation;
        self.directory.check()
    }
    pub fn discard(&self) -> Result<()> {
        let result = self
            .cleanup()
            .and_then(|()| remove_owned(&self.channels))
            .and_then(|()| self.directory.check());
        self.cleanup_confirmed
            .store(result.is_ok(), Ordering::SeqCst);
        result
    }
    pub fn cleanup(&self) -> Result<()> {
        let result = self
            .directory
            .check()
            .and_then(|()| remove_owned(&self.copies))
            .and_then(|()| self.directory.check());
        self.cleanup_confirmed
            .store(result.is_ok(), Ordering::SeqCst);
        result
    }
    pub fn read_result(&self) -> Result<protocol::ChildResult> {
        self.directory.check()?;
        let bytes = read_private(
            self.result
                .as_ref()
                .ok_or(Error::new(503, "result-invalid"))?,
            protocol::RESULT_LIMIT,
        )?;
        let result: protocol::ChildResult = protocol::decode(&bytes, protocol::RESULT_LIMIT)
            .map_err(|_| Error::new(503, "result-invalid"))?;
        if result.protocol_version != 1
            || result.nonce != self.nonce
            || result.operation != self.operation.name()
        {
            return Err(Error::new(503, "result-invalid"));
        }
        Ok(result)
    }
    pub fn progress(&self) -> Option<protocol::ChildProgress> {
        self.directory.check().ok()?;
        let file = native::open_private_shared_read(&self.progress).ok()?;
        let mut bytes = Vec::new();
        file.take(protocol::PROGRESS_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        let progress: protocol::ChildProgress =
            protocol::decode(&bytes, protocol::PROGRESS_LIMIT).ok()?;
        (progress.valid(&self.nonce, self.operation.name())
            && progress.scopes_closed <= progress.scopes_started)
            .then_some(progress)
    }
}
impl Drop for Prepared {
    fn drop(&mut self) {
        // An unbound or replaced workspace is retained, never traversed by the temp owner.
        if (!self.cleanup_confirmed.load(Ordering::SeqCst) || self.directory.check().is_err())
            && let Some(owner) = self.owner.take()
        {
            std::mem::forget(owner);
        }
    }
}
fn remove_owned(files: &[(PathBuf, FileIdentity)]) -> Result<()> {
    for (path, id) in files {
        match native::open_private_read(path) {
            Ok(file) => {
                if FileIdentity::read(&file).map_err(|_| unavailable())? != *id {
                    return Err(Error::new(503, "cleanup-unconfirmed"));
                }
                drop(file);
                std::fs::remove_file(path).map_err(|_| Error::new(503, "cleanup-unconfirmed"))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(Error::new(503, "cleanup-unconfirmed")),
        }
    }
    Ok(())
}
fn prepare_operation(
    operation: ChildOperation,
    directory: &Path,
    plan: Option<&plans::Plan>,
    outputs: Option<&[drmr_core::conversion::plan::bundle::BundleOutputIntent]>,
    fetch_policy: Option<PathBuf>,
    copies: &mut Vec<(PathBuf, FileIdentity)>,
) -> Result<ChildOperation> {
    Ok(match operation {
        ChildOperation::Plan {
            input_file,
            output_root,
            save_file,
            ..
        } => {
            let output_spec_file = directory.join("output-spec.json");
            let bytes = serde_json::to_vec(&serde_json::json!({"schema_version":1,
                "outputs":outputs.ok_or_else(unavailable)?}))
            .map_err(|_| unavailable())?;
            if bytes.len() > 65_536 {
                return Err(Error::new(413, "request-too-large"));
            }
            write_owned(&output_spec_file, &bytes, copies)?;
            ChildOperation::Plan {
                input_file,
                output_root,
                save_file,
                output_spec_file,
            }
        }
        ChildOperation::Analyze {
            source: protocol::ChildSource::Url {
                url, download_dir, ..
            },
        } => ChildOperation::Analyze {
            source: protocol::ChildSource::Url {
                url,
                download_dir,
                fetch_policy_file: fetch_policy,
            },
        },
        ChildOperation::Convert { .. } | ChildOperation::Verify { .. } => {
            let plan_file = directory.join("plan.json");
            write_owned(&plan_file, &plan.ok_or_else(unavailable)?.read()?, copies)?;
            if matches!(operation, ChildOperation::Convert { .. }) {
                ChildOperation::Convert { plan_file }
            } else {
                ChildOperation::Verify { plan_file }
            }
        }
        other @ ChildOperation::Analyze { .. } => other,
    })
}
fn write_request(
    request: &Path,
    body: &ChildRequest,
    copies: &mut Vec<(PathBuf, FileIdentity)>,
) -> Result<()> {
    body.validate(request).map_err(|_| unavailable())?;
    let bytes = serde_json::to_vec(body).map_err(|_| unavailable())?;
    if bytes.len() > protocol::REQUEST_LIMIT {
        return Err(Error::new(413, "request-too-large"));
    }
    write_owned(request, &bytes, copies)?;
    Ok(())
}
fn write_owned(path: &Path, bytes: &[u8], files: &mut Vec<(PathBuf, FileIdentity)>) -> Result<()> {
    use std::io::Write as _;
    let mut file = native::create_private_new(path).map_err(|_| unavailable())?;
    files.push((
        path.to_path_buf(),
        FileIdentity::read(&file).map_err(|_| unavailable())?,
    ));
    file.write_all(bytes)
        .and_then(|()| file.flush())
        .map_err(|_| unavailable())
}
fn copy(
    source: Option<&BoundPath>,
    destination: &Path,
    limit: usize,
    copies: &mut Vec<(PathBuf, FileIdentity)>,
) -> Result<Option<PathBuf>> {
    let Some(source) = source else {
        return Ok(None);
    };
    let bytes = Zeroizing::new(read_private(source, limit)?);
    write_owned(destination, &bytes, copies)?;
    Ok(Some(destination.to_path_buf()))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{io::Write as _, os::unix::fs::PermissionsExt};
    #[test]
    fn partial_configuration_remains_owned_through_cleanup_failure_and_recovery() {
        let root = native::private_tempdir().unwrap();
        let root_path = root.path().canonicalize().unwrap();
        let keys = root_path.join("keys.json");
        let mut file = native::create_private_new(&keys).unwrap();
        file.write_all(b"SYNTHETIC_PRIVATE_KEYS_MARKER").unwrap();
        drop(file);
        let fetch = root_path.join("fetch.json");
        std::fs::write(&fetch, b"{}").unwrap();
        std::fs::set_permissions(&fetch, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut prepared = Prepared::allocate(
            &root_path,
            "a".repeat(32),
            ChildOperation::Analyze {
                source: protocol::ChildSource::Local {
                    input_file: root_path.join("unused.mp4"),
                },
            },
        )
        .unwrap();
        let result = prepared.configure(Preparation {
            keys: Some(&BoundPath::capture(&keys).unwrap()),
            fetch: Some(&BoundPath::capture(&fetch).unwrap()),
            plan: None,
            outputs: None,
            maximum: 2,
            decode: false,
        });
        assert_eq!(result.unwrap_err().code, "private-config-unavailable");
        let copy = prepared.directory.path.join("keys.json");
        assert_eq!(
            std::fs::read(&copy).unwrap(),
            b"SYNTHETIC_PRIVATE_KEYS_MARKER"
        );
        std::fs::set_permissions(
            &prepared.directory.path,
            std::fs::Permissions::from_mode(0o500),
        )
        .unwrap();
        assert!(prepared.discard().is_err());
        assert!(copy.is_file());
        std::fs::set_permissions(
            &prepared.directory.path,
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        prepared.discard().unwrap();
        assert!(!copy.exists());
        assert_eq!(
            std::fs::read(&keys).unwrap(),
            b"SYNTHETIC_PRIVATE_KEYS_MARKER"
        );
        assert_eq!(
            std::fs::metadata(&fetch).unwrap().permissions().mode() & 0o777,
            0o644
        );
    }
}
