//! Invoke the ordinary CLI handlers through bounded private console channels.
use crate::{analyze_cli, convert_cli, inspect_cli};
use drmr_core::native::{self, FileIdentity};
use drmr_server::common_protocol::{
    self, ChildOperation, ChildProgress, ChildRequest, ChildSource, OperationName,
};
use serde_json::Value;
use std::{
    fs::File,
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

static CONTEXT: OnceLock<Context> = OnceLock::new();

struct Context {
    nonce: String,
    operation: OperationName,
    result_path: PathBuf,
    result_identity: FileIdentity,
    result_attempted: AtomicBool,
    result_written: AtomicBool,
    progress: Mutex<ProgressChannel>,
    #[cfg(windows)]
    _parent: native::PrivateDirectoryGuard,
}
struct ProgressChannel {
    path: PathBuf,
    sequence: u64,
}

fn invalid() -> io::Error {
    io::Error::other("common-job-channel-unavailable")
}

fn read_request(path: &Path) -> io::Result<ChildRequest> {
    let file = native::open_private_read(path)?;
    let mut bytes = Vec::new();
    file.take(common_protocol::REQUEST_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    let request: ChildRequest =
        common_protocol::decode(&bytes, common_protocol::REQUEST_LIMIT).map_err(|_| invalid())?;
    request.validate(path).map_err(|_| invalid())?;
    Ok(request)
}

impl Context {
    fn open(request: &ChildRequest) -> io::Result<Self> {
        let result = native::open_private_read(&request.channels.result_file)?;
        let progress = native::open_private_read(&request.channels.progress_file)?;
        // A request is single use. Existing reports are never overwritten on replay.
        if result.metadata()?.len() != 0 || progress.metadata()?.len() != 0 {
            return Err(invalid());
        }
        Ok(Self {
            nonce: request.nonce.clone(),
            operation: request.operation.name(),
            result_path: request.channels.result_file.clone(),
            result_identity: FileIdentity::read(&result)?,
            result_attempted: AtomicBool::new(false),
            result_written: AtomicBool::new(false),
            progress: Mutex::new(ProgressChannel {
                path: request.channels.progress_file.clone(),
                sequence: 0,
            }),
            #[cfg(windows)]
            _parent: native::pin_private_directory(
                request.channels.result_file.parent().ok_or_else(invalid)?,
            )?,
        })
    }

    fn write_result(&self, value: &Value) -> io::Result<()> {
        if !value.is_object() || self.result_attempted.swap(true, Ordering::AcqRel) {
            return Err(invalid());
        }
        let file = native::create_private_update(&self.result_path)?;
        if FileIdentity::read(&file)? != self.result_identity || file.metadata()?.len() != 0 {
            return Err(invalid());
        }
        let mut writer = LimitedWriter {
            file,
            remaining: common_protocol::RESULT_LIMIT,
        };
        write!(
            writer,
            "{{\"protocol_version\":1,\"nonce\":\"{}\",\"operation\":\"{}\",\"payload\":",
            self.nonce,
            self.operation.as_str()
        )?;
        serde_json::to_writer(&mut writer, value)?;
        writer.write_all(b"}\n")?;
        writer.flush()?;
        self.result_written.store(true, Ordering::Release);
        Ok(())
    }

    fn publish(&self, observation: Observation) -> io::Result<()> {
        let mut channel = self.progress.lock().map_err(|_| invalid())?;
        let update = ChildProgress {
            protocol_version: 1,
            nonce: self.nonce.clone(),
            operation: self.operation,
            sequence: channel.sequence.saturating_add(1),
            elapsed_ms: observation.elapsed_ms,
            last_activity: observation.last_activity.map(str::to_owned),
            scopes_started: observation.scopes_started,
            scopes_closed: observation.scopes_closed,
            workers: observation.workers,
        };
        if !update.valid(&self.nonce, self.operation) {
            return Err(invalid());
        }
        let temporary = channel.path.with_file_name("progress.pending.json");
        let mut created = false;
        let result = (|| {
            // Readers use shared-delete access on Windows. Publishing one complete
            // object avoids exposing partially written progress as a newer snapshot.
            drop(native::open_private_shared_read(&channel.path)?);
            let file = native::create_private_new(&temporary)?;
            created = true;
            let mut writer = LimitedWriter {
                file,
                remaining: common_protocol::PROGRESS_LIMIT,
            };
            serde_json::to_writer(&mut writer, &update)?;
            writer.flush()?;
            drop(writer);
            std::fs::rename(&temporary, &channel.path)?;
            Ok(())
        })();
        if result.is_err() && created {
            let _ = std::fs::remove_file(&temporary);
        } else if result.is_ok() {
            channel.sequence = update.sequence;
        }
        result
    }
}

struct LimitedWriter {
    file: File,
    remaining: usize,
}
impl io::Write for LimitedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(invalid());
        }
        let written = self.file.write(bytes)?;
        self.remaining -= written;
        Ok(written)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Observation {
    pub elapsed_ms: u64,
    pub last_activity: Option<&'static str>,
    pub scopes_started: u64,
    pub scopes_closed: u64,
    pub workers: Option<u64>,
}
pub(crate) fn progress(observation: Observation) {
    if let Some(context) = CONTEXT.get() {
        // Progress is observational. A missing snapshot is never media evidence.
        let _ = context.publish(observation);
    }
}

pub(crate) fn active() -> bool {
    CONTEXT.get().is_some()
}

/// None preserves the ordinary CLI's existing stdout behavior.
pub(crate) fn result(value: &Value) -> Option<io::Result<()>> {
    CONTEXT.get().map(|context| context.write_result(value))
}

pub(crate) fn run(path: &Path) -> ExitCode {
    let Ok(request) = read_request(path).and_then(|request| {
        let context = Context::open(&request)?;
        CONTEXT.set(context).map_err(|_| invalid())?;
        Ok(request)
    }) else {
        eprintln!("drmr: invalid common job request");
        return ExitCode::from(2);
    };
    let keys = request.runtime.keys_file;
    let workers = request.runtime.max_workers;
    let tools = request.runtime.tools;
    let exit = match request.operation {
        ChildOperation::Analyze { source } => {
            let (input, download_dir, fetch_policy) = match source {
                ChildSource::Local { input_file } => (input_file, None, None),
                ChildSource::Url {
                    url,
                    download_dir,
                    fetch_policy_file,
                } => (PathBuf::from(url), Some(download_dir), fetch_policy_file),
            };
            analyze_cli::analyze(&analyze_cli::AnalyzeArgs {
                input,
                metadata_only: false,
                download_dir,
                fetch_policy,
                keys_file: keys,
                packager_bin: tools.packager_bin,
                max_workers: workers,
                json: true,
                measure: false,
            })
        }
        ChildOperation::Plan {
            input_file,
            output_root,
            save_file,
            output_spec_file,
        } => inspect_cli::plan(&inspect_cli::PlanArgs {
            input: input_file,
            to: None,
            format: Vec::new(),
            json: true,
            save: Some(save_file),
            outputs: Some(output_spec_file),
            output: Some(output_root),
            keys_file: keys,
            packager_bin: Some(tools.packager_bin),
            max_workers: workers,
            measure: false,
        }),
        ChildOperation::Convert { plan_file } => convert_cli::convert(&convert_cli::ConvertArgs {
            input: None,
            plan: Some(plan_file),
            download_dir: None,
            fetch_policy: None,
            to: None,
            format: None,
            outputs: None,
            output: None,
            keys_file: keys,
            packager_bin: tools.packager_bin,
            max_workers: workers,
            hls_validator_bin: tools.hls_validator_bin,
            dash_validator_bin: tools.dash_validator_bin,
            json: true,
            measure: false,
        }),
        ChildOperation::Verify { plan_file } => convert_cli::verify(&convert_cli::VerifyArgs {
            output: None,
            plan: Some(plan_file),
            manifest: convert_cli::ManifestArgs {
                manifest_only: false,
                external_validator: false,
                manifest_profile: None,
                hls_validator_bin: Some(tools.hls_validator_bin),
                dash_validator_bin: Some(tools.dash_validator_bin),
            },
            source: None,
            keys_file: keys,
            packager_bin: tools.packager_bin,
            max_workers: workers,
            json: true,
            measure: false,
        }),
    };
    if CONTEXT
        .get()
        .is_none_or(|context| !context.result_written.load(Ordering::Acquire))
    {
        eprintln!("drmr: common job result unavailable");
        return ExitCode::from(1);
    }
    exit
}

#[cfg(test)]
mod tests {
    use super::*;
    fn context(root: &Path) -> Context {
        let result_path = root.join("result.json");
        let result = native::create_private_new(&result_path).unwrap();
        let progress_path = root.join("progress.json");
        drop(native::create_private_new(&progress_path).unwrap());
        Context {
            nonce: "a".repeat(32),
            operation: OperationName::Verify,
            result_path,
            result_identity: FileIdentity::read(&result).unwrap(),
            result_attempted: AtomicBool::new(false),
            result_written: AtomicBool::new(false),
            progress: Mutex::new(ProgressChannel {
                path: progress_path,
                sequence: 0,
            }),
            #[cfg(windows)]
            _parent: native::pin_private_directory(root).unwrap(),
        }
    }
    #[test]
    fn result_is_single_use_and_bound_to_the_original_file() {
        let root = native::private_tempdir().unwrap();
        let context = context(root.path());
        std::fs::rename(&context.result_path, root.path().join("original.json")).unwrap();
        drop(native::create_private_new(&context.result_path).unwrap());
        assert!(
            context
                .write_result(&serde_json::json!({"passed":true}))
                .is_err()
        );
        assert!(!context.result_written.load(Ordering::Acquire));
        assert_eq!(std::fs::metadata(&context.result_path).unwrap().len(), 0);
    }
    #[test]
    fn negative_result_and_atomic_progress_retain_separate_evidence() {
        let root = native::private_tempdir().unwrap();
        let context = context(root.path());
        let shared_reader =
            native::open_private_shared_read(&root.path().join("progress.json")).unwrap();
        for scopes in 1..=3 {
            context
                .publish(Observation {
                    elapsed_ms: scopes,
                    last_activity: Some("verification"),
                    scopes_started: scopes,
                    scopes_closed: 0,
                    workers: Some(2),
                })
                .unwrap();
        }
        drop(shared_reader);
        let bytes = std::fs::read(root.path().join("progress.json")).unwrap();
        let progress: ChildProgress =
            common_protocol::decode(&bytes, common_protocol::PROGRESS_LIMIT).unwrap();
        assert_eq!(progress.sequence, 3);
        assert!(!root.path().join("progress.pending.json").exists());
        context
            .write_result(&serde_json::json!({"passed":false}))
            .unwrap();
        let report: common_protocol::ChildResult = common_protocol::decode(
            &std::fs::read(&context.result_path).unwrap(),
            common_protocol::RESULT_LIMIT,
        )
        .unwrap();
        assert_eq!(report.payload["passed"], false);
        assert!(
            context
                .write_result(&serde_json::json!({"passed":true}))
                .is_err()
        );
    }
    #[test]
    fn preexisting_progress_temporary_is_never_removed_or_overwritten() {
        let root = native::private_tempdir().unwrap();
        let context = context(root.path());
        let temporary = root.path().join("progress.pending.json");
        let mut file = native::create_private_new(&temporary).unwrap();
        file.write_all(b"existing-owned-data").unwrap();
        drop(file);
        assert!(
            context
                .publish(Observation {
                    elapsed_ms: 1,
                    last_activity: Some("verification"),
                    scopes_started: 1,
                    scopes_closed: 0,
                    workers: None
                })
                .is_err()
        );
        assert_eq!(std::fs::read(&temporary).unwrap(), b"existing-owned-data");
        assert_eq!(
            std::fs::metadata(root.path().join("progress.json"))
                .unwrap()
                .len(),
            0
        );
    }
    #[test]
    fn bounded_writer_refuses_bytes_beyond_its_allowance() {
        let root = native::private_tempdir().unwrap();
        let file = native::create_private_new(&root.path().join("bounded")).unwrap();
        let mut writer = LimitedWriter { file, remaining: 4 };
        writer.write_all(b"123").unwrap();
        assert!(writer.write_all(b"45").is_err());
        writer.flush().unwrap();
        assert_eq!(std::fs::read(root.path().join("bounded")).unwrap(), b"123");
    }
}
