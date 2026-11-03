//! Native admission of complete transport renditions bound to their fetched bytes.
use super::{
    ConversionError, Converter, Prepared, SourceQualification, SourceValidationPurpose,
    VerificationSource, apply_transport_metadata_values, hls, hls_media_paths, native_preparation,
};
use std::path::{Path, PathBuf};

pub(in crate::conversion) struct AcquiredRendition {
    pub resources: Vec<PathBuf>,
    pub tracks: Vec<Prepared>,
}

pub(in crate::conversion) fn prepare(
    playlist: &Path,
    directory: &Path,
    backend: &super::super::backend::Backend,
    expected: ([u8; 32], u64),
    resources: &std::collections::BTreeMap<PathBuf, ([u8; 32], u64)>,
) -> Result<AcquiredRendition, ConversionError> {
    let localized =
        hls::localize_acquired_transport(playlist, directory, backend, expected, resources)?;
    let admission = localized
        .transport
        .as_ref()
        .ok_or(ConversionError("ts-original-evidence-required"))?;
    let mut tracks = native_preparation::transport_segments(&localized, admission, backend)?;
    super::apply_hls_target(&mut tracks, localized.target_seconds);
    for track in &mut tracks {
        // These views are qualification-only. Any later normalization takes a
        // fresh private snapshot; no execution consumes this speculative view.
        track.source = playlist.to_path_buf();
        track.media = playlist.to_path_buf();
        if localized.empty_cue_pair {
            track
                .compatibility
                .push(super::SourceCompatibilityNote::HlsEmptyCuePair);
        }
    }
    Ok(AcquiredRendition {
        resources: localized
            .resources
            .into_iter()
            .map(|(path, _)| path)
            .collect(),
        tracks,
    })
}

impl Converter {
    pub(in crate::conversion) fn qualify_acquired_hls(
        &self,
        input: VerificationSource<'_>,
        mut renditions: Vec<(PathBuf, Vec<Prepared>)>,
    ) -> Result<SourceQualification, ConversionError> {
        let command = self.qualification_command(input, SourceValidationPurpose::Conversion)?;
        let paths = hls_media_paths(input.input)?;
        if paths.len() != renditions.len() {
            return Err(ConversionError("source-admission-mismatch"));
        }
        let mut prepared = Vec::new();
        for media in &paths {
            let index = renditions
                .iter()
                .position(|(path, _)| *path == media.path)
                .ok_or(ConversionError("source-admission-mismatch"))?;
            let (_, mut tracks) = renditions.remove(index);
            media.validate_audio_variant(tracks.iter().map(|track| track.report.tracks[0].kind))?;
            if (paths.len() != 1 && tracks.len() != 1)
                || media.role.is_some_and(|kind| {
                    tracks.len() != 1 || tracks[0].report.tracks[0].kind != kind
                })
            {
                return Err(ConversionError("ts-presentation-layout-not-supported"));
            }
            apply_transport_metadata_values(
                &mut tracks,
                media,
                super::SourceProtection::Clear,
                false,
                &command.backend,
                input.input_keys,
            )?;
            prepared.extend(tracks);
        }
        if prepared.len() > 64
            || prepared
                .iter()
                .filter(|track| track.report.tracks[0].kind == super::TrackKind::Audio)
                .count()
                > 1
        {
            return Err(ConversionError("ts-presentation-layout-not-supported"));
        }
        let original = command.inspect_input(input.input)?;
        let qualified = command.qualify_prepared(
            input,
            SourceValidationPurpose::Conversion,
            original,
            &prepared,
        )?;
        Ok(command.finish_qualification(qualified, SourceValidationPurpose::Conversion))
    }
}
