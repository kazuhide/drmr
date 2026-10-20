//! Bounded track scheduling with the same process-wide media admission as other commands.
use super::{ConversionError, Converter};
use crate::concurrency::{Pool, Unit, UnitError, run_ordered};
use crate::resources::{Budget, Hardware, Workload};
use std::path::Path;

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
        self.backend.check_cancelled()?;
        if tracks == 0 {
            return Ok(Vec::new());
        }
        let known = self.backend.runner.registered_workload();
        let mut workload = known.unwrap_or_default();
        workload.tracks = workload.tracks.max(tracks);
        if known.is_some() {
            self.backend.runner.register_workload(workload);
        }
        let mut budget = Budget::choose(Hardware::detect(), workload, self.maximum_workers)
            .map_err(|_| ConversionError("resource-budget"))?;
        budget.workers = budget.workers.min(tracks);
        self.backend.runner.note_resource_budget(&budget);
        let execute = |ordinal| {
            let result = unit(ordinal);
            if result.is_ok() {
                self.backend.completed_work_unit(ordinal, tracks);
            }
            result
        };
        if budget.workers == 1 {
            return (0..tracks)
                .map(|ordinal| {
                    self.backend.check_cancelled()?;
                    execute(ordinal)
                })
                .collect();
        }
        std::thread::scope(|scope| {
            let pool = Pool::new(scope, budget.workers);
            let units = (0..tracks).map(|ordinal| {
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
