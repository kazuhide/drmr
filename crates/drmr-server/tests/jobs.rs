mod support;

use drmr_core::config::KeyMaterial;
use drmr_server::catalog::CatalogEntry;
use drmr_server::jobs::{ConversionJobManager, EnvSource, JobError, process_env};
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::Duration;
use support::{B_IV, B_KEY, B_KID, FakeCli, IV, KEY, KID};

fn entry(asset_id: &str, kid: &str, key: &str, iv: &str) -> CatalogEntry {
    CatalogEntry {
        asset_id: asset_id.into(),
        label: asset_id.into(),
        mpd_path: PathBuf::from("/stage/download/host/title/dash/manifest.mpd"),
        hls_master_path: PathBuf::from("/stage/download/host/title/hls/manifest.m3u8"),
        key_material: KeyMaterial::new(kid, key, iv).unwrap(),
        entry_dir: PathBuf::from("/stage"),
    }
}

struct Setup {
    _dir: tempfile::TempDir,
    root: PathBuf,
    cli: FakeCli,
    manager: Arc<ConversionJobManager>,
}

fn setup_with(env: EnvSource) -> Setup {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let cli = FakeCli::new(&root);
    let manager = Arc::new(
        ConversionJobManager::new(&root.join("jobs"), vec![OsString::from(&cli.script)], env)
            .unwrap(),
    );
    Setup {
        _dir: dir,
        root,
        cli,
        manager,
    }
}

fn setup() -> Setup {
    setup_with(process_env())
}

impl Drop for Setup {
    fn drop(&mut self) {
        if std::thread::panicking() {
            for entry in std::fs::read_dir(self.root.join("jobs")).unwrap().flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|ext| ext != "jsonl") {
                    continue;
                }
                if let Some(id) = path.file_stem().and_then(|name| name.to_str()) {
                    eprintln!(
                        "native fixture status: {:?}; log: {:?}",
                        self.manager.status(id),
                        std::fs::read_to_string(path)
                    );
                }
            }
        }
        self.manager.kill_current();
    }
}

fn flag(argv: &[String], name: &str) -> String {
    let index = argv
        .iter()
        .position(|arg| arg == name)
        .unwrap_or_else(|| panic!("{name} in {argv:?}"));
    argv[index + 1].clone()
}

fn wait_idle(manager: &ConversionJobManager) {
    support::wait_until(|| !manager.is_busy());
}

fn tail(manager: &ConversionJobManager, job_id: &str) -> Vec<String> {
    manager
        .tail_log(job_id, Duration::from_millis(5))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn new_hls_command_and_catalog_share_the_single_job_lease() {
    let s = setup();
    s.cli.hold();
    let arguments: Vec<OsString> = ["analyze-hls", "--source-config", "/private/source.json"]
        .map(OsString::from)
        .to_vec();
    let handle = s
        .manager
        .submit_command(&arguments, &[("PATH".into(), "/bin:/usr/bin".into())])
        .unwrap();
    assert_eq!(handle.state, "running");
    let refused = s
        .manager
        .submit(&entry("one", KID, KEY, IV), &s.root.join("out"));
    assert!(matches!(refused, Err(JobError::AlreadyRunning)));
    s.cli.wait_calls(1);
    assert_eq!(s.cli.calls(), 1);
    assert_eq!(
        s.cli.argv(0),
        ["analyze-hls", "--source-config", "/private/source.json"]
    );
}

#[test]
fn submit_builds_the_full_guarantee_argv() {
    let s = setup();
    let out = s.root.join("out");
    let e = entry("asset-1", KID, KEY, IV);
    s.manager.submit(&e, &out).unwrap();
    let argv = s.cli.argv(0);
    assert_eq!(argv[0], "run");
    assert_eq!(flag(&argv, "--mpd"), e.mpd_path.to_str().unwrap());
    assert_eq!(
        flag(&argv, "--hls-master"),
        e.hls_master_path.to_str().unwrap()
    );
    assert_eq!(flag(&argv, "--output"), out.to_str().unwrap());
    assert_eq!(flag(&argv, "--asset-id"), "asset-1");
    assert_eq!(flag(&argv, "--log-format"), "jsonl");
    assert_eq!(
        flag(&argv, "--manifest-local-url-base"),
        "/output/asset-1/cbcs/"
    );
}

#[test]
fn manifest_local_url_base_ends_with_a_trailing_slash() {
    // The value becomes the DASH <BaseURL>; RFC 3986 §5.2.3 drops a last segment without the
    // slash ("cbcs") and every segment 404s. The property, not one literal.
    let s = setup();
    s.manager
        .submit(&entry("asset-1", KID, KEY, IV), &s.root.join("out"))
        .unwrap();
    let base = flag(&s.cli.argv(0), "--manifest-local-url-base");
    assert!(base.ends_with('/'), "{base}");
}

#[test]
fn submit_never_passes_the_guarantee_weakening_flags() {
    let s = setup();
    let handle = s
        .manager
        .submit(&entry("asset-1", KID, KEY, IV), &s.root.join("out"))
        .unwrap();
    wait_capture(&s, &handle.job_id);
    let argv = s.cli.argv(0);
    for forbidden in ["--no-round-trip", "--limit-renditions", "--no-manifests"] {
        assert!(!argv.iter().any(|arg| arg == forbidden), "{forbidden}");
    }
}

#[test]
fn submit_never_puts_key_or_iv_on_the_command_line() {
    let s = setup();
    let handle = s
        .manager
        .submit(&entry("asset-1", KID, KEY, IV), &s.root.join("out"))
        .unwrap();
    wait_capture(&s, &handle.job_id);
    let argv = s.cli.argv(0);
    for value in [KEY, IV, KID] {
        assert!(
            !argv.iter().any(|arg| arg.contains(value)),
            "{value} on argv"
        );
    }
}

fn wait_capture(s: &Setup, job_id: &str) {
    support::wait_until(|| {
        let status = s.manager.status(job_id).unwrap();
        assert_ne!(
            status.state,
            "failed",
            "native fixture exited before readiness: {status:?}; log={:?}",
            std::fs::read_to_string(&status.log_path)
        );
        s.cli.dir.join("calls/0/inherited").is_file()
    });
}

#[test]
fn native_cli_readiness_covers_complete_captures_under_parallel_launches() {
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                for _ in 0..4 {
                    let s = setup();
                    let handle = s
                        .manager
                        .submit(&entry("capture", KID, KEY, IV), &s.root.join("out"))
                        .unwrap();
                    wait_capture(&s, &handle.job_id);
                    assert_eq!(s.cli.argv(0)[0], "run");
                    assert_eq!(s.cli.drm(0), [KID, KEY, IV]);
                    assert_eq!(s.cli.inherited(0).len(), 2);
                    wait_idle(&s.manager);
                    assert_eq!(s.manager.status(&handle.job_id).unwrap().exit_code, Some(0));
                }
            });
        }
    });
}

#[test]
fn completed_and_cancelled_jobs_clean_up_native_descendants() {
    for cancel in [false, true] {
        let s = setup();
        std::fs::write(s.cli.dir.join("tree"), b"").unwrap();
        if cancel {
            s.cli.hold();
        }
        let handle = s
            .manager
            .submit(&entry("tree", KID, KEY, IV), &s.root.join("out"))
            .unwrap();
        support::wait_until(|| {
            std::fs::read_to_string(s.cli.dir.join("descendant.pid"))
                .is_ok_and(|text| text.ends_with('\n') && text.trim().parse::<u32>().is_ok())
        });
        assert!(descendant_running(&s.cli.dir));
        if cancel {
            s.manager.kill_current();
        } else {
            wait_idle(&s.manager);
        }
        support::wait_until(|| !descendant_running(&s.cli.dir));
        assert_ne!(s.manager.status(&handle.job_id).unwrap().state, "running");
    }
}

#[cfg(windows)]
fn descendant_running(directory: &Path) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    let result = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(directory.join("descendant.lock"));
    match result {
        Ok(_) => false,
        Err(error) if error.raw_os_error() == Some(32) => true,
        Err(error) => panic!("descendant lock inspection failed: {error}"),
    }
}

#[cfg(unix)]
fn descendant_running(directory: &Path) -> bool {
    let pid = std::fs::read_to_string(directory.join("descendant.pid")).unwrap();
    let output = std::process::Command::new("ps")
        .args(["-p", pid.trim(), "-o", "stat="])
        .output()
        .unwrap();
    let state = String::from_utf8(output.stdout).unwrap();
    let state = state.trim();
    !state.is_empty() && !state.starts_with('Z')
}

#[cfg(windows)]
#[test]
fn windows_environment_names_cannot_bypass_key_isolation() {
    let s = setup_with(Arc::new(|| {
        vec![
            ("drm_key".into(), "ambient".into()),
            ("Drm_Kid".into(), "ambient".into()),
            ("dRm_Iv".into(), "ambient".into()),
            ("AMBIENT_MARKER".into(), "retained".into()),
        ]
    }));
    assert_eq!(
        s.manager.fresh_env(),
        [("AMBIENT_MARKER".into(), "retained".into())]
    );
}

#[test]
fn submit_injects_this_entrys_key_material_into_the_subprocess_env() {
    let s = setup();
    s.manager
        .submit(&entry("asset-1", KID, KEY, IV), &s.root.join("out"))
        .unwrap();
    assert_eq!(s.cli.drm(0), [KID, KEY, IV]);
    // The rest of the environment is inherited (PATH reaches the child).
    assert_ne!(s.cli.inherited(0)[0], "");
}

#[test]
fn cross_job_key_isolation_across_two_different_assets() {
    let before: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let s = setup();
    let out = s.root.join("out");
    s.manager
        .submit(&entry("asset-A", KID, KEY, IV), &out)
        .unwrap();
    wait_idle(&s.manager);
    assert_eq!(
        std::env::vars_os().collect::<Vec<_>>(),
        before,
        "between jobs"
    );
    s.manager
        .submit(&entry("asset-B", B_KID, B_KEY, B_IV), &out)
        .unwrap();
    wait_idle(&s.manager);
    let (a, b) = (s.cli.drm(0), s.cli.drm(1));
    assert_eq!(a, [KID, KEY, IV]);
    assert_eq!(b, [B_KID, B_KEY, B_IV]);
    for value in [B_KID, B_KEY, B_IV] {
        assert!(!a.iter().any(|v| v == value));
    }
    for value in [KID, KEY, IV] {
        assert!(!b.iter().any(|v| v == value));
    }
    assert_eq!(std::env::vars_os().collect::<Vec<_>>(), before, "after");
}

#[test]
fn submit_never_mutates_the_process_environment() {
    let before: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    let s = setup();
    s.manager
        .submit(&entry("asset-1", KID, KEY, IV), &s.root.join("out"))
        .unwrap();
    wait_idle(&s.manager);
    assert_eq!(std::env::vars_os().collect::<Vec<_>>(), before);
}

#[test]
fn entry_key_material_wins_over_any_ambient_drm_var() {
    let wrong = "deadbeefdeadbeefdeadbeefdeadbeef";
    let env: EnvSource = Arc::new(move || {
        let mut env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
        for name in ["DRM_KID", "DRM_KEY", "DRM_IV"] {
            env.push((name.into(), wrong.into()));
        }
        env
    });
    let s = setup_with(env);
    s.manager
        .submit(&entry("asset-1", KID, KEY, IV), &s.root.join("out"))
        .unwrap();
    assert_eq!(s.cli.drm(0), [KID, KEY, IV]);
}

#[test]
fn module_source_never_reads_the_process_environment_for_drm_vars() {
    let source = include_str!("../src/jobs.rs");
    for forbidden in [
        "var(\"DRM_",
        "var_os(\"DRM_",
        "env::var(\"DRM",
        "env::var_os(\"DRM",
    ] {
        assert!(!source.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn second_submit_while_a_job_is_running_raises() {
    let s = setup();
    s.cli.hold();
    let out = s.root.join("out");
    s.manager
        .submit(&entry("asset-A", KID, KEY, IV), &out)
        .unwrap();
    let second = s
        .manager
        .submit(&entry("asset-B", B_KID, B_KEY, B_IV), &out);
    assert!(matches!(second, Err(JobError::AlreadyRunning)));
    s.cli.wait_calls(1);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(s.cli.calls(), 1, "the refused submit never spawned");
}

#[test]
fn status_unknown_job_id_raises_job_not_found() {
    let s = setup();
    assert!(matches!(
        s.manager.status("never-submitted"),
        Err(JobError::NotFound(_))
    ));
}

#[test]
fn submit_is_race_free_under_concurrent_calls() {
    let s = setup();
    s.cli.hold();
    let out = s.root.join("out");
    let barrier = Arc::new(Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|index| {
            let manager = s.manager.clone();
            let barrier = barrier.clone();
            let out = out.clone();
            std::thread::spawn(move || {
                barrier.wait();
                manager.submit(&entry(&format!("asset-{index}"), KID, KEY, IV), &out)
            })
        })
        .collect();
    let outcomes: Vec<_> = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect();
    let handles: Vec<_> = outcomes
        .iter()
        .filter_map(|outcome| outcome.as_ref().ok())
        .collect();
    let refused = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, Err(JobError::AlreadyRunning)))
        .count();
    assert_eq!((handles.len(), refused), (1, 7));
    s.cli.wait_calls(1);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(s.cli.calls(), 1, "exactly one subprocess ever spawned");
    let job_id = handles[0].job_id.clone();
    s.cli.release(0);
    support::wait_until(|| s.manager.status(&job_id).unwrap().state == "succeeded");
}

#[test]
fn status_of_a_finished_job_survives_a_newer_submit() {
    let s = setup();
    s.cli.exit_code(3);
    let out = s.root.join("out");
    let first = s
        .manager
        .submit(&entry("asset-A", KID, KEY, IV), &out)
        .unwrap();
    wait_idle(&s.manager);
    let second = s
        .manager
        .submit(&entry("asset-B", B_KID, B_KEY, B_IV), &out)
        .unwrap();
    let superseded = s.manager.status(&first.job_id).unwrap();
    assert_eq!(
        (superseded.state, superseded.exit_code),
        ("failed", Some(3))
    );
    assert_eq!(
        superseded.log_path.file_name().unwrap().to_str().unwrap(),
        format!("{}.jsonl", first.job_id)
    );
    assert_eq!(
        s.manager.status(&second.job_id).unwrap().job_id,
        second.job_id
    );
    assert!(matches!(
        s.manager.status("never-submitted"),
        Err(JobError::NotFound(_))
    ));
}

#[test]
fn tail_log_of_a_superseded_job_completes_with_its_own_lines() {
    let s = setup();
    s.cli.lines(&[r#"{"stage": "fake", "message": "hello"}"#]);
    let out = s.root.join("out");
    let first = s
        .manager
        .submit(&entry("asset-A", KID, KEY, IV), &out)
        .unwrap();
    assert!(
        !tail(&s.manager, &first.job_id).is_empty(),
        "runs it to completion"
    );
    let second = s
        .manager
        .submit(&entry("asset-B", B_KID, B_KEY, B_IV), &out)
        .unwrap();
    let lines = tail(&s.manager, &first.job_id);
    let messages: Vec<String> = lines
        .iter()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["message"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(messages, ["hello"]);
    let last = s.manager.status(&first.job_id).unwrap();
    assert_eq!((last.state, last.exit_code), ("succeeded", Some(0)));
    tail(&s.manager, &second.job_id);
}

#[test]
fn tail_log_keeps_the_lines_a_child_writes_as_it_exits() {
    // The drain itself is pinned by the unit test of `TailLog`; here a real child writes its
    // last line and exits at once, many times, and the tail never loses it.
    let s = setup();
    s.cli.lines(&[
        r#"{"stage": "fake", "message": "first"}"#,
        r#"{"stage": "report", "message": "final-summary"}"#,
    ]);
    for _ in 0..10 {
        let handle = s
            .manager
            .submit(&entry("asset-1", KID, KEY, IV), &s.root.join("out"))
            .unwrap();
        let messages: Vec<String> = tail(&s.manager, &handle.job_id)
            .iter()
            .map(|line| {
                serde_json::from_str::<Value>(line).unwrap()["message"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(messages, ["first", "final-summary"]);
    }
}

#[test]
fn tail_log_wraps_a_non_json_cli_error_line() {
    let s = setup();
    s.cli.lines(&["error: --mpd: no such file"]);
    s.cli.exit_code(2);
    let handle = s
        .manager
        .submit(&entry("bogus-asset", KID, KEY, IV), &s.root.join("out"))
        .unwrap();
    let lines = tail(&s.manager, &handle.job_id);
    assert_ne!(lines, Vec::<String>::new());
    for line in &lines {
        serde_json::from_str::<Value>(line).unwrap();
    }
    assert!(lines.iter().any(|line| line.contains("error:")));
}

#[test]
fn tail_log_wraps_a_trailing_unterminated_non_json_fragment() {
    let s = setup();
    std::fs::write(s.cli.dir.join("lines"), "not-json-fragment").unwrap();
    let handle = s
        .manager
        .submit(&entry("frag-asset", KID, KEY, IV), &s.root.join("out"))
        .unwrap();
    let lines = tail(&s.manager, &handle.job_id);
    assert_ne!(lines, Vec::<String>::new());
    for line in &lines {
        serde_json::from_str::<Value>(line).unwrap();
    }
    assert!(lines.iter().any(|line| line.contains("not-json-fragment")));
}

#[test]
fn submit_returns_a_running_job_handle() {
    let s = setup();
    s.cli.hold();
    let handle = s
        .manager
        .submit(&entry("asset-1", KID, KEY, IV), &s.root.join("out"))
        .unwrap();
    assert_eq!((handle.state, handle.exit_code), ("running", None));
    assert_eq!(
        handle.log_path.file_name().unwrap().to_str().unwrap(),
        format!("{}.jsonl", handle.job_id)
    );
    assert_eq!(handle.job_id.len(), 32);
}

fn tree_hash(root: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let relative = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                files.push((relative, drmr_core::snapshot::sha256_file(&path).unwrap()));
            }
        }
    }
    files.sort();
    files
}

#[test]
fn a_conversion_runs_the_full_guarantee_and_leaves_the_input_tree_byte_identical() {
    let s = setup();
    s.cli.convert();
    let catalog = s.root.join("catalog");
    let (entry_dir, _) = support::stage_entry(&catalog, "real-asset", &support::Stage::default());
    let entry = drmr_server::catalog::get_catalog_entry(&catalog, "real-asset")
        .unwrap()
        .unwrap();
    let before = tree_hash(&entry_dir);
    let output = s.root.join("output");
    let handle = s.manager.submit(&entry, &output).unwrap();
    let lines = tail(&s.manager, &handle.job_id);
    let status = s.manager.status(&handle.job_id).unwrap();
    assert!(matches!(status.state, "succeeded" | "failed") && status.exit_code.is_some());
    assert_eq!(tree_hash(&entry_dir), before, "inputs are read-only");
    let argv = s.cli.argv(0);
    assert_eq!(
        flag(&argv, "--manifest-local-url-base"),
        "/output/real-asset/cbcs/"
    );
    assert_eq!(flag(&argv, "--log-format"), "jsonl");
    assert!(
        !argv
            .iter()
            .any(|arg| arg == "--no-round-trip" || arg == "--no-manifests")
    );
    assert_eq!(s.cli.drm(0)[1], KEY);
    let text = lines.join("\n");
    assert!(!text.contains(KEY) && !text.contains(IV));
    assert_ne!(lines, Vec::<String>::new());
    for line in &lines {
        serde_json::from_str::<Value>(line).unwrap();
    }
}

#[test]
fn stopping_a_replaced_job_never_stops_the_current_job() {
    let s = setup();
    let old = s
        .manager
        .submit_command(&[], &s.manager.fresh_env())
        .unwrap();
    wait_idle(&s.manager);
    let completed = s.manager.status(&old.job_id).unwrap();
    assert_eq!(completed.state, "succeeded");
    assert!(!completed.stop_requested);
    s.cli.hold();
    let current = s
        .manager
        .submit_command(&[], &s.manager.fresh_env())
        .unwrap();
    s.cli.wait_calls(2);
    assert_eq!(s.manager.request_stop(&old.job_id).unwrap(), completed);
    assert!(s.manager.is_busy());
    let still_running = s.manager.status(&current.job_id).unwrap();
    assert_eq!(still_running.state, "running");
    assert!(!still_running.stop_requested);
    let request = s.manager.request_stop(&current.job_id).unwrap();
    assert!(request.stop_requested);
    wait_idle(&s.manager);
    let stopped = s.manager.status(&current.job_id).unwrap();
    assert_eq!(stopped.state, "interrupted");
    assert!(stopped.exit_code.is_some());
    assert_eq!(s.manager.request_stop(&current.job_id).unwrap(), stopped);
    let next = s
        .manager
        .submit_command(&[], &s.manager.fresh_env())
        .unwrap();
    assert_eq!(s.manager.status(&current.job_id).unwrap(), stopped);
    assert_ne!(next.job_id, current.job_id);
}

#[test]
fn closing_the_manager_rejects_new_spawns_and_confirms_the_owned_job() {
    let s = setup();
    s.cli.hold();
    let job = s
        .manager
        .submit_command(&[], &s.manager.fresh_env())
        .unwrap();
    s.cli.wait_calls(1);
    s.manager.begin_shutdown();
    assert!(matches!(
        s.manager.submit_command(&[], &s.manager.fresh_env()),
        Err(JobError::ShuttingDown)
    ));
    s.manager.shutdown().unwrap();
    let stopped = s.manager.status(&job.job_id).unwrap();
    assert_eq!(stopped.state, "interrupted");
    assert!(stopped.stop_requested);
    assert!(!s.manager.is_busy());
    s.manager.shutdown().unwrap();
    assert_eq!(s.cli.calls(), 1);
}
