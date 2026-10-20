//! Conservative capacity estimates for media subprocesses, not operating-system RSS limits.

use crate::toolrun::{DEFAULT_STDERR_BYTES, DEFAULT_STDOUT_BYTES, MAX_CAPTURE_BYTES};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

mod limiter;
pub use limiter::{Claim, Limiter, Permit, ReservationError, process_limiter};

const MIB: u64 = 1024 * 1024;
const UNKNOWN_MEMORY_BYTES: u64 = 512 * MIB;
const MINIMUM_HEADROOM_BYTES: u64 = 256 * MIB;
const TRACK_RESIDENCY_BYTES: u64 = 8 * MIB;

/// Where the available-memory observation came from. No machine identifiers are collected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemorySource {
    /// Host available memory was the tighter readable limit.
    Host,
    /// This process's Linux control group, including its ancestors, was tighter.
    ProcessCgroup,
    /// No usable observation; a conservative fixed allowance is used.
    Unknown,
}

/// A point-in-time view of the resources this process may use.
#[derive(Clone, Copy, Debug)]
pub struct Hardware {
    /// CPU parallelism allowed by affinity and readable Linux CPU quotas, at least one.
    pub effective_cpus: usize,
    /// Available physical memory, limited by this process's control group when readable.
    pub available_memory_bytes: Option<u64>,
    /// The observation that supplied the effective memory value.
    pub memory_source: MemorySource,
}

impl Hardware {
    /// Query capacity only, without collecting other processes, arguments, or environment.
    pub fn detect() -> Self {
        let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let mut system = System::new();
        system.refresh_memory();
        let host = (system.total_memory() != 0).then(|| system.available_memory());
        let pid = Pid::from_u32(std::process::id());
        system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[pid]),
            true,
            ProcessRefreshKind::nothing().without_tasks(),
        );
        // System::cgroup_limits reads the root group. The current Process query resolves
        // /proc/<pid>/cgroup and includes tighter nested groups and ancestor headroom.
        let cgroup = system
            .process(pid)
            .and_then(sysinfo::Process::cgroup_limits)
            .map(|limits| limits.free_memory);
        Self::from_observations(cpus, host, cgroup)
    }

    /// Combine observations; a known zero is exhausted memory, never an unknown value.
    pub fn from_observations(cpus: usize, host: Option<u64>, cgroup: Option<u64>) -> Self {
        let (available_memory_bytes, memory_source) = match (host, cgroup) {
            (Some(host), Some(group)) if group <= host => {
                (Some(group), MemorySource::ProcessCgroup)
            }
            (Some(host), _) => (Some(host), MemorySource::Host),
            (None, Some(group)) => (Some(group), MemorySource::ProcessCgroup),
            (None, None) => (None, MemorySource::Unknown),
        };
        Self {
            effective_cpus: cpus.max(1),
            available_memory_bytes,
            memory_source,
        }
    }
}

/// Media dimensions and resource sizes already known from a source snapshot.
#[derive(Clone, Copy, Debug, Default)]
pub struct Workload {
    /// Largest video width times height; zero means audio or text only. Unknown assumes 4K.
    pub max_pixels: Option<u64>,
    /// Estimated input working set. Segment pipelines include their largest resource;
    /// streaming whole-track readers use a bounded window. Unknown assumes 32 MiB.
    pub input_buffer_bytes: Option<u64>,
    /// Tracks with retained metadata and intermediate state, including audio and text.
    pub tracks: usize,
}

impl Workload {
    /// Read resource sizes from a verified source snapshot. Dimensions come from admitted
    /// stream metadata; audio-only sources do not pay the video-frame estimate.
    pub fn from_analysis(analysis: &crate::source::SourceAnalysis) -> std::io::Result<Self> {
        let mut pixels = 0;
        let mut largest = 0;
        for track in &analysis.tracks {
            if track.content_type == "video" {
                let size = match (track.width, track.height) {
                    (Some(width), Some(height)) if width != 0 && height != 0 => {
                        width.saturating_mul(height)
                    }
                    _ => 3840 * 2160,
                };
                pixels = pixels.max(size);
            }
            for path in track
                .init_path
                .iter()
                .chain(track.segments.iter().map(|segment| &segment.path))
            {
                largest = largest.max(std::fs::metadata(path)?.len());
            }
        }
        Ok(Self {
            max_pixels: Some(pixels),
            input_buffer_bytes: Some(largest),
            tracks: analysis.tracks.len(),
        })
    }

    /// Combine already registered work conservatively. Missing dimensions and sizes use
    /// the same fallback as an estimate; a later call cannot lower an existing job charge.
    #[must_use]
    pub fn merge(self, other: Self) -> Self {
        Self {
            max_pixels: Some(
                self.max_pixels
                    .unwrap_or(3840 * 2160)
                    .max(other.max_pixels.unwrap_or(3840 * 2160)),
            ),
            input_buffer_bytes: Some(
                self.input_buffer_bytes
                    .unwrap_or(32 * MIB)
                    .max(other.input_buffer_bytes.unwrap_or(32 * MIB)),
            ),
            tracks: self.tracks.max(other.tracks),
        }
    }

    /// Estimated subprocess and capture working set. This is an admission estimate;
    /// third-party codecs and allocators can exceed it.
    pub fn estimated_child_bytes(self, stdout_bytes: usize, stderr_bytes: usize) -> u64 {
        let pixels = self.max_pixels.unwrap_or(3840 * 2160);
        let segment = self.input_buffer_bytes.unwrap_or(32 * MIB);
        let captured = u64::try_from(stdout_bytes.min(MAX_CAPTURE_BYTES))
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(stderr_bytes.min(MAX_CAPTURE_BYTES)).unwrap_or(u64::MAX));
        (64 * MIB)
            .saturating_add(pixels.saturating_mul(24))
            .saturating_add(segment.saturating_mul(2))
            .saturating_add(captured.saturating_mul(4))
    }
}

/// A recommendation with one CPU token per single-threaded media child.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    /// Recommended pool size; an explicit maximum is always an upper bound.
    pub workers: usize,
    /// Explicit `FFmpeg` decoder/encoder thread count per child.
    pub child_threads: usize,
    /// CPU tokens available across participating pools.
    pub cpu_tokens: usize,
    /// Memory tokens available after reserving operating-system and track headroom.
    pub memory_tokens_bytes: u64,
    /// Estimated tokens charged for each child.
    pub estimated_child_bytes: u64,
    /// Whether the memory observation is host, control-group, or unknown.
    pub memory_source: MemorySource,
}

/// Refusal to start media work with an invalid maximum or insufficient capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BudgetError {
    /// Public callers must supply the same positive, bounded maximum as the CLI.
    #[error("maximum workers must be between 1 and 64")]
    InvalidWorkers,
    /// Even one estimated child does not fit the available headroom.
    #[error("insufficient available memory for the estimated media workload")]
    Insufficient,
}

pub(crate) fn capacity(hardware: Hardware, tracks: usize) -> (usize, u64) {
    let cpus = hardware.effective_cpus.max(1);
    let cpu_tokens = if hardware.available_memory_bytes.is_none() {
        1
    } else {
        cpus.saturating_sub(cpus.div_ceil(4)).clamp(1, 64)
    };
    let memory_tokens_bytes = hardware
        .available_memory_bytes
        .map_or(UNKNOWN_MEMORY_BYTES, |available| {
            available.saturating_sub((available / 4).max(MINIMUM_HEADROOM_BYTES))
        })
        .saturating_sub(
            u64::try_from(tracks.max(1))
                .unwrap_or(u64::MAX)
                .saturating_mul(TRACK_RESIDENCY_BYTES),
        );
    (cpu_tokens, memory_tokens_bytes)
}

impl Budget {
    /// Select concurrency from hardware, actual media, and an optional user upper bound.
    /// Reserve a quarter of available memory (at least 256 MiB), track state, and a quarter
    /// of CPU parallelism when more than one CPU is available. Unknown memory allows one child.
    pub fn choose(
        hardware: Hardware,
        workload: Workload,
        maximum_workers: Option<usize>,
    ) -> Result<Self, BudgetError> {
        if maximum_workers.is_some_and(|maximum| maximum == 0 || maximum > 64) {
            return Err(BudgetError::InvalidWorkers);
        }
        let (cpu_tokens, memory_tokens_bytes) = capacity(hardware, workload.tracks);
        let estimated_child_bytes =
            workload.estimated_child_bytes(DEFAULT_STDOUT_BYTES, DEFAULT_STDERR_BYTES);
        let memory_workers =
            usize::try_from(memory_tokens_bytes / estimated_child_bytes).unwrap_or(usize::MAX);
        if memory_workers == 0 {
            return Err(BudgetError::Insufficient);
        }
        let workers = cpu_tokens
            .min(memory_workers)
            .min(maximum_workers.unwrap_or(32).clamp(1, 64));
        Ok(Self {
            workers: if hardware.available_memory_bytes.is_none() {
                1
            } else {
                workers
            },
            child_threads: 1,
            cpu_tokens,
            memory_tokens_bytes,
            estimated_child_bytes,
            memory_source: hardware.memory_source,
        })
    }
}
