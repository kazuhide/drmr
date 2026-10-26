//! Temporary audio clock projection for resegmentation; final media retains the original clocks.
use super::{ConversionError, files};
use crate::{
    concurrency::Stop,
    inspection::{OutputFormat, TrackKind},
};
use std::path::{Path, PathBuf};
mod manifest;
mod metadata;
mod owned;

const BAD: ConversionError = ConversionError("packaging-clock-not-supported");
const CHANGED: ConversionError = ConversionError("packaging-clock-source-changed");
const LIMIT: ConversionError = ConversionError("packaging-clock-resource-limit");
const OVERFLOW: ConversionError = ConversionError("packaging-clock-field-overflow");

struct Applied<'a> {
    file: owned::BoundFile<'a>,
    plan: metadata::Plan,
    current: [u8; 32],
    dirty: bool,
}
impl Applied<'_> {
    fn restore(&mut self, stop: &Stop) -> Result<(), ConversionError> {
        if self.dirty {
            let digest =
                metadata::apply(&mut self.file, &self.plan.patches, self.current, true, stop)?;
            if digest != self.plan.digest {
                return Err(CHANGED);
            }
            self.dirty = false;
        }
        Ok(())
    }
}

pub(super) struct RebasedAudio<'a> {
    inputs: Vec<Applied<'a>>,
    tracks: Vec<Option<metadata::Track>>,
    kinds: Vec<TrackKind>,
}

pub(super) fn prepare<'a>(
    owner: &'a files::Stage,
    clears: &[PathBuf],
    kinds: &[TrackKind],
    stop: &Stop,
) -> Result<RebasedAudio<'a>, ConversionError> {
    metadata::checkpoint(stop)?;
    if clears.is_empty() || clears.len() != kinds.len() || clears.len() > 64 {
        return Err(BAD);
    }
    let mut unique = std::collections::BTreeSet::new();
    let mut planned = Vec::new();
    let mut identities = Vec::new();
    let mut tracks = vec![None; clears.len()];
    let mut budget = metadata::Budget::default();
    for (ordinal, (path, kind)) in clears.iter().zip(kinds).enumerate() {
        metadata::checkpoint(stop)?;
        if !unique.insert(path) {
            return Err(BAD);
        }
        let file = owned::BoundFile::open(owner, path)?;
        if identities.contains(&file.identity()) {
            return Err(BAD);
        }
        identities.push(file.identity());
        if *kind == TrackKind::Audio {
            let plan = metadata::prepare(&file, &mut budget, stop)?;
            tracks[ordinal] = Some(plan.track);
            planned.push((file, plan));
        } else if *kind != TrackKind::Video {
            return Err(BAD);
        }
    }
    let mut result = RebasedAudio {
        inputs: Vec::new(),
        tracks,
        kinds: kinds.to_vec(),
    };
    for (mut file, plan) in planned {
        if plan.patches.is_empty() {
            continue;
        }
        let current = metadata::apply(&mut file, &plan.patches, plan.digest, false, stop)?;
        result.inputs.push(Applied {
            file,
            plan,
            current,
            dirty: true,
        });
    }
    Ok(result)
}

impl RebasedAudio<'_> {
    pub(super) fn restore(
        mut self,
        owner: &files::Stage,
        root: &Path,
        format: OutputFormat,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        metadata::checkpoint(stop)?;
        if self.inputs.is_empty() {
            return Ok(());
        }
        let mut outputs = manifest::outputs(owner, root, &self.tracks, &self.kinds, format, stop)?;
        let projection = if format == OutputFormat::Dash {
            Some(manifest::dash(owner, root, &self.tracks, stop)?)
        } else {
            None
        };
        for input in &mut self.inputs {
            input.restore(stop)?;
        }
        for output in &mut outputs {
            metadata::apply(
                &mut output.file,
                &output.plan.patches,
                output.plan.digest,
                false,
                stop,
            )?;
            output.file.suspend();
        }
        if let Some(projection) = projection {
            projection.finish(root, stop)?;
        }
        Ok(())
    }
}

impl Drop for RebasedAudio<'_> {
    fn drop(&mut self) {
        let stop = Stop::new();
        for input in self.inputs.iter_mut().rev() {
            let _ = input.restore(&stop);
        }
    }
}

#[cfg(test)]
mod tests;
