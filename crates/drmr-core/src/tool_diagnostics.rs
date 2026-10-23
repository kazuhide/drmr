//! Bounded, read-only dependency checks shared by the CLI and native application.
//!
//! Discovery never executes a tool. Explicit probing checks advertised options, not
//! media compatibility, license delivery, or the correctness of a particular build.

use crate::events::NoSink;
use crate::toolrun::{CleanupOwner, RunOptions, Secrets, ToolRunner};
use regex::Regex;
use serde::Serialize;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

const TOOLS: [(&str, &str, &str); 3] = [
    ("ffmpeg", "media processing (FFmpeg)", "9.0.1"),
    ("ffprobe", "media inspection (FFmpeg)", "9.0.1"),
    (
        "packager",
        "media encryption and packaging (Shaka Packager)",
        "3.8.0",
    ),
];

/// A selected processing route or the complete dependency set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Profile(&'static str);

impl Profile {
    pub const ALL: Self = Self("all");

    pub fn parse(value: &str) -> Option<Self> {
        [
            "all",
            "progressive-mp4",
            "fragmented-mp4",
            "dash-presentation",
            "fmp4-hls-presentation",
            "mpeg-ts-hls",
            "mpeg-ts-sample-aes-hls",
            "mpeg-ts-hls-ts",
        ]
        .into_iter()
        .find(|name| *name == value)
        .map(Self)
    }

    fn required(self, tool: &str) -> bool {
        match tool {
            "packager" => self.0 != "progressive-mp4",
            _ => true,
        }
    }
}

/// One fixed, advertised command-line option. Presence is not a media execution test.
#[derive(Clone, Debug, Serialize)]
pub struct FeatureCheck {
    pub name: &'static str,
    pub available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct ToolDiagnostic {
    pub name: &'static str,
    pub purpose: &'static str,
    pub available: bool,
    pub required: bool,
    pub path: Option<PathBuf>,
    pub path_status: &'static str,
    pub minimum_version: &'static str,
    pub version: Option<String>,
    pub version_status: &'static str,
    pub feature_status: &'static str,
    pub features: Vec<FeatureCheck>,
    pub failure_code: Option<&'static str>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiagnosticReport {
    pub schema_version: u8,
    pub mode: &'static str,
    pub profile: &'static str,
    pub ready: bool,
    pub media_compatibility_checked: bool,
    pub cleanup_status: &'static str,
    pub cancelled: bool,
    pub tools: Vec<ToolDiagnostic>,
}

impl DiagnosticReport {
    /// A native-dialog summary. Paths are quoted to escape control characters.
    pub fn summary(&self) -> String {
        let mut text = String::from("Tools in this application's launch PATH:\n\n");
        for tool in &self.tools {
            writeln!(
                text,
                "{}: {}{}",
                tool.name,
                if tool.available { "found" } else { "missing" },
                if tool.required {
                    " (required)"
                } else {
                    " (optional)"
                }
            )
            .expect("String write");
            if let Some(path) = &tool.path {
                writeln!(
                    text,
                    "  {}",
                    serde_json::to_string(path).unwrap_or_else(|_| "[unrepresentable path]".into())
                )
                .expect("String write");
            }
            writeln!(
                text,
                "  version: {} [{}]; options: {}",
                tool.version.as_deref().unwrap_or("unknown"),
                tool.version_status,
                tool.feature_status
            )
            .expect("String write");
            if let Some(code) = tool.failure_code {
                writeln!(text, "  {code}").expect("String write");
            }
        }
        text.push_str("\nVersion/help checks do not prove media compatibility.\n\nIf required checks fail, install compatible tools and reopen drmr from an environment whose PATH contains them. Shell startup files and extra installation folders are not searched.");
        text
    }
}

/// A serial diagnostic operation that retains any unconfirmed process owner.
/// Keep this session alive until cleanup is confirmed; explicitly retry on failure.
/// Each call keeps its three-second admission/execution budget and native cleanup
/// keeps its separate existing deadline. No window or media processing is started.
pub struct DiagnosticSession {
    profile: Profile,
    probe: bool,
    cleanup: Arc<CleanupOwner>,
    runner: ToolRunner,
    stop: crate::concurrency::Stop,
    state: Mutex<SessionState>,
}

#[derive(Default)]
struct SessionState {
    running: bool,
    report: Option<Arc<DiagnosticReport>>,
}

impl DiagnosticSession {
    pub fn new(profile: Profile, probe: bool) -> Self {
        let cleanup = Arc::new(CleanupOwner::new());
        let runner = ToolRunner::new(Arc::new(Secrets::new()), Arc::new(NoSink))
            .with_cleanup_owner(Arc::clone(&cleanup));
        Self {
            profile,
            probe,
            cleanup,
            runner,
            stop: crate::concurrency::Stop::new(),
            state: Mutex::new(SessionState::default()),
        }
    }

    /// Use the caller's termination signal before the diagnostic worker starts.
    #[must_use]
    pub fn with_stop(mut self, stop: crate::concurrency::Stop) -> Self {
        self.stop = stop;
        self
    }

    /// Observe tool calls without changing their cleanup owner or admission limits.
    /// Records include diagnostic arguments and output; project only fixed labels and
    /// numeric fields when presenting them as user-facing progress.
    #[must_use]
    pub fn with_activity_sink(mut self, sink: Arc<dyn crate::events::Sink>) -> Self {
        self.runner = self.runner.observed(sink);
        self
    }

    /// Run once, or return the same completed report. Concurrent runs are refused.
    pub fn run(&self) -> std::io::Result<Arc<DiagnosticReport>> {
        let path = std::env::var_os("PATH");
        self.run_with(path.as_deref(), probe_tool)
    }

    fn run_with(
        &self,
        path: Option<&std::ffi::OsStr>,
        probe: impl FnMut(
            &ToolRunner,
            &mut ToolDiagnostic,
            &crate::concurrency::Stop,
        ) -> Result<(), &'static str>,
    ) -> std::io::Result<Arc<DiagnosticReport>> {
        {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(report) = &state.report {
                return Ok(Arc::clone(report));
            }
            if state.running {
                return Err(std::io::Error::other("diagnostics-still-running"));
            }
            state.running = true;
        }
        let report = Arc::new(diagnose(self, path, probe));
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.running = false;
        state.report = Some(Arc::clone(&report));
        Ok(report)
    }

    /// Stop the active call and prevent every subsequent probe from starting.
    pub fn request_stop(&self) {
        self.stop.set();
    }

    /// Whether the worker finished and no active or retained tree remains.
    pub fn cleanup_confirmed(&self) -> bool {
        !self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running
            && !self.cleanup.has_pending()
    }

    /// Retain foreground ownership until cleanup is confirmed after the worker returns.
    ///
    /// Each native retry has its existing cleanup deadline. Repeated failures keep
    /// this call waiting, with one fixed notification and a one-second pause between
    /// attempts; Stop does not authorize abandoning the retained child. The original
    /// cached report remains a record of the probe attempt, not a current cleanup view.
    pub fn wait_for_cleanup(&self, waiting: impl FnMut()) {
        self.wait_for_cleanup_with(
            || self.retry_cleanup(),
            waiting,
            || std::thread::sleep(Duration::from_secs(1)),
        );
    }

    fn wait_for_cleanup_with(
        &self,
        mut retry: impl FnMut() -> std::io::Result<()>,
        mut waiting: impl FnMut(),
        mut pause: impl FnMut(),
    ) {
        if self.cleanup_confirmed() {
            return;
        }
        self.request_stop();
        let mut notified = false;
        loop {
            let _ = retry();
            if self.cleanup_confirmed() {
                return;
            }
            if !notified {
                waiting();
                notified = true;
            }
            pause();
        }
    }

    /// Retry previously unconfirmed cleanup once, after the worker has returned.
    pub fn retry_cleanup(&self) -> std::io::Result<()> {
        self.request_stop();
        if self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .running
        {
            return Err(std::io::Error::other("diagnostics-still-running"));
        }
        self.cleanup.retry_finish()
    }
}

// Discovery uses exactly the launcher's PATH and never runs a shell or guesses folders.
fn diagnose(
    session: &DiagnosticSession,
    path: Option<&std::ffi::OsStr>,
    mut run_probe: impl FnMut(
        &ToolRunner,
        &mut ToolDiagnostic,
        &crate::concurrency::Stop,
    ) -> Result<(), &'static str>,
) -> DiagnosticReport {
    let profile = session.profile;
    let probe = session.probe;
    let directories: Vec<_> = path
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect();
    let runner = &session.runner;
    let tools: Vec<_> = TOOLS
        .iter()
        .map(|&(name, purpose, minimum_version)| {
            let executable = if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.into()
            };
            let resolved = directories
                .iter()
                .map(|p| p.join(&executable))
                .find(|p| is_executable(p))
                .and_then(|p| p.canonicalize().ok());
            let available = resolved.is_some();
            let representable = resolved.as_ref().is_none_or(|p| p.to_str().is_some());
            let mut tool = ToolDiagnostic {
                name,
                purpose,
                minimum_version,
                available,
                required: profile.required(name),
                path: resolved.filter(|_| representable),
                path_status: if !representable {
                    "not-utf8"
                } else if available {
                    "resolved"
                } else {
                    "missing"
                },
                version: None,
                version_status: "not-checked",
                feature_status: "not-checked",
                features: Vec::new(),
                failure_code: if representable {
                    None
                } else {
                    Some("probe-path-not-utf8")
                },
            };
            if probe && (session.stop.is_set() || session.cleanup.has_pending()) {
                tool.failure_code = Some("probe-not-started");
            } else if probe
                && tool.available
                && tool.failure_code.is_none()
                && let Err(code) = run_probe(runner, &mut tool, &session.stop)
            {
                tool.failure_code = Some(code);
            }
            tool
        })
        .collect();
    let cleanup_confirmed = !session.cleanup.has_pending();
    let ready = cleanup_confirmed
        && !session.stop.is_set()
        && tools.iter().filter(|t| t.required).all(|t| {
            t.available
                && (!probe
                    || (t.version_status == "supported"
                        && t.feature_status == "available"
                        && t.failure_code.is_none()))
        });
    DiagnosticReport {
        schema_version: 1,
        mode: if probe { "probe" } else { "discovery" },
        profile: profile.0,
        ready,
        media_compatibility_checked: false,
        cleanup_status: if !probe {
            "not-required"
        } else if cleanup_confirmed {
            "confirmed"
        } else {
            "unconfirmed"
        },
        cancelled: session.stop.is_set(),
        tools,
    }
}

fn output(
    runner: &ToolRunner,
    tool: &ToolDiagnostic,
    arguments: &[&str],
    usage: bool,
    stop: &crate::concurrency::Stop,
) -> Result<String, &'static str> {
    let mut argv = vec![
        tool.path
            .as_ref()
            .expect("discovered tool")
            .to_str()
            .ok_or("probe-path-not-utf8")?
            .to_owned(),
    ];
    argv.extend(arguments.iter().map(|a| (*a).to_owned()));
    let shown = [tool.name.to_owned(), "[diagnostic arguments]".to_owned()];
    let result = runner
        .run_tool(
            &argv,
            tool.name,
            &RunOptions {
                timeout: Some(Duration::from_secs(3)),
                stop: Some(stop),
                max_stdout_bytes: 1024 * 1024,
                max_stderr_bytes: 1024 * 1024,
                max_output_chars: 0,
                log_stdout: false,
                argv_redacted: Some(&shown),
                ..RunOptions::default()
            },
        )
        .map_err(|e| match e.kind {
            "timeout" => "probe-timeout",
            "cancelled" => "probe-cancelled",
            "output-limit" => "probe-output-limit",
            "cleanup-unconfirmed" => "probe-cleanup-unconfirmed",
            _ => "probe-execution-failed",
        })?;
    if result.returncode != Some(0) && !(usage && result.returncode == Some(1)) {
        return Err("probe-exit-failed");
    }
    Ok(format!(
        "{}\n{}",
        result.stdout_text(),
        result.stderr_text()
    ))
}

fn probe_tool(
    runner: &ToolRunner,
    tool: &mut ToolDiagnostic,
    stop: &crate::concurrency::Stop,
) -> Result<(), &'static str> {
    let (args, pattern, minimum): (&[&str], &str, &[u64]) = match tool.name {
        "ffmpeg" => (
            &["-version"],
            r"(?m)^ffmpeg version n?([0-9]+)\.([0-9]+)(?:\.([0-9]+))?",
            &[9, 0, 1],
        ),
        "ffprobe" => (
            &["-version"],
            r"(?m)^ffprobe version n?([0-9]+)\.([0-9]+)(?:\.([0-9]+))?",
            &[9, 0, 1],
        ),
        "packager" => (
            &["--version"],
            r"(?m)^packager version v?([0-9]+)\.([0-9]+)(?:\.([0-9]+))?",
            &[3, 8, 0],
        ),
        _ => return Err("probe-tool-unsupported"),
    };
    let banner = output(runner, tool, args, false, stop)?;
    let Some(captures) = Regex::new(pattern)
        .expect("fixed banner pattern")
        .captures(&banner)
    else {
        tool.version_status = "unrecognized";
        return Err("probe-version-unrecognized");
    };
    let numbers: Option<Vec<u64>> = (1..=3)
        .map(|i| captures.get(i).map_or(Some(0), |m| m.as_str().parse().ok()))
        .collect();
    let Some(v) = numbers else {
        tool.version_status = "unrecognized";
        return Err("probe-version-unrecognized");
    };
    tool.version = Some(format!("{}.{}.{}", v[0], v[1], v[2]));
    tool.version_status = if v.as_slice() < minimum {
        "below-minimum"
    } else {
        "supported"
    };
    tool.features = probe_features(runner, tool, stop)?;
    tool.feature_status = if tool.features.iter().all(|f| f.available) {
        "available"
    } else {
        "missing"
    };
    Ok(())
}

fn probe_features(
    runner: &ToolRunner,
    tool: &ToolDiagnostic,
    stop: &crate::concurrency::Stop,
) -> Result<Vec<FeatureCheck>, &'static str> {
    let (help, features): (String, &[&'static str]) = match tool.name {
        "ffmpeg" => (
            format!(
                "{}\n{}\n{}",
                output(
                    runner,
                    tool,
                    &["-hide_banner", "-h", "muxer=mp4"],
                    false,
                    stop
                )?,
                output(
                    runner,
                    tool,
                    &["-hide_banner", "-h", "demuxer=mov"],
                    false,
                    stop
                )?,
                output(runner, tool, &["-hide_banner", "-h", "long"], false, stop)?
            ),
            &[
                "encryption_scheme",
                "decryption_key",
                "decryption_keys",
                "thread_queue_size",
            ],
        ),
        "ffprobe" => (
            output(runner, tool, &["-hide_banner", "-h", "full"], false, stop)?,
            &[
                "show_packets",
                "show_data_hash",
                "show_entries",
                "decryption_keys",
            ],
        ),
        "packager" => (
            output(runner, tool, &["--help"], true, stop)?,
            &[
                "enable_raw_key_encryption",
                "enable_raw_key_decryption",
                "protection_scheme",
                "segment_duration",
                "mpd_output",
                "hls_master_playlist_output",
            ],
        ),
        _ => return Err("probe-tool-unsupported"),
    };
    let mut checks: Vec<_> = features
        .iter()
        .map(|&name| {
            let pattern = format!(r"(?m)^\s*--?{}(?:\s|\b)", regex::escape(name));
            FeatureCheck {
                name,
                available: Regex::new(&pattern)
                    .expect("fixed option pattern")
                    .is_match(&help),
            }
        })
        .collect();
    if tool.name == "ffmpeg" {
        for (name, component, header) in [
            ("data_muxer", "muxer=data", "Muxer data "),
            ("hash_muxer", "muxer=hash", "Muxer hash "),
            ("rawvideo_encoder", "encoder=rawvideo", "Encoder rawvideo "),
            (
                "pcm_s16le_encoder",
                "encoder=pcm_s16le",
                "Encoder pcm_s16le ",
            ),
        ] {
            let help = output(
                runner,
                tool,
                &["-hide_banner", "-h", component],
                false,
                stop,
            )?;
            checks.push(FeatureCheck {
                name,
                available: help.lines().any(|line| line.starts_with(header)),
            });
        }
    }
    Ok(checks)
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn isolated_session(profile: Profile) -> DiagnosticSession {
        let mut session = DiagnosticSession::new(profile, true);
        // Isolate fixture admission from unrelated parallel media tests while
        // preserving the actual session runner and its shared cleanup owner.
        session.runner = session
            .runner
            .observed(Arc::new(NoSink))
            .with_limiter(Arc::new(crate::resources::Limiter::new(
                1,
                1024 * 1024 * 1024,
            )));
        session
    }

    fn fixtures(root: &Path) -> std::path::PathBuf {
        for name in ["ffmpeg", "ffprobe"] {
            drmr_test_support::install(&root.join(name), &drmr_test_support::ToolSpec::default())
                .unwrap();
        }
        drmr_test_support::program_path(&root.join("ffmpeg"))
    }

    fn held_arguments(program: &Path, lock: &Path, ready: &Path) -> Vec<String> {
        vec![
            program.to_str().unwrap().into(),
            "--fixture-lock".into(),
            lock.to_str().unwrap().into(),
            ready.to_str().unwrap().into(),
        ]
    }

    fn wait_ready(path: &Path) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !path.is_file() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        path.is_file()
    }

    fn retained_probe(root: &Path) -> (DiagnosticSession, Arc<DiagnosticReport>, PathBuf) {
        let program = fixtures(root);
        let lock = root.join("held-lock");
        let ready = root.join("ready");
        let argv = held_arguments(&program, &lock, &ready);
        let session = isolated_session(Profile::parse("progressive-mp4").unwrap());
        let mut calls = 0;
        let mut ready_observed = false;
        let report = session
            .run_with(Some(root.as_os_str()), |runner, _, stop| {
                calls += 1;
                runner
                    .run_tool_injected_cleanup(
                        &argv,
                        &RunOptions {
                            timeout: Some(Duration::from_millis(100)),
                            stop: Some(stop),
                            ..RunOptions::default()
                        },
                        |_| {
                            ready_observed = wait_ready(&ready);
                            Err(std::io::Error::other("injected cleanup uncertainty"))
                        },
                    )
                    .map(|_| ())
                    .map_err(|error| {
                        assert_eq!(error.kind, "cleanup-unconfirmed");
                        "probe-cleanup-unconfirmed"
                    })
            })
            .unwrap();
        assert_eq!(calls, 1);
        assert!(ready_observed, "diagnostic fixture did not become ready");
        (session, report, lock)
    }

    #[test]
    fn unconfirmed_probe_keeps_the_actual_child_and_stops_later_probes_until_explicit_retry() {
        let root = tempfile::tempdir().unwrap();
        let (session, report, lock) = retained_probe(root.path());
        assert!(!report.ready);
        assert_eq!(report.cleanup_status, "unconfirmed");
        assert_eq!(
            report.tools[0].failure_code,
            Some("probe-cleanup-unconfirmed")
        );
        assert_eq!(report.tools[1].failure_code, Some("probe-not-started"));
        assert!(!session.cleanup_confirmed());
        session.retry_cleanup().unwrap();
        assert!(session.cleanup_confirmed());
        let released = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock)
            .unwrap();
        released.try_lock().unwrap();
        assert!(Arc::ptr_eq(&session.run().unwrap(), &report));
        assert_eq!(
            report.cleanup_status, "unconfirmed",
            "the original failed attempt stays truthful after cleanup"
        );
    }

    #[test]
    fn foreground_cleanup_driver_cannot_return_or_drop_a_still_live_probe() {
        let root = tempfile::tempdir().unwrap();
        let (session, report, lock) = retained_probe(root.path());
        let assert_held = || {
            let held = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&lock);
            #[cfg(unix)]
            assert!(matches!(
                held.unwrap().try_lock(),
                Err(std::fs::TryLockError::WouldBlock)
            ));
            #[cfg(windows)]
            // The native fixture opens with share_mode(0): ERROR_SHARING_VIOLATION.
            assert_eq!(held.unwrap_err().raw_os_error(), Some(32));
        };
        assert_held();
        let mut attempts = 0;
        let mut notices = 0;
        let mut pauses = 0;
        session.wait_for_cleanup_with(
            || {
                attempts += 1;
                if attempts <= 2 {
                    assert_held();
                    Err(std::io::Error::other("injected retry uncertainty"))
                } else {
                    session.retry_cleanup()
                }
            },
            || notices += 1,
            || {
                pauses += 1;
                assert!(session.stop.is_set());
                assert!(!session.cleanup_confirmed());
                assert_held();
            },
        );
        assert!(
            session.cleanup_confirmed(),
            "foreground driver returned with a live owner"
        );
        assert_eq!(attempts, 3);
        assert_eq!(notices, 1);
        assert_eq!(pauses, 2);
        let released = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock)
            .unwrap();
        released.try_lock().unwrap();
        assert!(!report.ready);
        assert_eq!(
            report.tools[0].failure_code,
            Some("probe-cleanup-unconfirmed")
        );
        assert_eq!(report.cleanup_status, "unconfirmed");
    }

    #[test]
    fn stop_during_an_actual_probe_drains_it_and_prevents_the_next_probe() {
        let root = tempfile::tempdir().unwrap();
        let program = fixtures(root.path());
        let lock = root.path().join("held-lock");
        let ready = root.path().join("ready");
        let argv = held_arguments(&program, &lock, &ready);
        let stop = crate::concurrency::Stop::new();
        let session = isolated_session(Profile::ALL).with_stop(stop.clone());
        let mut calls = 0;
        let report = std::thread::scope(|scope| {
            scope.spawn(|| {
                let ready_observed = wait_ready(&ready);
                let worker_active = !session.cleanup_confirmed();
                stop.set();
                assert!(
                    ready_observed && worker_active,
                    "diagnostic did not reach its active fixture"
                );
            });
            session
                .run_with(Some(root.path().as_os_str()), |runner, _, stop| {
                    calls += 1;
                    runner
                        .run_tool(
                            &argv,
                            "fixture",
                            &RunOptions {
                                timeout: Some(Duration::from_secs(3)),
                                stop: Some(stop),
                                ..RunOptions::default()
                            },
                        )
                        .map(|_| ())
                        .map_err(|error| {
                            assert_eq!(error.kind, "cancelled");
                            "probe-cancelled"
                        })
                })
                .unwrap()
        });
        assert_eq!(calls, 1);
        assert!(report.cancelled);
        assert!(!report.ready);
        assert_eq!(report.cleanup_status, "confirmed");
        assert!(session.cleanup_confirmed());
        let released = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock)
            .unwrap();
        released.try_lock().unwrap();
    }

    #[test]
    fn stop_before_start_never_executes_a_probe() {
        let root = tempfile::tempdir().unwrap();
        fixtures(root.path());
        let session = isolated_session(Profile::ALL);
        session.request_stop();
        let report = session
            .run_with(Some(root.path().as_os_str()), |_, _, _| {
                panic!("probe started after stop")
            })
            .unwrap();
        assert!(!report.ready);
        assert!(report.cancelled);
        assert!(session.cleanup_confirmed());
        session.retry_cleanup().unwrap();
    }
}
