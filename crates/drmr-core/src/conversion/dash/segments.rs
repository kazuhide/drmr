//! Private DASH resource snapshots and exact source timeline declarations.
use super::{
    ConversionError, MAX_BYTES, MAX_SEGMENTS, MAX_TRACKS, Protection, TrackKind, Window, error,
};
use crate::{concurrency::Stop, conversion::files, native::FileIdentity};
use std::{
    fs,
    io::{Seek, SeekFrom},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::conversion) struct SegmentBoundary {
    pub number: u64,
    pub time: u64,
    pub duration: u64,
    pub timescale: u64,
}

pub(in crate::conversion) struct SegmentedRepresentation {
    pub initialization: PathBuf,
    pub resources: Vec<(PathBuf, SegmentBoundary)>,
    /// Original PTO and actual presented first/end, each in its exact clock units.
    pub window: Window,
    pub kind: TrackKind,
    pub protection: Protection,
}

pub(in crate::conversion) struct Segmented {
    pub manifest: PathBuf,
    pub report: crate::inspection::Inspection,
    pub representations: Vec<SegmentedRepresentation>,
}

#[derive(Default)]
pub(super) struct Budget {
    bytes: u64,
    paths: usize,
    resources: usize,
}
impl Budget {
    pub(super) fn snapshot(
        &mut self,
        source: &Path,
        destination: &Path,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        self.snapshot_range(source, destination, None, stop)
    }
    pub(super) fn snapshot_range(
        &mut self,
        source: &Path,
        destination: &Path,
        range: Option<super::addressing::ByteRange>,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        checkpoint(Some(stop))?;
        self.resources = self
            .resources
            .checked_add(1)
            .filter(|n| *n <= MAX_SEGMENTS + MAX_TRACKS + 1)
            .ok_or(error("dash-segment-limit"))?;
        self.paths = self
            .paths
            .checked_add(source.as_os_str().len())
            .and_then(|n| n.checked_add(destination.as_os_str().len()))
            .filter(|n| *n <= 16 * 1024 * 1024)
            .ok_or(error("source-graph-limit"))?;
        let metadata = fs::symlink_metadata(source).map_err(|_| error("input-unreadable"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > MAX_BYTES {
            return Err(error("input-not-regular-or-too-large"));
        }
        let mut input = files::regular_input(source)?;
        let before = input.metadata().map_err(|_| error("media-copy-failed"))?;
        if !before.is_file() || before.len() > MAX_BYTES {
            return Err(error("input-not-regular-or-too-large"));
        }
        let identity = FileIdentity::read(&input).map_err(|_| error("media-copy-failed"))?;
        let mut output = files::create_new(destination)?;
        if let Some(range) = range {
            use std::io::Read as _;
            if range.end >= before.len() {
                return Err(error("dash-resource-range-outside-file"));
            }
            input
                .seek(SeekFrom::Start(range.start))
                .map_err(|_| error("media-copy-failed"))?;
            let length = range.length()?;
            let initial = self.bytes;
            self.copy_chunks(&mut (&mut input).take(length), &mut output, stop)?;
            if self.bytes - initial != length {
                return Err(error("media-copy-failed"));
            }
        } else {
            self.copy_chunks(&mut input, &mut output, stop)?;
        }
        let after = input.metadata().map_err(|_| error("media-copy-failed"))?;
        if before.len() != after.len()
            || before.modified().map_err(|_| error("media-copy-failed"))?
                != after.modified().map_err(|_| error("media-copy-failed"))?
            || FileIdentity::read(&files::regular_input(source)?)
                .map_err(|_| error("media-copy-failed"))?
                != identity
        {
            return Err(error("source-changed"));
        }
        Ok(())
    }
    fn copy_chunks(
        &mut self,
        input: &mut impl std::io::Read,
        output: &mut impl std::io::Write,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        let mut buffer = Zeroizing::new(vec![0; 64 * 1024]);
        loop {
            checkpoint(Some(stop))?;
            let n = input
                .read(&mut buffer)
                .map_err(|_| error("media-copy-failed"))?;
            if n == 0 {
                break;
            }
            let copied = self
                .bytes
                .checked_add(u64::try_from(n).map_err(|_| error("dash-media-size-limit"))?)
                .filter(|n| *n <= MAX_BYTES)
                .ok_or(error("dash-media-size-limit"))?;
            checkpoint(Some(stop))?;
            output
                .write_all(&buffer[..n])
                .map_err(|_| error("media-copy-failed"))?;
            self.bytes = copied;
        }
        checkpoint(Some(stop))
    }
}

pub(super) fn checkpoint(stop: Option<&Stop>) -> Result<(), ConversionError> {
    if stop.is_some_and(Stop::is_set) {
        Err(error("cancelled"))
    } else {
        Ok(())
    }
}

pub(super) fn check_clock(
    resolved: &super::addressing::Representation,
    segment: &super::addressing::Segment,
    clock: &mut crate::conversion::timing::FragmentClock,
    copied: &Path,
    ordinal: usize,
) -> Result<(u64, u64, u64), ConversionError> {
    let (declared, presented) = clock.dash_clocks(copied)?;
    if segment.resource.range.is_some() && !clock.relocatable() {
        return Err(error("dash-ranged-absolute-fragment-offset-not-supported"));
    }
    if resolved.index.is_some() && !clock.starts_with_sync() {
        return Err(error("dash-index-sap-mismatch"));
    }
    if let Some(nominal) = resolved.nominal {
        super::addressing::check_nominal(
            segment,
            nominal,
            resolved.offset,
            resolved.timescale,
            presented,
            ordinal,
            resolved.segments.len(),
        )?;
    } else {
        let observed = if resolved.index.is_some() {
            (
                presented.0,
                presented
                    .1
                    .checked_sub(presented.0)
                    .ok_or(error("dash-invalid-index"))?,
                presented.2,
            )
        } else {
            declared
        };
        if u128::from(observed.0) * u128::from(resolved.timescale)
            != u128::from(segment.time) * u128::from(observed.2)
            || u128::from(observed.1) * u128::from(resolved.timescale)
                != u128::from(segment.duration) * u128::from(observed.2)
        {
            return Err(error("manifest-fragment-timeline-mismatch"));
        }
    }
    Ok(declared)
}

#[cfg(test)]
mod tests;
