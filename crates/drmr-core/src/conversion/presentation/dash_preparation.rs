//! DASH metadata preparation follows complete, budgeted representation capture.
use super::{
    ConversionError, LocalTrack, Path, Prepared, SourceValidationPurpose, TrackKind,
    VerificationSource, bind_audio_presentation, manifested_clock, metadata, prepare_for,
};
use crate::conversion::{backend::Backend, execution::Scheduler};

pub(super) fn prepare(
    input: &VerificationSource<'_>,
    stage: &Path,
    backend: &Backend,
    maximum_workers: Option<usize>,
    purpose: SourceValidationPurpose,
) -> Result<Vec<Prepared>, ConversionError> {
    let mut backend = backend.clone();
    backend.runner = std::sync::Arc::new(
        backend
            .runner
            .cohort(maximum_workers.unwrap_or(crate::concurrency::DEFAULT_WORKERS_CAP)),
    );
    backend.check_cancelled()?;
    let progress = backend.preparation();
    let tracks = if progress.is_none() {
        super::super::dash::snapshot_dash(input.input, stage)?
    } else {
        super::super::dash::snapshot_dash_observed(input.input, stage, &mut |done, total| {
            if done == 0
                && let Some(progress) = progress
            {
                progress.begin(total, super::super::media_progress::Unit::Renditions);
            }
            backend.check_cancelled()
        })?
    };
    let descriptions = metadata::read_dash(&stage.join("dash-manifest.mpd"))?;
    let scheduler = Scheduler::new(&backend, maximum_workers);
    collect(&scheduler, &tracks, &descriptions, |ordinal| {
        prepare_track(
            ordinal,
            &tracks[ordinal],
            descriptions[ordinal].clone(),
            input,
            stage,
            &backend,
            purpose,
        )
    })
}

fn collect<T: Send>(
    scheduler: &Scheduler<'_>,
    tracks: &[LocalTrack],
    descriptions: &[metadata::Metadata],
    unit: impl Fn(usize) -> Result<T, ConversionError> + Sync,
) -> Result<Vec<T>, ConversionError> {
    if tracks.len() != descriptions.len() {
        return Err(ConversionError("presentation-track-count-mismatch"));
    }
    scheduler.run(
        tracks.len(),
        tracks.len(),
        retained_metadata(tracks, descriptions)?,
        unit,
    )
}

fn retained_metadata(
    tracks: &[LocalTrack],
    descriptions: &[metadata::Metadata],
) -> Result<u64, ConversionError> {
    let bad = ConversionError("resource-budget");
    let mut bytes = std::mem::size_of_val(tracks)
        .checked_add(std::mem::size_of_val(descriptions))
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or(bad)?;
    for (track, description) in tracks.iter().zip(descriptions) {
        // A prepared result retains both original and media path buffers.
        bytes = bytes
            .checked_add(track.source.capacity().checked_mul(3).ok_or(bad)?)
            .ok_or(bad)?;
        let audio = description.audio.as_ref();
        for capacity in [
            description.audio_group.as_ref().map_or(0, String::capacity),
            audio.map_or(0, |audio| audio.group.capacity()),
            audio
                .and_then(|audio| audio.name.as_ref())
                .map_or(0, String::capacity),
            audio.map_or(0, |audio| audio.language.capacity()),
            audio
                .and_then(|audio| audio.channels.as_ref())
                .map_or(0, String::capacity),
        ] {
            // Input descriptions and prepared results each retain their own strings.
            bytes = bytes
                .checked_add(capacity.checked_mul(2).ok_or(bad)?)
                .ok_or(bad)?;
        }
    }
    u64::try_from(bytes).map_err(|_| bad)
}

fn prepare_track(
    ordinal: usize,
    track: &LocalTrack,
    metadata: metadata::Metadata,
    input: &VerificationSource<'_>,
    stage: &Path,
    backend: &Backend,
    purpose: SourceValidationPurpose,
) -> Result<Prepared, ConversionError> {
    let progress = backend.preparation();
    let task = progress.and_then(|progress| progress.start(ordinal));
    if let Some(task) = &task {
        task.describe(track.kind, metadata.resolution);
    }
    let directory = stage.join(format!("dash-metadata-{ordinal:04}"));
    let mut prepared = {
        let claim = crate::resources::Claim::for_workload(
            backend.runner.registered_workload().unwrap_or_default(),
            crate::toolrun::DEFAULT_STDOUT_BYTES,
            crate::toolrun::DEFAULT_STDERR_BYTES,
        );
        let _permit = backend
            .runner
            .native_permit(claim, &backend.stop, backend.timeout)
            .map_err(|error| match error {
                crate::resources::ReservationError::Cancelled => ConversionError("cancelled"),
                crate::resources::ReservationError::Insufficient => {
                    ConversionError("resource-budget")
                }
                crate::resources::ReservationError::Timeout => {
                    ConversionError("media-tool-timeout")
                }
            })?;
        let mut prepared = prepare_for(
            &track.source,
            track.index,
            track.kind,
            input.input_keys,
            purpose,
        )?;
        prepared.clock = manifested_clock(&prepared.media, backend)?;
        super::super::files::create_dir(&directory)?;
        prepared
    };
    // Audio binding and its evidence readers reserve their own CPU/memory capacity.
    bind_audio_presentation(&mut prepared, backend, input.input_keys, &directory)?;
    prepared.metadata = metadata;
    if prepared.metadata.audio.is_some() && track.kind != TrackKind::Audio {
        return Err(ConversionError("track-role-mismatch"));
    }
    prepared.dash_reference = true;
    backend.check_cancelled()?;
    if let Some(progress) = progress {
        progress.completed(ordinal);
    }
    if let Some(task) = task {
        task.finish();
    }
    Ok(prepared)
}

#[cfg(test)]
mod tests;
