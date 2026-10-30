//! Original resource boundaries supplement complete, continuously qualified track evidence.
use super::media::{Media, Track, accumulator_with_clock};
use crate::conversion::{
    ConversionError, InputKeys,
    audio_presentation::{self, AudioClaim, AudioPresentation},
    backend::Backend,
    native_media::{self, SegmentEvidence, SegmentInput, SegmentReceipt},
};
use std::path::{Path, PathBuf};

const BAD: ConversionError = ConversionError("native-sample-evidence-invalid");
const INCOMPLETE: ConversionError = ConversionError("native-sample-evidence-incomplete");
const CHANGED: ConversionError = ConversionError("native-source-changed");
const MAX_BOUNDARY_BYTES: usize = 64 * 1024 * 1024;

pub(in crate::conversion) struct SegmentedMedia {
    pub media: Media,
    pub boundaries: Vec<SegmentEvidence>,
    pub audio_presentation: Option<AudioPresentation>,
}

struct Counted {
    receipts: Vec<SegmentReceipt>,
    totals: Vec<u64>,
}

fn check_movie(
    input: &SegmentInput,
    mut movie: native_media::Movie,
) -> Result<(), ConversionError> {
    for track in &mut movie.tracks {
        track.sample_count = 0;
    }
    if movie != *input.movie() {
        return Err(ConversionError("native-track-changed"));
    }
    Ok(())
}

fn boundary_budget(resources: usize, tracks: usize) -> Result<(), ConversionError> {
    let bytes = tracks
        .checked_mul(std::mem::size_of::<native_media::SegmentTrackEvidence>())
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<SegmentReceipt>()))
        .and_then(|bytes| resources.checked_mul(bytes));
    if bytes.is_none_or(|bytes| bytes > MAX_BOUNDARY_BYTES) {
        return Err(ConversionError("resource-budget"));
    }
    Ok(())
}

fn count_resources(
    input: &SegmentInput,
    resources: &[PathBuf],
    keys: &InputKeys,
    backend: &Backend,
) -> Result<Counted, ConversionError> {
    let tracks = &input.movie().tracks;
    if resources.is_empty()
        || tracks.is_empty()
        || tracks.iter().any(|track| track.sample_count != 0)
    {
        return Err(INCOMPLETE);
    }
    boundary_budget(resources.len(), tracks.len())?;
    let mut receipts = Vec::new();
    receipts
        .try_reserve_exact(resources.len())
        .map_err(|_| ConversionError("resource-budget"))?;
    let mut totals = vec![0_u64; tracks.len()];
    for resource in resources {
        backend.check_cancelled()?;
        let (movie, receipt) =
            input.scan_with_evidence(resource, keys, &backend.stop, &mut |_, _, _| Ok(()))?;
        for descriptor in &movie.tracks {
            let count = receipt
                .evidence
                .tracks
                .iter()
                .find(|part| part.index == descriptor.index)
                .map_or(0, |part| part.sample_count);
            if count != descriptor.sample_count {
                return Err(INCOMPLETE);
            }
            let total = totals.get_mut(descriptor.index).ok_or(BAD)?;
            *total = total.checked_add(count).ok_or(BAD)?;
        }
        check_movie(input, movie)?;
        receipts.push(receipt);
    }
    if totals.contains(&0) {
        return Err(INCOMPLETE);
    }
    Ok(Counted { receipts, totals })
}

fn raw_movie(
    input: &SegmentInput,
) -> Result<(native_media::Movie, Option<(AudioClaim, u32)>), ConversionError> {
    let mut movie = input.movie().clone();
    let claim = audio_presentation::guarded_claim(input.movie_metadata())?;
    if let Some((_, track_id)) = claim {
        let [track] = movie.tracks.as_mut_slice() else {
            return Err(audio_presentation::MISMATCH);
        };
        if track.track_id != track_id || track.edits.len() != 2 {
            return Err(audio_presentation::MISMATCH);
        }
        // The canonical empty gap and matching media origin cancel exactly.
        // Accumulate unchanged raw sample clocks; bind the finite end afterward.
        track.edits.clear();
    }
    Ok((movie, claim))
}

fn bind_audio(
    claim: Option<(AudioClaim, u32)>,
    media: &Media,
) -> Result<Option<AudioPresentation>, ConversionError> {
    claim
        .map(|(claim, _)| {
            let [track] = media.tracks.as_slice() else {
                return Err(audio_presentation::MISMATCH);
            };
            claim.bind(super::media::audio_evidence(track)?)
        })
        .transpose()
}

fn collect_counted(
    input: &SegmentInput,
    resources: &[PathBuf],
    keys: &InputKeys,
    backend: &Backend,
    counted: Counted,
    clock: Option<super::ManifestAacClock>,
) -> Result<SegmentedMedia, ConversionError> {
    if resources.len() != counted.receipts.len() {
        return Err(INCOMPLETE);
    }
    let (mut movie, claim) = raw_movie(input)?;
    if let Some(clock) = clock {
        clock.check_movie(&movie)?;
    }
    let mut pending = Vec::with_capacity(movie.tracks.len());
    for descriptor in &mut movie.tracks {
        descriptor.sample_count = *counted.totals.get(descriptor.index).ok_or(BAD)?;
        let (samples, config) = accumulator_with_clock(descriptor, clock)?;
        pending.push((samples, config));
    }
    let mut offsets = vec![0_u64; movie.tracks.len()];
    for (resource, expected) in resources.iter().zip(&counted.receipts) {
        backend.check_cancelled()?;
        let (observed, receipt) = input.scan_with_fragment_metadata(
            resource,
            keys,
            &backend.stop,
            &mut |bytes| {
                if let Some((_, track_id)) = claim {
                    audio_presentation::guarded_fragment(bytes, track_id)?;
                }
                Ok(())
            },
            &mut |descriptor, info, bytes| {
                let (samples, _) = pending.get_mut(descriptor.index).ok_or(BAD)?;
                let mut info = info.clone();
                info.ordinal = info
                    .ordinal
                    .checked_add(*offsets.get(descriptor.index).ok_or(BAD)?)
                    .ok_or(BAD)?;
                samples.push(&info, bytes)
            },
        )?;
        // Only these exact guarded bytes may supply the earlier sample totals.
        // Successful callbacks alone never establish a completed resource scan.
        if &receipt != expected {
            return Err(CHANGED);
        }
        for descriptor in &observed.tracks {
            let offset = offsets.get_mut(descriptor.index).ok_or(BAD)?;
            *offset = offset.checked_add(descriptor.sample_count).ok_or(BAD)?;
        }
        check_movie(input, observed)?;
    }
    if offsets != counted.totals {
        return Err(INCOMPLETE);
    }
    let mut tracks = Vec::with_capacity(pending.len());
    for (descriptor, (samples, config)) in movie.tracks.iter().cloned().zip(pending) {
        let samples = samples.finish()?;
        if !samples.continuous_decode {
            return Err(ConversionError("fragment-timeline-gap-or-overlap"));
        }
        if descriptor.protection.scheme == native_media::Scheme::Clear
            && samples.protected_samples != 0
        {
            return Err(ConversionError("native-sample-protection-mismatch"));
        }
        tracks.push(Track {
            descriptor,
            samples,
            config,
        });
    }
    backend.check_cancelled()?;
    let media = Media { movie, tracks };
    let audio_presentation = bind_audio(claim, &media)?;
    super::duration_headers::validate(&media)?;
    let result = SegmentedMedia {
        media,
        audio_presentation,
        boundaries: counted
            .receipts
            .into_iter()
            .map(|receipt| receipt.evidence)
            .collect(),
    };
    backend.native_admit_segmented(&result)?;
    Ok(result)
}

impl Backend {
    /// The caller retains the native resource permit for this complete operation.
    /// No joined file, media subprocess or persistent plaintext is created here.
    pub(in crate::conversion) fn native_segmented_media(
        &self,
        initialization: &Path,
        resources: &[PathBuf],
        keys: &InputKeys,
    ) -> Result<SegmentedMedia, ConversionError> {
        self.check_cancelled()?;
        let input = SegmentInput::new(initialization, &self.stop)?;
        let counted = count_resources(&input, resources, keys, self)?;
        collect_counted(&input, resources, keys, self, counted, None)
    }

    /// The caller has resolved a supported static HLS/DASH resource graph.
    pub(in crate::conversion) fn native_manifest_segmented_media(
        &self,
        initialization: &Path,
        resources: &[PathBuf],
        keys: &InputKeys,
    ) -> Result<SegmentedMedia, ConversionError> {
        self.check_cancelled()?;
        let input = SegmentInput::new(initialization, &self.stop)?;
        let clock = super::ManifestAacClock::qualify_manifest(input.movie())?;
        let counted = count_resources(&input, resources, keys, self)?;
        collect_counted(&input, resources, keys, self, counted, clock)
    }
}

#[cfg(test)]
mod tests;
