//! Exact packet-timing comparisons and fragment-duration measurements.
//!
//! Rational arithmetic preserves timestamp precision. Integer division is rounded
//! correctly when converting final measurements to floating point.

use super::python::{self, ratio};
use super::{MediaEngine, MediaError, PacketInfo};
use num_bigint::BigInt;
use num_rational::BigRational;
use num_traits::Signed as _;
use std::path::{Path, PathBuf};

/// `same_timing`'s default `tolerance`: `Fraction(1, 45000)` (two 90 kHz transport ticks).
pub fn default_timing_tolerance() -> BigRational {
    BigRational::new(BigInt::from(1), BigInt::from(45_000))
}

impl PacketInfo {
    /// `PacketInfo.start`: `Fraction(min(pts), timescale)` — a `ValueError` without packets, a
    /// `ZeroDivisionError` for a zero timescale.
    pub fn start(&self) -> Result<BigRational, MediaError> {
        let first = self
            .pts
            .iter()
            .min()
            .ok_or_else(|| MediaError::Value("min() arg is an empty sequence".to_string()))?;
        ratio(*first, self.timescale)
    }

    /// `PacketInfo.end`: `max(Fraction(p + d, timescale) for p, d in zip(pts, durations))`.
    pub fn end(&self) -> Result<BigRational, MediaError> {
        let mut end: Option<BigRational> = None;
        for (pts, duration) in self.pts.iter().zip(&self.durations) {
            let candidate = ratio(i128::from(*pts) + i128::from(*duration), self.timescale)?;
            if end.as_ref().is_none_or(|current| candidate > *current) {
                end = Some(candidate);
            }
        }
        end.ok_or_else(|| MediaError::Value("max() arg is an empty sequence".to_string()))
    }
}

/// Every `(a, b)` pair within `tolerance` of `b / scale_b - a / scale_a - shift`, lazily as
/// Python's `all` over a `zip` (the shorter list bounds it; the first pair decides first).
fn pairs_within(
    source: &[i64],
    source_scale: i64,
    output: &[i64],
    output_scale: i64,
    shift: &BigRational,
    tolerance: &BigRational,
) -> Result<bool, MediaError> {
    for (a, b) in source.iter().zip(output) {
        let difference = ratio(*b, output_scale)? - ratio(*a, source_scale)? - shift;
        if difference.abs() > *tolerance {
            return Ok(false);
        }
    }
    Ok(true)
}

/// `media::same_timing`: the same packet count, every pts and dts moved by exactly `shift`
/// and every duration unchanged, each within `tolerance` (default
/// [`default_timing_tolerance`]); compared exactly, in field insertion order (pts, then dts, then
/// durations, each stopping at the first pair outside). A zero timescale is Python's
/// `ZeroDivisionError`.
pub fn same_timing(
    source: &PacketInfo,
    output: &PacketInfo,
    shift: &BigRational,
    tolerance: Option<&BigRational>,
) -> Result<bool, MediaError> {
    if source.pts.len() != output.pts.len() {
        return Ok(false);
    }
    let default = default_timing_tolerance();
    let tolerance = tolerance.unwrap_or(&default);
    let zero = BigRational::from_integer(BigInt::from(0));
    Ok(pairs_within(
        &source.pts,
        source.timescale,
        &output.pts,
        output.timescale,
        shift,
        tolerance,
    )? && pairs_within(
        &source.dts,
        source.timescale,
        &output.dts,
        output.timescale,
        shift,
        tolerance,
    )? && pairs_within(
        &source.durations,
        source.timescale,
        &output.durations,
        output.timescale,
        &zero,
        tolerance,
    )?)
}

/// `media::measure_segment_durations_s` over any engine: every fragment's `tfdt` first
/// (in order; a fragment without one is the Python `RuntimeError`), then the differences over
/// `timescale` (`int / int`), then the last fragment's end from the engine's
/// `probe_segment_timing` of `init || last` in a temporary directory.
pub(super) fn measure_segment_durations_s<E: MediaEngine + ?Sized>(
    engine: &E,
    init_path: &Path,
    segment_paths: &[PathBuf],
    timescale: i64,
) -> Result<Vec<f64>, MediaError> {
    let Some(last) = segment_paths.last() else {
        return Ok(Vec::new());
    };
    let mut tfdts: Vec<BigInt> = Vec::with_capacity(segment_paths.len());
    for segment in segment_paths {
        let boundary = engine.read_fragment_boundary(segment)?.ok_or_else(|| {
            MediaError::Runtime(format!(
                "{} has no fragment (moof/tfdt/trun) metadata to measure",
                python::path_str(segment).unwrap_or_else(|_| segment.display().to_string())
            ))
        })?;
        tfdts.push(BigInt::from(python::int(&boundary.base_media_decode_time)?));
    }
    let scale = BigInt::from(timescale);
    let mut durations = Vec::with_capacity(segment_paths.len());
    for pair in tfdts.windows(2) {
        durations.push(python::true_divide(&(&pair[1] - &pair[0]), &scale)?);
    }
    let directory = tempfile::Builder::new().prefix("tmp").tempdir()?;
    #[cfg(unix)]
    let work = directory.path().to_path_buf();
    #[cfg(windows)]
    let work = {
        let path = directory.path().join("private");
        crate::native::create_private_dir_all(&path)?;
        path
    };
    let merged = work.join("tail.mp4");
    concat_bytes(&[init_path, last.as_path()], &merged)?;
    let end_s = engine.probe_segment_timing(&merged)?.duration_s;
    let last_start = python::true_divide(tfdts.last().expect("one tfdt per segment"), &scale)?;
    durations.push(end_s - last_start);
    Ok(durations)
}

/// `media::_concat_bytes`: the parts into `dest`, in order (no directory created).
fn concat_bytes(parts: &[&Path], dest: &Path) -> Result<(), MediaError> {
    let mut out = super::create_output_file(dest)?;
    for part in parts {
        let mut source = std::fs::File::open(part)?;
        std::io::copy(&mut source, &mut out)?;
    }
    Ok(())
}
