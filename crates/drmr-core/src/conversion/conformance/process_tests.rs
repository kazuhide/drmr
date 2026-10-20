use super::*;
use std::{os::unix::fs::PermissionsExt as _, sync::Arc, time::Duration};

fn fixture(temp: &Path, body: &str) -> ManifestValidationTools {
    let script = temp.join("validator");
    std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    ManifestValidationTools {
        mediastreamvalidator: Some(script.to_str().unwrap().to_owned()),
        dash_manifest_validator: Some(script.to_str().unwrap().to_owned()),
    }
}

fn media(temp: &Path) -> std::path::PathBuf {
    let path = temp.join("media.m3u8");
    std::fs::write(&path,"#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:2\n#EXT-X-MAP:URI=\"absent-init.mp4\"\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://example.invalid/asset\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\n#EXTINF:2.0,\nabsent-segment.m4s\n#EXT-X-ENDLIST\n").unwrap();
    path
}

fn converter(sink: Arc<crate::events::Collector>, timeout: Duration) -> Converter {
    Converter::new(
        Arc::new(crate::toolrun::ToolRunner::new(
            Arc::new(crate::toolrun::Secrets::new()),
            sink,
        )),
        "unused",
        "unused",
    )
    .with_timeout(timeout)
}

fn apple_success() -> &'static str {
    "printf '{\"dataVersion\":1.2,\"validatorName\":\"mediastreamvalidator\",\"validatorVersion\":\"1.25.36\",\"url\":\"%s\",\"playlistKind\":\"media\",\"fetchCount\":1,\"parsedSegmentsCount\":1,\"processedSegmentsCount\":0,\"hasEndTag\":true,\"discontinuities\":[{\"segments\":[{\"processed\":false}]}]}' \"$6\""
}

#[test]
fn owned_runner_accepts_complete_json_and_keeps_private_diagnostics_out_of_events() {
    let temp = tempfile::tempdir().unwrap();
    let input = media(temp.path());
    let tools = fixture(
        temp.path(),
        &format!(
            "printf 'UNREGISTERED-PRIVATE-SIGNAL' >&2\n{}",
            apple_success()
        ),
    );
    let sink = Arc::new(crate::events::Collector::default());
    let report = converter(sink.clone(), Duration::from_secs(10))
        .validate_manifest(&input, OutputFormat::Hls, &tools)
        .unwrap();
    assert_eq!(report.scope, "manifest-only");
    assert_eq!(report.manifest_count, 1);
    let events = serde_json::to_string(&sink.records()).unwrap();
    assert!(!events.contains("UNREGISTERED-PRIVATE-SIGNAL"));
    assert!(!events.contains(temp.path().to_str().unwrap()));
    assert!(!serde_json::to_string(&report).unwrap().contains("skd://"));
}

#[test]
fn nonzero_and_malformed_successful_process_results_never_pass() {
    let temp = tempfile::tempdir().unwrap();
    let input = media(temp.path());
    for body in [
        "printf 'PRIVATE-RESULT'; exit 1",
        "printf '{\"passed\":true}'",
        "printf ''",
    ] {
        let tools = fixture(temp.path(), body);
        let result = converter(Arc::default(), Duration::from_secs(10)).validate_manifest(
            &input,
            OutputFormat::Hls,
            &tools,
        );
        assert!(result.is_err());
        assert!(!format!("{result:?}").contains("PRIVATE-RESULT"));
    }
}

#[test]
fn tool_capture_limits_and_timeout_remain_errors() {
    let temp = tempfile::tempdir().unwrap();
    let input = media(temp.path());
    for (body, timeout, expected) in [
        (
            "head -c 16777217 /dev/zero",
            Duration::from_secs(10),
            "manifest-validator-output-limit",
        ),
        (
            "head -c 1048577 /dev/zero >&2",
            Duration::from_secs(10),
            "manifest-validator-output-limit",
        ),
        (
            "exec sleep 60",
            Duration::from_millis(30),
            "manifest-validator-timeout",
        ),
    ] {
        let tools = fixture(temp.path(), body);
        assert_eq!(
            converter(Arc::default(), timeout)
                .validate_manifest(&input, OutputFormat::Hls, &tools)
                .unwrap_err()
                .0,
            expected
        );
    }
}

#[test]
fn changed_final_manifest_is_detected_after_a_successful_tool() {
    let temp = tempfile::tempdir().unwrap();
    let input = media(temp.path());
    let quoted = format!("'{}'", input.to_str().unwrap().replace('\'', "'\\''"));
    let tools = fixture(
        temp.path(),
        &format!("printf '#EXTM3U\\n' > {quoted}\n{}", apple_success()),
    );
    assert_eq!(
        converter(Arc::default(), Duration::from_secs(10))
            .validate_manifest(&input, OutputFormat::Hls, &tools)
            .unwrap_err()
            .0,
        "manifest-changed-during-validation"
    );
}

#[test]
fn missing_configured_executable_returns_only_a_fixed_error() {
    let temp = tempfile::tempdir().unwrap();
    let input = media(temp.path());
    let tools = ManifestValidationTools {
        mediastreamvalidator: Some(
            temp.path()
                .join("missing-private-tool")
                .to_str()
                .unwrap()
                .to_owned(),
        ),
        ..Default::default()
    };
    assert_eq!(
        converter(Arc::default(), Duration::from_secs(10))
            .validate_manifest(&input, OutputFormat::Hls, &tools)
            .unwrap_err()
            .0,
        "manifest-validator-unavailable"
    );
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[test]
fn a_replaced_private_workspace_preserves_the_foreign_directory_on_failure() {
    replaced_workspace(true, OutputFormat::Hls, false);
}

#[test]
fn a_replaced_visible_workspace_never_passes_or_deletes_the_foreign_directory() {
    for format in [OutputFormat::Hls, OutputFormat::Dash] {
        for success in [false, true] {
            replaced_workspace(false, format, success);
        }
    }
}

fn replaced_workspace(outer: bool, format: OutputFormat, success: bool) {
    use std::os::unix::fs::MetadataExt as _;

    let temp = tempfile::tempdir().unwrap();
    let input = if format == OutputFormat::Dash {
        let input = temp.path().join("manifest.mpd");
        std::fs::write(&input, "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"/>").unwrap();
        input
    } else {
        media(temp.path())
    };
    let original = std::fs::read(&input).unwrap();
    let foreign = temp.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::set_permissions(&foreign, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(foreign.join("sentinel"), b"foreign bytes must remain").unwrap();
    let identity = std::fs::metadata(&foreign).unwrap();
    let observed = temp.path().join("workspace-path");
    let moved = temp.path().join("moved-original");
    let find_outer = if outer {
        "case \"$workspace\" in */.drmr-convert-*/media) workspace=$(dirname \"$workspace\");; esac\n"
    } else {
        ""
    };
    let result_body = if success {
        if format == OutputFormat::Dash {
            dash_success(&original)
        } else {
            apple_success().to_owned()
        }
    } else {
        "exit 1".to_owned()
    };
    let tools = fixture(
        temp.path(),
        &format!(
            "workspace=$(dirname \"$6\")\n{find_outer}printf '%s' \"$workspace\" > {observed}\nmv \"$workspace\" {moved}\nmv {foreign} \"$workspace\"\n{result_body}",
            observed = quote(observed.to_str().unwrap()),
            moved = quote(moved.to_str().unwrap()),
            foreign = quote(foreign.to_str().unwrap())
        ),
    );
    let result = converter(Arc::default(), Duration::from_secs(10))
        .validate_manifest(&input, format, &tools);
    let workspace = std::fs::read_to_string(observed).unwrap();
    let replacement = Path::new(&workspace);
    let retained = std::fs::read(replacement.join("sentinel"));
    // Restore only the test-created replacement, even when a regression fails.
    if let Ok(metadata) = std::fs::symlink_metadata(replacement) {
        assert_eq!(
            (metadata.dev(), metadata.ino()),
            (identity.dev(), identity.ino())
        );
        std::fs::rename(replacement, &foreign).unwrap();
    }
    // Once the fixture-owned replacement is restored, remove only an empty
    // retained stage container. The uncertain original payload is under temp.
    if !outer
        && replacement.file_name().is_some_and(|name| name == "media")
        && let Some(parent) = replacement.parent()
        && parent
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".drmr-convert-"))
        && parent.is_dir()
    {
        std::fs::remove_dir(parent).unwrap();
    }
    assert_eq!(
        result.unwrap_err().0,
        if success {
            "manifest-validation-workspace-changed"
        } else {
            "manifest-validator-failed"
        }
    );
    assert_eq!(retained.unwrap(), b"foreign bytes must remain");
    assert_eq!(std::fs::read(input).unwrap(), original);
    assert!(moved.is_dir());
}

#[test]
fn changed_private_hls_copy_never_passes() {
    private_hls_copy_change(false);
}

#[test]
fn replaced_private_hls_copy_with_identical_bytes_never_passes() {
    private_hls_copy_change(true);
}

fn private_hls_copy_change(replacement: bool) {
    let temp = tempfile::tempdir().unwrap();
    let input = media(temp.path());
    let original = std::fs::read(&input).unwrap();
    let mutation = if replacement {
        "cp \"$6\" \"$6.replacement\"\nmv \"$6.replacement\" \"$6\""
    } else {
        "printf '#EXTM3U\\n' > \"$6\""
    };
    let tools = fixture(temp.path(), &format!("{}\n{mutation}", apple_success()));
    let result = converter(Arc::default(), Duration::from_secs(10)).validate_manifest(
        &input,
        OutputFormat::Hls,
        &tools,
    );
    assert_eq!(std::fs::read(input).unwrap(), original);
    assert!(result.is_err(), "changed private copy was accepted");
    assert_eq!(
        result.unwrap_err().0,
        "manifest-validation-workspace-changed"
    );
}

fn dash_success(bytes: &[u8]) -> String {
    format!(
        "printf '%s' '{{\"kind\":\"drmr-dash-manifest-validation\",\"schema_version\":1,\"scope\":\"manifest-only\",\"engine\":\"dash-if\",\"engine_revision\":null,\"manifest_sha256\":\"{}\",\"passed\":true,\"checks\":{{\"xsd\":{{\"completed\":true,\"passed\":true,\"issue_count\":0}},\"schematron\":{{\"completed\":true,\"passed\":true,\"issue_count\":0}}}}}}'",
        digest(bytes)
    )
}

#[test]
fn changed_private_dash_manifest_and_adapter_never_pass() {
    for target in ["$5", "$6"] {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("manifest.mpd");
        let original = b"<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"/>";
        std::fs::write(&input, original).unwrap();
        let tools = fixture(
            temp.path(),
            &format!(
                "{}\nprintf 'changed private copy' > \"{target}\"",
                dash_success(original)
            ),
        );
        let result = converter(Arc::default(), Duration::from_secs(10)).validate_manifest(
            &input,
            OutputFormat::Dash,
            &tools,
        );
        assert_eq!(std::fs::read(input).unwrap(), original);
        assert!(result.is_err(), "changed private copy was accepted");
        assert_eq!(
            result.unwrap_err().0,
            "manifest-validation-workspace-changed"
        );
    }
}

#[test]
fn private_workspaces_are_removed_after_success_and_failure() {
    for format in [OutputFormat::Hls, OutputFormat::Dash] {
        for success in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let observed = temp.path().join("workspace-path");
            let input = if format == OutputFormat::Dash {
                let input = temp.path().join("manifest.mpd");
                std::fs::write(&input, "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"/>").unwrap();
                input
            } else {
                media(temp.path())
            };
            let original = std::fs::read(&input).unwrap();
            let result_body = if success {
                if format == OutputFormat::Dash {
                    dash_success(&original)
                } else {
                    apple_success().to_owned()
                }
            } else {
                "exit 1".to_owned()
            };
            let tools = fixture(
                temp.path(),
                &format!(
                    "workspace=$(dirname \"$6\")\ncase \"$workspace\" in */.drmr-convert-*/media) workspace=$(dirname \"$workspace\");; esac\nprintf '%s' \"$workspace\" > {}\n{result_body}",
                    quote(observed.to_str().unwrap())
                ),
            );
            let result = converter(Arc::default(), Duration::from_secs(10))
                .validate_manifest(&input, format, &tools);
            assert_eq!(result.is_ok(), success, "{result:?}");
            assert_eq!(std::fs::read(input).unwrap(), original);
            assert!(!Path::new(&std::fs::read_to_string(observed).unwrap()).exists());
        }
    }
}

#[test]
fn nested_hls_copies_are_checked_together_before_success() {
    for change_previous in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("nested")).unwrap();
        let input = temp.path().join("master.m3u8");
        let master = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=500000\nnested/media.m3u8\n";
        std::fs::write(&input, master).unwrap();
        let child = media(&temp.path().join("nested"));
        let original_child = std::fs::read(&child).unwrap();
        let mutation = if change_previous {
            "printf '#EXTM3U\\n' > \"$(dirname \"$6\")/../master.m3u8\""
        } else {
            ""
        };
        let tools = fixture(
            temp.path(),
            &format!(
                "case \"$6\" in */master.m3u8)\nprintf '{{\"dataVersion\":1.2,\"validatorName\":\"mediastreamvalidator\",\"validatorVersion\":\"1.25.36\",\"url\":\"%s\",\"playlistKind\":\"multivariant\",\"variants\":[{{\"playlistKind\":\"media\",\"fetchCount\":1,\"parsedSegmentsCount\":1,\"processedSegmentsCount\":0,\"hasEndTag\":true,\"discontinuities\":[{{\"segments\":[{{\"processed\":false}}]}}]}}]}}' \"$6\";;\n*)\n{}\n{mutation};;\nesac",
                apple_success()
            ),
        );
        let result = converter(Arc::default(), Duration::from_secs(10)).validate_manifest(
            &input,
            OutputFormat::Hls,
            &tools,
        );
        if change_previous {
            assert_eq!(
                result.unwrap_err().0,
                "manifest-validation-workspace-changed"
            );
        } else {
            assert_eq!(result.unwrap().manifest_count, 2);
        }
        assert_eq!(std::fs::read(input).unwrap(), master);
        assert_eq!(std::fs::read(child).unwrap(), original_child);
    }
}

#[test]
fn timeout_and_cancellation_reap_descendants_before_removing_adapter_workspace() {
    use std::time::Instant;
    for cancel in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("manifest.mpd");
        std::fs::write(&input, "<MPD xmlns=\"urn:mpeg:dash:schema:mpd:2011\"/>").unwrap();
        let ready = temp.path().join("ready");
        let body = format!(
            "adapter_dir=$(dirname \"$5\")\nmkdir \"$adapter_dir/engine-owned\"\nprintf 'private' > \"$adapter_dir/engine-owned/report\"\nsleep 60 &\nchild=$!\nprintf '%s\\n%s\\n%s\\n' \"$adapter_dir\" \"$$\" \"$child\" > {ready}.tmp\nmv {ready}.tmp {ready}\nwait \"$child\"",
            ready = quote(ready.to_str().unwrap())
        );
        let tools = fixture(temp.path(), &body);
        let stop = crate::concurrency::Stop::new();
        let converter = converter(
            Arc::default(),
            Duration::from_secs(if cancel { 10 } else { 2 }),
        )
        .with_stop(stop.clone());
        let handle = std::thread::spawn(move || {
            converter.validate_manifest(&input, OutputFormat::Dash, &tools)
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let observed = std::fs::read_to_string(&ready);
        if cancel || observed.is_err() {
            stop.set();
        }
        let result = handle.join().unwrap();
        let observed = observed.expect("validator readiness handshake did not complete");
        let values: Vec<_> = observed.lines().collect();
        assert_eq!(values.len(), 3);
        assert_eq!(
            result.unwrap_err().0,
            if cancel {
                "cancelled"
            } else {
                "manifest-validator-timeout"
            }
        );
        assert!(!Path::new(values[0]).exists(), "owned workspace remains");
        for value in &values[1..] {
            let pid = rustix::process::Pid::from_raw(value.parse().unwrap()).unwrap();
            assert_eq!(
                rustix::process::test_kill_process(pid),
                Err(rustix::io::Errno::SRCH),
                "owned validator process remains"
            );
        }
    }
}

#[test]
fn cleanup_fork_preserves_existing_owner_sinks_and_executable_search_path() {
    let temp = tempfile::tempdir().unwrap();
    fixture(temp.path(), "printf 'complete'");
    let sink = Arc::new(crate::events::Collector::default());
    let observer = Arc::new(crate::events::Collector::default());
    let existing = Arc::new(crate::toolrun::CleanupOwner::new());
    let runner = crate::toolrun::ToolRunner::new(Arc::default(), sink.clone())
        .with_cleanup_owner(existing.clone())
        .with_path(temp.path().as_os_str())
        .observed(observer.clone());
    let (fork, owner) = runner.ensuring_cleanup_owner();
    assert!(Arc::ptr_eq(&owner, &existing));
    let result = fork
        .run_tool(
            &["validator".into()],
            "fixture",
            &RunOptions {
                timeout: Some(Duration::from_secs(10)),
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(result.stdout, b"complete");
    assert!(!owner.has_pending());
    assert!(sink.records().len() >= 2);
    assert_eq!(sink.records(), observer.records());
}

#[test]
fn a_denied_manifest_call_preserves_an_unrelated_retained_tree_for_its_caller() {
    use std::time::Instant;
    let temp = tempfile::tempdir().unwrap();
    let ready = temp.path().join("ready");
    let tools = fixture(
        temp.path(),
        &format!(
            "printf '%s' \"$$\" > {}\nexec sleep 60",
            quote(ready.to_str().unwrap())
        ),
    );
    let owner = Arc::new(crate::toolrun::CleanupOwner::new());
    let runner = Arc::new(
        crate::toolrun::ToolRunner::new(Arc::default(), Arc::new(crate::events::NoSink))
            .with_cleanup_owner(owner.clone()),
    );
    let failed = runner.run_tool_injected_cleanup(
        &[tools.mediastreamvalidator.as_ref().unwrap().clone()],
        &RunOptions {
            timeout: Some(Duration::from_millis(30)),
            ..RunOptions::default()
        },
        |_| {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !ready.is_file() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(std::io::Error::other("injected cleanup uncertainty"))
        },
    );
    assert_eq!(failed.unwrap_err().kind, "cleanup-unconfirmed");
    assert!(owner.has_pending());
    let input = media(temp.path());
    let result = Converter::new(runner, "unused", "unused")
        .with_timeout(Duration::from_secs(10))
        .validate_manifest(&input, OutputFormat::Hls, &tools);
    let pending_when_returned = owner.has_pending();
    let pid =
        rustix::process::Pid::from_raw(std::fs::read_to_string(&ready).unwrap().parse().unwrap())
            .unwrap();
    let alive_when_returned = rustix::process::test_kill_process(pid).is_ok();
    // The fixture's original caller always reaps its own retained tree, including
    // when a regression wrongly adopted it or unexpectedly started another tool.
    owner.retry_finish().unwrap();
    assert_eq!(
        result.unwrap_err().0,
        "manifest-validator-cleanup-unconfirmed"
    );
    assert!(pending_when_returned, "unrelated retained tree was adopted");
    assert!(alive_when_returned, "unrelated retained tree was reaped");
    assert!(!owner.has_pending());
    assert_eq!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    );
}

type Reentry = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct ReentrantSink {
    action: std::sync::Mutex<Option<Reentry>>,
}

impl crate::events::Sink for ReentrantSink {
    fn emit(&self, record: &crate::events::Record) {
        if record.get("tool").and_then(serde_json::Value::as_str) != Some("outer-fixture")
            || record.get("event").and_then(serde_json::Value::as_str) != Some("tool-output")
        {
            return;
        }
        let action = self.action.lock().unwrap().take();
        if let Some(action) = action {
            action();
        }
    }
}

fn isolated_manifest_test(name: &str) -> bool {
    const MARKER: &str = "DRMR_TEST_MANIFEST_PROCESS_CHILD";
    if std::env::var_os(MARKER).as_deref() == Some(std::ffi::OsStr::new(name)) {
        return false;
    }
    let workspace = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", name, "--nocapture"])
        .env(MARKER, name)
        .env("TMPDIR", workspace.path())
        .stdin(std::process::Stdio::null());
    let mut child = crate::native::ManagedChild::spawn(command, true).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    child.finish().unwrap();
    assert!(
        status.is_some_and(|status| status.success()),
        "manifest validation fixture failed or blocked"
    );
    true
}

#[test]
fn reentrant_sink_does_not_wait_for_an_outer_active_cleanup_lease() {
    if isolated_manifest_test(
        "conversion::conformance::process_tests::reentrant_sink_does_not_wait_for_an_outer_active_cleanup_lease",
    ) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let input = media(temp.path());
    let marker = temp.path().join("nested-tool-started");
    let tools = fixture(
        temp.path(),
        &format!(
            "printf 'started' > {}\n{}",
            quote(marker.to_str().unwrap()),
            apple_success()
        ),
    );
    let outer = temp.path().join("outer");
    std::fs::write(&outer, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&outer, std::fs::Permissions::from_mode(0o700)).unwrap();
    let owner = Arc::new(crate::toolrun::CleanupOwner::new());
    let sink = Arc::new(ReentrantSink::default());
    let runner = Arc::new(
        crate::toolrun::ToolRunner::new(Arc::default(), sink.clone())
            .with_cleanup_owner(owner.clone()),
    );
    let observed = Arc::new(std::sync::Mutex::new(None));
    let nested_observed = observed.clone();
    let nested_owner = owner.clone();
    let nested_runner = runner.clone();
    *sink.action.lock().unwrap() = Some(Box::new(move || {
        let result = Converter::new(nested_runner, "unused", "unused")
            .with_timeout(Duration::from_secs(10))
            .validate_manifest(&input, OutputFormat::Hls, &tools);
        assert!(nested_owner.has_pending(), "outer lease was released early");
        assert!(!marker.exists(), "denied validator process was started");
        *nested_observed.lock().unwrap() = Some(result.unwrap_err().0);
    }));
    let completed = runner
        .run_tool(
            &[outer.to_str().unwrap().to_owned()],
            "outer-fixture",
            &RunOptions {
                timeout: Some(Duration::from_secs(10)),
                ..RunOptions::default()
            },
        )
        .unwrap();
    assert_eq!(completed.returncode, Some(0));
    assert_eq!(
        *observed.lock().unwrap(),
        Some("manifest-validator-cleanup-unconfirmed")
    );
    assert!(!owner.has_pending());
}

struct PanicAfterRetention {
    copy: std::path::PathBuf,
    ready: std::path::PathBuf,
    observed: Arc<std::sync::atomic::AtomicBool>,
}

impl crate::events::Sink for PanicAfterRetention {
    fn emit(&self, record: &crate::events::Record) {
        if record.get("level").and_then(serde_json::Value::as_str) != Some("error") {
            return;
        }
        assert!(
            self.copy.is_file(),
            "private copy was removed before retention"
        );
        let pid = rustix::process::Pid::from_raw(
            std::fs::read_to_string(&self.ready)
                .unwrap()
                .parse()
                .unwrap(),
        )
        .unwrap();
        assert!(rustix::process::test_kill_process(pid).is_ok());
        self.observed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        panic!("synthetic failure sink panic after retention");
    }
}

struct PanicOnActivity(Arc<std::sync::atomic::AtomicUsize>);

impl crate::events::Sink for PanicOnActivity {
    fn emit(&self, _record: &crate::events::Record) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        panic!("activity must not be called while cleanup unwinds");
    }
}

#[test]
fn a_failure_sink_panic_reaps_its_own_tree_before_private_files_are_removed() {
    if isolated_manifest_test(
        "conversion::conformance::process_tests::a_failure_sink_panic_reaps_its_own_tree_before_private_files_are_removed",
    ) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let ready = temp.path().join("ready");
    let tools = fixture(
        temp.path(),
        &format!(
            "printf '%s' \"$$\" > {}\nexec sleep 60",
            quote(ready.to_str().unwrap())
        ),
    );
    let owner = Arc::new(crate::toolrun::CleanupOwner::new());
    let retained = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let activity_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut outer = None;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut workspace = workspace::Workspace::new().unwrap();
        let copy = workspace
            .write(Path::new("manifest.mpd"), b"private manifest bytes")
            .unwrap();
        outer = Some(workspace.path().parent().unwrap().to_owned());
        let sink = Arc::new(PanicAfterRetention {
            copy,
            ready: ready.clone(),
            observed: retained.clone(),
        });
        let runner = Arc::new(
            crate::toolrun::ToolRunner::new(Arc::default(), sink).with_cleanup_owner(owner.clone()),
        );
        let converter = Converter::new(runner.clone(), "unused", "unused")
            .with_activity_sink(Arc::new(PanicOnActivity(activity_calls.clone())));
        let mut invocation = ManifestInvocation {
            converter: &converter,
            cleanup: None,
        };
        let _ = runner.run_tool_injected_cleanup_with_receipt(
            &[tools.mediastreamvalidator.as_ref().unwrap().clone()],
            &RunOptions {
                timeout: Some(Duration::from_millis(100)),
                ..RunOptions::default()
            },
            &mut invocation.cleanup,
            |_| {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !ready.is_file() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(std::io::Error::other("injected cleanup uncertainty"))
            },
        );
    }));
    let pid =
        rustix::process::Pid::from_raw(std::fs::read_to_string(ready).unwrap().parse().unwrap())
            .unwrap();
    let pending_when_caught = owner.has_pending();
    let alive_when_caught = rustix::process::test_kill_process(pid).is_ok();
    // Keep negative controls bounded even if invocation Drop omits cleanup.
    owner.retry_finish().unwrap();
    assert!(result.is_err());
    assert!(retained.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!pending_when_caught, "own retained tree outlived unwinding");
    assert!(!alive_when_caught, "own retained process remains");
    assert!(!outer.unwrap().exists());
    assert_eq!(activity_calls.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    );
}
