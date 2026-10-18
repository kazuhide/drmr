//! Optional, command-local numeric measurements; no input text or paths are retained.
use crate::events::{Record, Sink};
use serde::Serialize;
use std::{
    fs,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Phase {
    InputPreparation,
    Inspection,
    Normalization,
    Packaging,
    Verification,
    Publication,
}
const PHASES: [Phase; 6] = [
    Phase::InputPreparation,
    Phase::Inspection,
    Phase::Normalization,
    Phase::Packaging,
    Phase::Verification,
    Phase::Publication,
];
const TOOLS: [&str; 3] = ["ffmpeg", "ffprobe", "packager"];

/// Sums of observed scopes, not CPU time, wall time, or a critical path.
#[derive(Clone, Debug, Serialize)]
pub struct PhaseMeasurement {
    phase: Phase,
    status: &'static str,
    scopes: u64,
    scope_elapsed_sum_seconds: f64,
}
/// Call durations include admission, capture/parser work, and confirmed child cleanup.
#[derive(Clone, Debug, Serialize)]
pub struct ToolMeasurement {
    tool: &'static str,
    status: &'static str,
    calls: u64,
    unsuccessful_calls: u64,
    reported_duration_sum_seconds: f64,
}
/// Regular-file lengths at explicit, quiescent checkpoints of disjoint owned stages.
#[derive(Clone, Debug, Default, Serialize)]
pub struct WorkspaceMeasurement {
    status: &'static str,
    checkpoints: u64,
    incomplete_checkpoints: u64,
    checkpoint_regular_file_bytes_max: Option<u64>,
    reason: Option<&'static str>,
}
/// A missing metric is never represented as a measured zero.
#[derive(Clone, Debug, Serialize)]
pub struct MissingMeasurement {
    status: &'static str,
    reason: &'static str,
}
/// Measurements supplement the media proof and never establish its success.
#[derive(Clone, Debug, Serialize)]
pub struct MeasurementReport {
    schema_version: u32,
    status: &'static str,
    phase_semantics: &'static str,
    tool_semantics: &'static str,
    coverage: &'static str,
    phases: Vec<PhaseMeasurement>,
    tools: Vec<ToolMeasurement>,
    unclassified_tool_calls: u64,
    invalid_tool_records: u64,
    workspace: WorkspaceMeasurement,
    io: MissingMeasurement,
}
#[derive(Clone, Copy, Default)]
struct Aggregate {
    count: u64,
    unsuccessful: u64,
    seconds: f64,
}
#[derive(Default)]
struct State {
    phases: [Aggregate; 6],
    tools: [Aggregate; 3],
    unclassified: u64,
    invalid: u64,
    workspace: WorkspaceMeasurement,
}
#[derive(Default)]
pub(super) struct Measurements {
    state: Mutex<State>,
    failed: AtomicBool,
}
impl Measurements {
    fn update(&self, update: impl FnOnce(&mut State)) {
        match self.state.lock() {
            Ok(mut state) => update(&mut state),
            Err(_) => {
                self.failed.store(true, Ordering::Relaxed);
            }
        }
    }
    pub fn scope(self: &Arc<Self>, phase: Phase) -> Scope {
        Scope(Some((Arc::clone(self), phase, Instant::now())))
    }
    fn elapsed(&self, phase: Phase, elapsed: Duration) {
        self.update(|state| {
            let entry = &mut state.phases[phase as usize];
            entry.count = entry.count.saturating_add(1);
            entry.seconds += elapsed.as_secs_f64();
        });
    }
    pub fn report(&self) -> MeasurementReport {
        let state = self.state.lock().ok();
        let empty = State::default();
        let state = state.as_deref().unwrap_or(&empty);
        let failed = self.failed.load(Ordering::Relaxed) || self.state.is_poisoned();
        let mut workspace = state.workspace.clone();
        if workspace.status.is_empty() {
            workspace.status = "unmeasured";
            workspace.reason = Some("no-owned-workspace-checkpoints");
        }
        MeasurementReport {
            schema_version: 1,
            status: if failed {
                "unmeasured"
            } else if state.invalid > 0 || state.unclassified > 0 {
                "partial"
            } else {
                "observed"
            },
            phase_semantics: "elapsed-scope-sums-overlap-not-command-wall-or-cpu",
            tool_semantics: "generic-call-duration-sums-include-admission-capture-and-cleanup",
            coverage: "explicit-generic-scopes-only-not-all-rust-or-filesystem-work",
            phases: PHASES
                .into_iter()
                .enumerate()
                .map(|(index, phase)| PhaseMeasurement {
                    phase,
                    status: if state.phases[index].count == 0 {
                        "unmeasured"
                    } else {
                        "observed-scopes"
                    },
                    scopes: state.phases[index].count,
                    scope_elapsed_sum_seconds: state.phases[index].seconds,
                })
                .collect(),
            tools: TOOLS
                .into_iter()
                .enumerate()
                .map(|(index, tool)| ToolMeasurement {
                    tool,
                    status: if state.tools[index].count == 0 {
                        "unobserved"
                    } else {
                        "observed-calls"
                    },
                    calls: state.tools[index].count,
                    unsuccessful_calls: state.tools[index].unsuccessful,
                    reported_duration_sum_seconds: state.tools[index].seconds,
                })
                .collect(),
            unclassified_tool_calls: state.unclassified,
            invalid_tool_records: state.invalid,
            workspace,
            io: MissingMeasurement {
                status: "unmeasured",
                reason: "total-process-io-not-collected",
            },
        }
    }
    pub fn checkpoint(&self, roots: &[&super::files::Stage]) {
        let paths: Vec<_> = roots.iter().map(|stage| stage.path()).collect();
        let observed = walk(&paths, 100_000, 64, Duration::from_millis(100));
        self.update(|state| {
            let value = &mut state.workspace;
            match observed {
                Ok(bytes) => {
                    value.checkpoints = value.checkpoints.saturating_add(1);
                    value.checkpoint_regular_file_bytes_max = Some(
                        value
                            .checkpoint_regular_file_bytes_max
                            .unwrap_or(0)
                            .max(bytes),
                    );
                }
                Err(reason) => {
                    value.incomplete_checkpoints = value.incomplete_checkpoints.saturating_add(1);
                    value.reason = Some(reason);
                }
            }
            value.status = if value.incomplete_checkpoints > 0 {
                "partial"
            } else {
                "observed-checkpoints"
            };
        });
    }
}
impl Sink for Measurements {
    fn emit(&self, record: &Record) {
        if record.get("event").and_then(serde_json::Value::as_str) != Some("tool-output") {
            return;
        }
        let Some(duration) = record
            .get("duration_s")
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
        else {
            self.update(|state| state.invalid = state.invalid.saturating_add(1));
            return;
        };
        let tool = record.get("tool").and_then(serde_json::Value::as_str);
        let unsuccessful = record.get("returncode").and_then(serde_json::Value::as_i64) != Some(0);
        self.update(|state| {
            if let Some(index) = TOOLS.iter().position(|name| Some(*name) == tool) {
                let entry = &mut state.tools[index];
                let next = entry.seconds + duration;
                if !next.is_finite() {
                    state.invalid = state.invalid.saturating_add(1);
                    return;
                }
                entry.count = entry.count.saturating_add(1);
                entry.unsuccessful = entry.unsuccessful.saturating_add(u64::from(unsuccessful));
                entry.seconds = next;
            } else {
                state.unclassified = state.unclassified.saturating_add(1);
            }
        });
    }
}
#[derive(Default)]
pub(super) struct Scope(Option<(Arc<Measurements>, Phase, Instant)>);
impl Drop for Scope {
    fn drop(&mut self) {
        if let Some((measurement, phase, started)) = self.0.take() {
            measurement.elapsed(phase, started.elapsed());
        }
    }
}

fn reparse(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
fn walk(
    roots: &[&Path],
    entry_limit: usize,
    depth_limit: usize,
    budget: Duration,
) -> Result<u64, &'static str> {
    if roots.is_empty() {
        return Err("no-owned-workspace-checkpoints");
    }
    for (index, root) in roots.iter().enumerate() {
        if roots[index + 1..]
            .iter()
            .any(|other| root.starts_with(other) || other.starts_with(root))
        {
            return Err("overlapping-owned-roots");
        }
    }
    let started = Instant::now();
    let mut pending: Vec<_> = roots.iter().map(|root| (root.to_path_buf(), 0)).collect();
    let mut entries = 0usize;
    let mut bytes = 0u64;
    while let Some((path, depth)) = pending.pop() {
        if started.elapsed() >= budget {
            return Err("checkpoint-time-budget");
        }
        if depth > depth_limit {
            return Err("checkpoint-depth-limit");
        }
        entries = entries.checked_add(1).ok_or("checkpoint-entry-limit")?;
        if entries > entry_limit {
            return Err("checkpoint-entry-limit");
        }
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| "checkpoint-metadata-unavailable")?;
        if reparse(&metadata) {
            return Err("checkpoint-reparse-or-symlink");
        }
        if metadata.is_file() {
            bytes = bytes
                .checked_add(metadata.len())
                .ok_or("checkpoint-size-overflow")?;
        } else if metadata.is_dir() {
            for entry in fs::read_dir(&path).map_err(|_| "checkpoint-directory-unavailable")? {
                if pending.len() >= entry_limit.saturating_sub(entries) {
                    return Err("checkpoint-entry-limit");
                }
                if started.elapsed() >= budget {
                    return Err("checkpoint-time-budget");
                }
                pending.push((
                    entry
                        .map_err(|_| "checkpoint-directory-unavailable")?
                        .path(),
                    depth + 1,
                ));
            }
        } else {
            return Err("checkpoint-nonregular-entry");
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durations_are_numeric_allowlisted_sums_and_parallel_scopes_are_not_wall_time() {
        let measurement = Measurements::default();
        let record = serde_json::json!({"event":"tool-output", "tool":"ffmpeg", "duration_s":1.25, "returncode":0,
            "argv":["secret-key-and-path"],"stderr":"secret-key-and-path"}).as_object().unwrap().clone();
        measurement.emit(&record);
        measurement.emit(&record);
        measurement.elapsed(Phase::Verification, Duration::from_secs(3));
        measurement.elapsed(Phase::Verification, Duration::from_secs(3));
        let encoded = serde_json::to_value(measurement.report()).unwrap();
        assert_eq!(encoded["tools"][0]["calls"], 2);
        assert_eq!(encoded["tools"][0]["reported_duration_sum_seconds"], 2.5);
        assert_eq!(encoded["phases"][4]["scope_elapsed_sum_seconds"], 6.0);
        assert!(!encoded.to_string().contains("secret-key-and-path"));
        assert_eq!(encoded["workspace"]["status"], "unmeasured");
    }
    #[test]
    fn checkpoints_count_only_regular_lengths_and_refuse_overlap_and_limits() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("sample"), [0u8; 17]).unwrap();
        let other = tempfile::tempdir().unwrap();
        fs::write(other.path().join("sample"), [0u8; 23]).unwrap();
        assert_eq!(
            walk(&[root.path(), other.path()], 100, 5, Duration::from_secs(1)),
            Ok(40)
        );
        assert_eq!(
            walk(&[root.path(), root.path()], 100, 5, Duration::from_secs(1)),
            Err("overlapping-owned-roots")
        );
        assert_eq!(
            walk(&[root.path()], 1, 5, Duration::from_secs(1)),
            Err("checkpoint-entry-limit")
        );
        assert_eq!(
            walk(&[root.path()], 100, 0, Duration::from_secs(1)),
            Err("checkpoint-depth-limit")
        );
        assert_eq!(
            walk(&[root.path()], 100, 5, Duration::ZERO),
            Err("checkpoint-time-budget")
        );
    }
    #[cfg(unix)]
    #[test]
    fn checkpoint_never_follows_an_outside_symlink() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("sentinel"), b"unchanged").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
        assert_eq!(
            walk(&[root.path()], 100, 5, Duration::from_secs(1)),
            Err("checkpoint-reparse-or-symlink")
        );
        assert_eq!(
            fs::read(outside.path().join("sentinel")).unwrap(),
            b"unchanged"
        );
    }
    #[test]
    fn reused_and_concurrent_converters_isolate_observers_and_preserve_the_original_sink() {
        let original = Arc::new(crate::events::Collector::default());
        let converter = super::super::Converter::new(
            Arc::new(crate::toolrun::ToolRunner::new(
                Arc::new(crate::toolrun::Secrets::new()),
                original.clone(),
            )),
            "packager",
            "ffmpeg",
        );
        std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                converter.measure(|command| {
                    command
                        .backend
                        .runner
                        .note_result("ffmpeg", Some(0), 2.0, None, "", None);
                    Ok(1)
                })
            });
            let b = scope.spawn(|| {
                converter.measure(|command| {
                    command
                        .backend
                        .runner
                        .note_result("ffprobe", Some(0), 3.0, None, "", None);
                    Err::<(), _>(super::super::ConversionError("cancelled"))
                })
            });
            let (result, report) = a.join().unwrap();
            assert_eq!(result, Ok(1));
            assert_eq!(report.tools[0].calls, 1);
            assert_eq!(report.tools[1].calls, 0);
            let (result, report) = b.join().unwrap();
            assert_eq!(result, Err(super::super::ConversionError("cancelled")));
            assert_eq!(report.tools[0].calls, 0);
            assert_eq!(report.tools[1].calls, 1);
        });
        assert_eq!(original.records().len(), 2);
        let (_, report) = converter.measure(|_| Ok(()));
        assert!(report.tools.iter().all(|tool| tool.calls == 0));
    }
    #[test]
    fn invalid_and_unknown_events_do_not_turn_into_complete_or_nonfinite_measurements() {
        let measurement = Measurements::default();
        for (tool, duration) in [
            ("ffmpeg", -1.0),
            ("ffmpeg", f64::MAX),
            ("ffmpeg", f64::MAX),
            ("operator-path-secret", 2.0),
        ] {
            measurement.emit(serde_json::json!({"event":"tool-output", "tool":tool, "duration_s":duration, "returncode":0}).as_object().unwrap());
        }
        let report = measurement.report();
        assert_eq!(report.status, "partial");
        assert_eq!(report.invalid_tool_records, 2);
        assert_eq!(report.unclassified_tool_calls, 1);
        assert!(
            report
                .tools
                .iter()
                .all(|tool| tool.reported_duration_sum_seconds.is_finite())
        );
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("operator-path-secret")
        );
    }
    #[test]
    fn poisoned_telemetry_does_not_replace_the_operations_result() {
        let converter = super::super::Converter::new(
            Arc::new(crate::toolrun::ToolRunner::new(
                Arc::new(crate::toolrun::Secrets::new()),
                Arc::new(crate::events::NoSink),
            )),
            "packager",
            "ffmpeg",
        );
        let (result, report) = converter.measure(|command| {
            let meter = command.backend.measurement.as_ref().unwrap();
            let _caught = std::panic::catch_unwind(|| {
                let _held = meter.state.lock().unwrap();
                panic!("test-only poisoned measurement lock");
            });
            drop(command.backend.scope(Phase::Verification));
            Ok("media-result-unchanged")
        });
        assert_eq!(result, Ok("media-result-unchanged"));
        assert_eq!(report.status, "unmeasured");
    }
}
