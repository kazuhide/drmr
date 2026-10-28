//! Bounded track scheduling with the same process-wide media admission as other commands.
use super::{ConversionError, Converter};
use crate::concurrency::{Pool, Unit, UnitError, run_ordered};
use crate::resources::{Budget, Hardware, Workload};
use std::path::Path;

fn resource_budget(
    hardware: Hardware,
    workload: Workload,
    resources: usize,
    maximum: Option<usize>,
) -> Result<Budget, ConversionError> {
    let mut budget = Budget::choose(hardware, workload, maximum)
        .map_err(|_| ConversionError("resource-budget"))?;
    budget.workers = budget.workers.min(resources.max(1));
    Ok(budget)
}

fn retained_bytes<T>(units: usize, metadata: u64) -> Result<u64, ConversionError> {
    let bad = ConversionError("resource-budget");
    // A growing result Vec can retain twice its length. Each pending task also
    // owns queue/future/callback state; the bounded window caps that charge.
    let results = units
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or(bad)?;
    let pending = units
        .min(crate::concurrency::MAX_PENDING_TASKS)
        .checked_mul(8 * 1024)
        .ok_or(bad)?;
    metadata
        .checked_add(u64::try_from(results).map_err(|_| bad)?)
        .and_then(|bytes| bytes.checked_add(u64::try_from(pending).ok()?))
        .ok_or(bad)
}

fn scheduling_budget<T>(
    hardware: Hardware,
    workload: Workload,
    units: usize,
    metadata: u64,
    maximum: Option<usize>,
    capacity: (usize, u64),
) -> Result<Budget, ConversionError> {
    let mut budget = resource_budget(hardware, workload, units, maximum)?;
    budget.cpu_tokens = budget.cpu_tokens.min(capacity.0);
    budget.memory_tokens_bytes = budget
        .memory_tokens_bytes
        .min(capacity.1)
        .saturating_sub(retained_bytes::<T>(units, metadata)?);
    let memory_workers = usize::try_from(budget.memory_tokens_bytes / budget.estimated_child_bytes)
        .unwrap_or(usize::MAX);
    budget.workers = budget.workers.min(budget.cpu_tokens).min(memory_workers);
    if budget.workers == 0 {
        return Err(ConversionError("resource-budget"));
    }
    Ok(budget)
}

impl UnitError for ConversionError {
    fn cancelled() -> Self {
        Self("cancelled")
    }
    fn is_cancelled(&self) -> bool {
        self.0 == "cancelled"
    }
}
impl Converter {
    /// Set an optional upper bound; CPU and memory admission can choose fewer workers.
    pub fn with_max_workers(mut self, maximum: Option<usize>) -> Result<Self, ConversionError> {
        if maximum.is_some_and(|count| count == 0 || count > 64) {
            return Err(ConversionError("invalid-max-workers"));
        }
        self.maximum_workers = maximum;
        Ok(self)
    }
    pub(super) fn run_tracks<T: Send>(
        &self,
        tracks: usize,
        unit: impl Fn(usize) -> Result<T, ConversionError> + Sync,
    ) -> Result<Vec<T>, ConversionError> {
        self.run_units(tracks, tracks, 0, unit)
    }

    /// Unit count controls queueing, while only actual retained tracks are charged
    /// as track contexts. Caller-owned resource/result payloads have a byte charge.
    pub(super) fn run_units<T: Send>(
        &self,
        units: usize,
        retained_tracks: usize,
        metadata_bytes: u64,
        unit: impl Fn(usize) -> Result<T, ConversionError> + Sync,
    ) -> Result<Vec<T>, ConversionError> {
        Scheduler::new(&self.backend, self.maximum_workers).run(
            units,
            retained_tracks,
            metadata_bytes,
            unit,
        )
    }
}

pub(super) struct Scheduler<'a> {
    backend: &'a super::backend::Backend,
    maximum_workers: Option<usize>,
    hardware: Hardware,
    quiet: bool,
}
impl<'a> Scheduler<'a> {
    pub fn new(backend: &'a super::backend::Backend, maximum_workers: Option<usize>) -> Self {
        Self {
            backend,
            maximum_workers,
            #[cfg(test)]
            hardware: backend.scheduling_hardware.unwrap_or_else(Hardware::detect),
            #[cfg(not(test))]
            hardware: Hardware::detect(),
            quiet: false,
        }
    }
    #[cfg(test)]
    pub fn observed(
        backend: &'a super::backend::Backend,
        maximum_workers: Option<usize>,
        hardware: Hardware,
    ) -> Self {
        Self {
            backend,
            maximum_workers,
            hardware,
            quiet: false,
        }
    }
    pub fn without_observers(mut self) -> Self {
        self.quiet = true;
        self
    }
    pub fn run<T: Send>(
        &self,
        units: usize,
        retained_tracks: usize,
        metadata_bytes: u64,
        unit: impl Fn(usize) -> Result<T, ConversionError> + Sync,
    ) -> Result<Vec<T>, ConversionError> {
        self.backend.check_cancelled()?;
        if units == 0 {
            return Ok(Vec::new());
        }
        let known = self.backend.runner.registered_workload();
        let mut workload = known.unwrap_or_default();
        workload.tracks = workload.tracks.max(retained_tracks);
        if known.is_some() {
            self.backend.runner.register_workload(workload);
        }
        let budget = scheduling_budget::<T>(
            self.hardware,
            workload,
            units,
            metadata_bytes,
            self.maximum_workers,
            self.backend.runner.native_capacity(),
        )?;
        if !self.quiet {
            self.backend.runner.note_resource_budget(&budget);
        }
        let execute = |ordinal| {
            let result = unit(ordinal);
            if result.is_ok() && !self.quiet {
                self.backend.completed_work_unit(ordinal, units);
            }
            result
        };
        if budget.workers == 1 {
            return (0..units)
                .map(|ordinal| {
                    self.backend.check_cancelled()?;
                    execute(ordinal)
                })
                .collect();
        }
        std::thread::scope(|scope| {
            let pool = Pool::new(scope, budget.workers);
            let units = (0..units).map(|ordinal| {
                let unit = &execute;
                Box::new(move || {
                    self.backend.check_cancelled()?;
                    unit(ordinal)
                }) as Unit<'_, T, ConversionError>
            });
            // Keep ordinary failure propagation local to this invocation. An
            // external cancellation flag must not poison a later conversion.
            run_ordered(|unit| pool.submit(unit), units, None)
        })
    }
}

/// Whole-track files are streamed, so their total duration is not a resident-buffer charge.
/// Third-party codec allocations remain estimates rather than an operating-system limit.
pub(super) fn observe_profile(
    profile: &serde_json::Value,
    path: &Path,
) -> Result<Workload, ConversionError> {
    let size = path
        .metadata()
        .map_err(|_| ConversionError("input-unreadable"))?
        .len();
    Ok(profile_workload(profile, size))
}
fn profile_workload(profile: &serde_json::Value, size: u64) -> Workload {
    let pixels = if profile["codec_type"] == "audio" {
        0
    } else {
        profile["width"]
            .as_u64()
            .zip(profile["height"].as_u64())
            .filter(|(width, height)| *width > 0 && *height > 0)
            .map_or(3840 * 2160, |(width, height)| width.saturating_mul(height))
    };
    Workload {
        max_pixels: Some(pixels),
        input_buffer_bytes: Some(size.min(32 * 1024 * 1024)),
        tracks: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::MemorySource;
    use std::path::PathBuf;

    #[test]
    fn two_hundred_resources_do_not_become_two_hundred_resident_tracks() {
        let hardware = Hardware::from_observations(8, Some(2 * 1024 * 1024 * 1024), None);
        let workload = Workload {
            max_pixels: Some(1920 * 1080),
            input_buffer_bytes: Some(64 * 1024),
            tracks: 1,
        };
        for maximum in [Some(1), Some(2), Some(4), None] {
            let short = resource_budget(hardware, workload, 5, maximum).unwrap();
            let long = resource_budget(hardware, workload, 200, maximum).unwrap();
            assert_eq!(short.memory_tokens_bytes, long.memory_tokens_bytes);
            assert!(long.workers > 0 && long.workers <= maximum.unwrap_or(32));
        }
    }

    #[test]
    fn resource_results_and_pending_state_still_reduce_available_memory() {
        let hardware = Hardware::from_observations(8, Some(2 * 1024 * 1024 * 1024), None);
        let workload = Workload {
            max_pixels: Some(1920 * 1080),
            input_buffer_bytes: Some(64 * 1024),
            tracks: 1,
        };
        for units in [5, 200, 2000] {
            let base = resource_budget(hardware, workload, units, Some(4)).unwrap();
            let budget = scheduling_budget::<PathBuf>(
                hardware,
                workload,
                units,
                4096,
                Some(4),
                (4, u64::MAX),
            )
            .unwrap();
            assert_eq!(
                base.memory_tokens_bytes - budget.memory_tokens_bytes,
                retained_bytes::<PathBuf>(units, 4096).unwrap()
            );
            assert!(budget.workers > 0 && budget.workers <= 4);
        }
        assert!(retained_bytes::<u64>(usize::MAX, 0).is_err());
        assert!(retained_bytes::<u8>(1, u64::MAX).is_err());
        assert!(
            scheduling_budget::<u64>(hardware, workload, 200, u64::MAX, Some(1), (1, u64::MAX))
                .is_err()
        );
        assert!(scheduling_budget::<u64>(hardware, workload, 200, 0, Some(1), (1, 1)).is_err());
    }

    #[test]
    fn resource_scheduling_never_registers_its_unit_count_as_tracks() {
        let converter = converter(crate::concurrency::Stop::new());
        converter.backend.runner.register_workload(Workload {
            max_pixels: Some(0),
            input_buffer_bytes: Some(64 * 1024),
            tracks: 1,
        });
        let results = converter.run_units(200, 1, 1024, Ok).unwrap();
        assert_eq!(results, (0..200).collect::<Vec<_>>());
        assert_eq!(
            converter
                .backend
                .runner
                .registered_workload()
                .unwrap()
                .tracks,
            1
        );
        converter.run_tracks(2, |_| Ok(())).unwrap();
        assert_eq!(
            converter
                .backend
                .runner
                .registered_workload()
                .unwrap()
                .tracks,
            2
        );
    }
    fn converter(stop: crate::concurrency::Stop) -> Converter {
        Converter::new(
            std::sync::Arc::new(crate::toolrun::ToolRunner::new(
                std::sync::Arc::new(crate::toolrun::Secrets::new()),
                std::sync::Arc::new(crate::events::NoSink),
            )),
            "packager",
            "ffmpeg",
        )
        .with_stop(stop)
        .with_max_workers(Some(1))
        .unwrap()
    }
    #[test]
    fn cancellation_prevents_new_work_and_both_publication_forms() {
        let stop = crate::concurrency::Stop::new();
        let converter = converter(stop.clone());
        let stage = super::super::workspace().unwrap();
        stop.set();
        assert_eq!(
            converter.run_tracks(1, |_| Ok(())).unwrap_err(),
            ConversionError("cancelled")
        );
        assert_eq!(
            converter
                .backend
                .run(&["missing".into()], "ffprobe", &[])
                .unwrap_err(),
            ConversionError("cancelled")
        );
        assert_eq!(
            converter
                .publish(&stage, Path::new("missing"), Path::new("unused"))
                .unwrap_err(),
            ConversionError("cancelled")
        );
        assert_eq!(
            converter
                .publish_directory(&stage, Path::new("unused"))
                .unwrap_err(),
            ConversionError("cancelled")
        );
    }
    #[test]
    fn inline_scheduling_observes_cancellation_before_the_next_track() {
        let stop = crate::concurrency::Stop::new();
        let converter = converter(stop.clone());
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let result = converter.run_tracks(3, |_| {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            stop.set();
            Ok(())
        });
        assert_eq!(result.unwrap_err(), ConversionError("cancelled"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
    #[test]
    fn a_failed_track_does_not_set_the_callers_cancellation_flag() {
        let stop = crate::concurrency::Stop::new();
        let converter = converter(stop.clone()).with_max_workers(Some(2)).unwrap();
        assert_eq!(
            converter
                .run_tracks(2, |_| Err::<(), _>(ConversionError("synthetic-failure")))
                .unwrap_err(),
            ConversionError("synthetic-failure")
        );
        assert!(!stop.is_set());
        assert_eq!(converter.run_tracks(1, |_| Ok(7)).unwrap(), [7]);
    }
    #[test]
    fn a_long_streaming_track_does_not_require_its_whole_file_to_fit_in_memory() {
        let profile = serde_json::json!({"codec_type":"video", "width":1920, "height":1080});
        let workload = profile_workload(&profile, 16 * 1024 * 1024 * 1024);
        let hardware = Hardware {
            effective_cpus: 8,
            available_memory_bytes: Some(2 * 1024 * 1024 * 1024),
            memory_source: MemorySource::Host,
        };
        assert!(Budget::choose(hardware, workload, None).is_ok());
        let high_resolution = profile_workload(
            &serde_json::json!({"codec_type":"video","width":8192,"height":8192}),
            1024,
        );
        assert!(Budget::choose(hardware, high_resolution, None).is_err());
    }
}
