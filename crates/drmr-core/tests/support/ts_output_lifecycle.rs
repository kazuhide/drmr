//! The new packaging route uses the existing mandatory immediate process/lock completion contract.
use super::{
    Arc, ConversionRequest, Converter, HlsInputKeys, INPUT_IV, INPUT_KEY, InputKeys, OutputFormat,
    Path, Secrets, TargetProtection, ToolRunner, common, sample_aes_fixture,
};
fn exclusive_lock(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.share_mode(0);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    file.try_lock().map_err(std::io::Error::other)?;
    Ok(file)
}
struct FailureCleanup(Option<Box<dyn FnOnce()>>);
impl Drop for FailureCleanup {
    fn drop(&mut self) {
        if let Some(cleanup) = self.0.take() {
            cleanup();
        }
    }
}
struct CancelOnDrop(drmr_core::concurrency::Stop);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.set();
    }
}
fn controlled_packager(
    root: &Path,
    pid: &Path,
    lock: &Path,
    stop: &drmr_core::concurrency::Stop,
) -> Converter {
    let actual_packager = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|directory| drmr_test_support::program_path(&directory.join("packager")))
        .find(|program| program.is_file())
        .and_then(|program| std::fs::canonicalize(program).ok())
        .expect("actual packager must be available");
    let tool = drmr_test_support::install(
        &root.join("probe"),
        &drmr_test_support::ToolSpec::default()
            .with("real_packager", actual_packager.to_str().unwrap())
            .with(
                "packager_route_audit",
                root.join("route-audit").to_str().unwrap(),
            )
            .with("descendant_pid_file", pid.to_str().unwrap())
            .with("descendant_lock_file", lock.to_str().unwrap())
            .with("release_file", root.join("never-release").to_str().unwrap()),
    )
    .unwrap();
    Converter::new(
        Arc::new(ToolRunner::new(
            Arc::new(Secrets::default()),
            Arc::new(drmr_core::events::NoSink),
        )),
        tool.to_str().unwrap(),
        "ffmpeg",
    )
    .with_stop(stop.clone())
    .with_timeout(std::time::Duration::from_secs(15))
}
#[test]
fn cancellation_inside_ts_output_packager_confirms_descendant_completion_before_return() {
    use std::time::{Duration, Instant};
    let root = common::private_tempdir().unwrap();
    let fixture = sample_aes_fixture::source(&root.path().join("input"), INPUT_KEY, INPUT_IV);
    let input = fixture.clear;
    let original = common::snapshot_tree(input.parent().unwrap());
    let pid = root.path().join("descendant.pid");
    let lock = root.path().join("descendant.lock");
    let output = root.path().join("result");
    let stop = drmr_core::concurrency::Stop::new();
    let c = controlled_packager(root.path(), &pid, &lock, &stop);
    std::thread::scope(|scope| {
        let _cancel = CancelOnDrop(stop.clone());
        let worker = scope.spawn(|| {
            c.convert(ConversionRequest {
                input: &input,
                output: &output,
                format: OutputFormat::HlsTs,
                target: TargetProtection::Clear,
                input_keys: &InputKeys::default(),
                hls_input_keys: &HlsInputKeys::default(),
                output_key: None,
                hls_key_uri: None,
                hls_output_key: None,
            })
        });
        let end = Instant::now() + Duration::from_secs(10);
        while !std::fs::read_to_string(&pid)
            .is_ok_and(|s| s.ends_with('\n') && s.trim().parse::<u32>().is_ok())
        {
            assert!(
                !worker.is_finished(),
                "converter exited before fixture readiness"
            );
            assert!(Instant::now() < end, "media child did not become ready");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            exclusive_lock(&lock).is_err(),
            "fixture must hold an actual exclusive lock"
        );
        let descendant = std::fs::read_to_string(&pid)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        #[cfg(unix)]
        let mut failure_cleanup = {
            let process =
                rustix::process::Pid::from_raw(i32::try_from(descendant).unwrap()).unwrap();
            let group = rustix::process::getpgid(Some(process)).unwrap();
            assert_ne!(group, rustix::process::getpgrp());
            FailureCleanup(Some(Box::new(move || {
                let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
            })))
        };
        #[cfg(windows)]
        let mut failure_cleanup = {
            let process = winsafe::HPROCESS::OpenProcess(
                winsafe::co::PROCESS::SYNCHRONIZE | winsafe::co::PROCESS::TERMINATE,
                false,
                descendant,
            )
            .unwrap();
            FailureCleanup(Some(Box::new(move || {
                let _ = process.TerminateProcess(1);
                let _ = process.WaitForSingleObject(Some(5000));
            })))
        };
        #[cfg(windows)]
        let process =
            winsafe::HPROCESS::OpenProcess(winsafe::co::PROCESS::SYNCHRONIZE, false, descendant)
                .unwrap();
        assert_eq!(
            std::fs::read(root.path().join("route-audit")).unwrap(),
            b"demux\ndemux\nts-output\n",
            "both real demux calls must complete before the TS-output child is held"
        );
        stop.set();
        let result = worker.join().unwrap();
        #[cfg(windows)]
        assert_eq!(
            process.WaitForSingleObject(Some(0)),
            Ok(winsafe::co::WAIT::OBJECT_0),
            "return preceded kernel process completion"
        );
        exclusive_lock(&lock).unwrap_or_else(|error| {
            panic!("return preceded actual descendant lock release: {error:?}; result={result:?}")
        });
        failure_cleanup.0 = None;
        assert_eq!(result.unwrap_err().0, "cancelled");
        assert!(!output.exists());
    });
    for (name, bytes) in original {
        assert_eq!(
            std::fs::read(input.parent().unwrap().join(name)).unwrap(),
            bytes
        );
    }
}
