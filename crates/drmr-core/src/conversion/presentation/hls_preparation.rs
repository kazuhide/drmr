//! Independent rendition preparation; segment/clock validation stays ordered per rendition.
use super::{
    ConversionError, MediaPath, Path, Prepared, Protection, SourceCompatibilityNote,
    SourceProtection, SourceValidationPurpose, TrackKind, VerificationSource, apply_hls_target,
    apply_transport_metadata, bind_audio_presentation, hls, hls_media_paths, inspection, key_for,
    manifested_clock, metadata, native_preparation, prepare_for, prepare_native_hls_transport,
    prepare_transport, validate_transport_roles,
};

struct Preparation<'a, 'keys> {
    input: &'a VerificationSource<'keys>,
    stage: &'a Path,
    backend: &'a super::super::backend::Backend,
    maximum_workers: Option<usize>,
    purpose: SourceValidationPurpose,
    normalize: bool,
    renditions: usize,
}

pub(super) fn prepare(
    input: &VerificationSource<'_>,
    stage: &Path,
    backend: &super::super::backend::Backend,
    maximum_workers: Option<usize>,
    purpose: SourceValidationPurpose,
    normalize: bool,
) -> Result<Vec<Prepared>, ConversionError> {
    let backend = preparation_backend(backend, maximum_workers);
    let paths = hls_media_paths(input.input)?;
    let progress = backend.preparation();
    if let Some(progress) = progress {
        progress.begin(paths.len(), super::super::media_progress::Unit::Renditions);
    }
    backend.check_cancelled()?;
    let preparation = Preparation {
        input,
        stage,
        backend: &backend,
        maximum_workers,
        purpose,
        normalize,
        renditions: paths.len(),
    };
    let scheduler = super::super::execution::Scheduler::new(&backend, maximum_workers);
    let tracks = collect(&scheduler, &paths, |ordinal| {
        let tracks = preparation.rendition(ordinal, &paths[ordinal])?;
        if let Some(progress) = progress {
            progress.completed(ordinal);
        }
        backend.check_cancelled()?;
        Ok(tracks)
    })?;
    if tracks.len() > 64
        || tracks
            .iter()
            .filter(|p| p.report.tracks[0].kind == TrackKind::Audio)
            .count()
            > 1
        || (tracks.iter().any(|track| track.transport_index.is_some())
            && tracks.iter().any(|track| track.transport_index.is_none()))
    {
        return Err(ConversionError("ts-presentation-layout-not-supported"));
    }
    if let Some(mut workload) = backend.runner.registered_workload() {
        workload.tracks = workload.tracks.max(tracks.len());
        backend.runner.register_workload(workload);
    }
    Ok(tracks)
}

fn preparation_backend(
    backend: &super::super::backend::Backend,
    maximum_workers: Option<usize>,
) -> super::super::backend::Backend {
    let mut scoped = backend.clone();
    // Every rendition and its nested segment probes share this command's cap.
    // An existing bundle cohort is inherited; other commands keep their capacity.
    scoped.runner = std::sync::Arc::new(
        backend
            .runner
            .cohort(maximum_workers.unwrap_or(crate::concurrency::DEFAULT_WORKERS_CAP)),
    );
    scoped
}

fn collect<T: Send>(
    scheduler: &super::super::execution::Scheduler<'_>,
    paths: &[MediaPath],
    unit: impl Fn(usize) -> Result<Vec<T>, ConversionError> + Sync,
) -> Result<Vec<T>, ConversionError> {
    let contexts = if paths.len() == 1 && paths[0].role.is_none() {
        2
    } else {
        paths.len()
    };
    scheduler
        .run(
            paths.len(),
            contexts,
            retained_metadata::<T>(paths, contexts)?,
            unit,
        )
        .map(|tracks| tracks.into_iter().flatten().collect())
}

fn retained_metadata<T>(paths: &[MediaPath], contexts: usize) -> Result<u64, ConversionError> {
    let bad = ConversionError("resource-budget");
    let mut bytes = std::mem::size_of_val(paths).checked_mul(2).ok_or(bad)?;
    // Both the per-rendition results and the final flattened Vec can retain capacity.
    bytes = bytes
        .checked_add(
            contexts
                .checked_mul(std::mem::size_of::<T>())
                .and_then(|n| n.checked_mul(4))
                .ok_or(bad)?,
        )
        .ok_or(bad)?;
    for media in paths {
        let rendition = media.metadata.audio.as_ref();
        for length in [
            media.path.capacity(),
            media
                .metadata
                .audio_group
                .as_ref()
                .map_or(0, String::capacity),
            rendition.map_or(0, |r| r.group.capacity()),
            rendition
                .and_then(|r| r.name.as_ref())
                .map_or(0, String::capacity),
            rendition.map_or(0, |r| r.language.capacity()),
            rendition
                .and_then(|r| r.channels.as_ref())
                .map_or(0, String::capacity),
        ] {
            bytes = bytes.checked_add(length).ok_or(bad)?;
        }
    }
    u64::try_from(bytes).map_err(|_| bad)
}

impl Preparation<'_, '_> {
    fn rendition(
        &self,
        ordinal: usize,
        media: &MediaPath,
    ) -> Result<Vec<Prepared>, ConversionError> {
        let Self {
            input,
            backend,
            maximum_workers,
            purpose,
            normalize,
            ..
        } = *self;
        let directory = self.stage.join(format!("track-{ordinal:04}"));
        super::super::files::create_dir(&directory)
            .map_err(|_| ConversionError("temporary-workspace-unavailable"))?;
        if let Some(prepared) = prepare_native_hls_transport(
            input,
            media,
            &directory,
            backend,
            normalize,
            self.renditions,
        )? {
            return Ok(prepared);
        }
        let localized = hls::localize(&media.path, &directory, input.hls_keys, backend)?;
        if let Some(admission) = &localized.transport {
            validate_transport_roles(media, admission, self.renditions)?;
            let mut prepared = if backend.validation_depth == super::super::ValidationDepth::Native
                && !normalize
            {
                native_preparation::transport(&localized, admission, backend)?
            } else {
                prepare_transport(
                    &localized,
                    admission,
                    &directory,
                    input.input_keys,
                    backend,
                    maximum_workers,
                )?
            };
            apply_transport_metadata(&mut prepared, media, &localized, backend, input.input_keys)?;
            apply_hls_target(&mut prepared, localized.target_seconds);
            return Ok(prepared);
        }
        let actual = inspection::inspect(&localized.combined)
            .map_err(|_| ConversionError("invalid-localized-hls-media"))?;
        media.validate_audio_variant(actual.tracks.iter().map(|track| track.kind))?;
        if actual.tracks.len() != 1 {
            return Err(ConversionError("hls-multiplexed-media-not-supported"));
        }
        if media.role.is_some_and(|kind| kind != actual.tracks[0].kind) {
            return Err(ConversionError("track-role-mismatch"));
        }
        let mut track = prepare_for(
            &localized.combined,
            0,
            actual.tracks[0].kind,
            input.input_keys,
            purpose,
        )?;
        track.clock = manifested_clock(&track.media, backend)?;
        bind_audio_presentation(&mut track, backend, input.input_keys, &directory)?;
        track.metadata = media.metadata.clone();
        track.hls_target_seconds = Some(localized.target_seconds);
        if actual.tracks[0].kind == TrackKind::Video {
            track.hls_video_segment_ms = localized.maximum_segment_ms;
        }
        if localized.empty_cue_pair {
            track
                .compatibility
                .push(SourceCompatibilityNote::HlsEmptyCuePair);
        }
        match localized.report.protection {
            Protection::HlsAes128 => track.outer_protection = SourceProtection::HlsAes128,
            Protection::Mixed => track.outer_protection = SourceProtection::Mixed,
            _ => {}
        }
        metadata::validate_profile(&track, backend, key_for(&track, input.input_keys))?;
        track.playlist_reference = if track.key_id.is_none() {
            Some(localized.playlist.clone())
        } else if actual.protection == Protection::Cbcs {
            Some(hls::sample_reference(
                &localized.playlist,
                key_for(&track, input.input_keys).ok_or(ConversionError("input-key-required"))?,
            )?)
        } else {
            None
        };
        track.source = if track.audio_presentation.is_some() {
            localized.combined
        } else {
            localized.reference
        };
        Ok(vec![track])
    }
}

#[cfg(test)]
mod cohort_tests;
#[cfg(test)]
mod media_tests;
#[cfg(test)]
mod tests;
