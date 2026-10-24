//! Segment snapshots retain source grouping before any transformation or joining.
use super::super::{
    ConversionError, Converter, FrozenSource, VerificationSource, files, hls, presentation,
};
use crate::inspection::{self, InputFormat, Inspection, OutputFormat};
use std::path::Path;

pub(super) struct Part {
    pub media: hls::Segmented,
    pub declaration: presentation::MediaPath,
}

pub(super) struct HlsSource {
    pub report: Inspection,
    pub format: OutputFormat,
    pub parts: Vec<Part>,
    frozen: FrozenSource,
    _workspace: files::Stage,
}

impl HlsSource {
    pub fn load(
        converter: &Converter,
        input: VerificationSource<'_>,
    ) -> Result<Self, ConversionError> {
        let _scope = converter
            .backend
            .scope(super::super::measurement::Phase::InputPreparation);
        let frozen = FrozenSource::capture(input.input, &converter.backend.stop)?;
        let report = inspection::inspect(frozen.entrypoint())
            .map_err(|_| ConversionError("input-inspection-failed"))?;
        if report.format != InputFormat::Hls {
            return Err(ConversionError("processing-segments-source-required"));
        }
        let declarations = presentation::hls_media_paths(frozen.entrypoint())?;
        let workspace = super::super::workspace()?;
        let parts = converter.run_tracks(declarations.len(), |index| {
            let directory = workspace.path().join(format!("source-{index}"));
            files::create_dir(&directory)?;
            let declaration = &declarations[index];
            let media = hls::localize_segments(
                &declaration.path,
                &directory,
                input.hls_keys,
                &converter.backend,
            )?;
            if let Some(admission) = &media.transport {
                declaration
                    .validate_audio_variant(admission.tracks.iter().map(|track| track.kind))?;
            } else if declaration.audio_variant_alias {
                let initialization = media
                    .initialization
                    .as_deref()
                    .ok_or(ConversionError("hls-map-required"))?;
                let actual = inspection::inspect(initialization)
                    .map_err(|_| ConversionError("invalid-localized-hls-media"))?;
                declaration.validate_audio_variant(actual.tracks.iter().map(|track| track.kind))?;
            }
            Ok((index, media))
        })?;
        let mut parts = parts
            .into_iter()
            .zip(declarations)
            .map(|((_, media), declaration)| Part { media, declaration })
            .collect::<Vec<_>>();
        let transport = parts.iter().all(|part| part.media.transport.is_some());
        if parts.is_empty()
            || (!transport && parts.iter().any(|part| part.media.transport.is_some()))
        {
            return Err(ConversionError("ts-presentation-layout-not-supported"));
        }
        let mut total = 0_u64;
        for part in &mut parts {
            for path in part
                .media
                .initialization
                .iter()
                .chain(part.media.resources.iter().map(|(path, _)| path))
            {
                total = total
                    .checked_add(
                        files::regular_input(path)?
                            .metadata()
                            .map_err(|_| ConversionError("input-unreadable"))?
                            .len(),
                    )
                    .filter(|total| *total <= super::super::MAX_MEDIA_BYTES)
                    .ok_or(ConversionError("source-graph-limit"))?;
            }
        }
        frozen.check_original()?;
        Ok(Self {
            report,
            format: if transport {
                OutputFormat::HlsTs
            } else {
                OutputFormat::Hls
            },
            parts,
            frozen,
            _workspace: workspace,
        })
    }

    pub fn check_original(&self) -> Result<(), ConversionError> {
        self.frozen.check_original()
    }
    pub fn frozen(&self) -> &FrozenSource {
        &self.frozen
    }

    pub fn entrypoint(&self) -> &Path {
        self.frozen.entrypoint()
    }
}

pub(super) fn entrypoint(root: &Path, format: OutputFormat) -> std::path::PathBuf {
    root.join(if format == OutputFormat::Dash {
        "manifest.mpd"
    } else {
        "master.m3u8"
    })
}
