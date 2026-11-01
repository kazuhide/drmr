//! A source-admission scope can record the one modeled legacy VP9 cipher layout.
mod manifest;
use super::Backend;
use crate::conversion::{
    ConversionError, Converter, InputKeys, OutputKey, SourceValidationPurpose,
    TrackConversionRequest, VerificationReport,
    native_media::{self, Vp9Protection},
    verify::StreamView,
};
use crate::inspection::TargetProtection;
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(in crate::conversion) struct SourceAdmission {
    allow_tail: bool,
    needs_repair: AtomicBool,
}

const BAD: ConversionError = ConversionError("native-vp9-output-normalization-failed");

struct Rendition {
    initialization: PathBuf,
    resources: Vec<PathBuf>,
    playlist: Option<PathBuf>,
}

impl Converter {
    pub(in crate::conversion) fn finish_vp9_presentation(
        &self,
        sources: &[PathBuf],
        root: &Path,
        request: &TrackConversionRequest<'_>,
    ) -> Result<(), ConversionError> {
        let configs = sources
            .iter()
            .map(|source| {
                let movie = native_media::inspect(source, &self.backend.stop)?;
                let [track] = movie.tracks.as_slice() else {
                    return Err(BAD);
                };
                Ok((track.codec == *b"vp09").then(|| track.decoder_config.clone()))
            })
            .collect::<Result<Vec<_>, ConversionError>>()?;
        if configs.iter().all(Option::is_none) {
            return Ok(());
        }
        let root = root.canonicalize().map_err(|_| BAD)?;
        let rows = manifest::graph(&root, request.format, &self.backend.stop)?;
        let metadata = retained_bytes(&rows)?;
        let codecs = self.run_units(rows.len(), rows.len(), metadata, |ordinal| {
            let row = &rows[ordinal];
            let index = (0..sources.len())
                .find(|index| row.initialization == root.join(format!("track-{index:04}/init.mp4")))
                .ok_or(BAD)?;
            let Some(config) = &configs[index] else {
                return Ok(None);
            };
            let _permit = self.backend.native_sample_permit()?;
            native_media::restore_vp9_level(&row.initialization, config, &self.backend.stop)?;
            if request.target == TargetProtection::Cenc {
                native_media::reprotect_generated_rendition(
                    &row.initialization,
                    &row.resources,
                    request
                        .output_key
                        .ok_or(ConversionError("output-key-required"))?,
                    &self.backend.stop,
                )?;
            }
            let movie = native_media::inspect(&row.initialization, &self.backend.stop)?;
            let [track] = movie.tracks.as_slice() else {
                return Err(BAD);
            };
            Ok(Some(crate::conversion::segments::fmp4_codec(track)?))
        })?;
        manifest::finish(&root, request.format, &rows, &codecs, &self.backend.stop)
    }
}

fn retained_bytes(rows: &[Rendition]) -> Result<u64, ConversionError> {
    let mut bytes = rows
        .len()
        .checked_mul(std::mem::size_of::<Rendition>())
        .ok_or(BAD)?;
    for row in rows {
        bytes = bytes
            .checked_add(row.initialization.capacity())
            .ok_or(BAD)?;
        bytes = bytes
            .checked_add(
                row.resources
                    .capacity()
                    .checked_mul(std::mem::size_of::<PathBuf>())
                    .ok_or(BAD)?,
            )
            .ok_or(BAD)?;
        for resource in &row.resources {
            bytes = bytes.checked_add(resource.capacity()).ok_or(BAD)?;
        }
        if let Some(playlist) = &row.playlist {
            bytes = bytes.checked_add(playlist.capacity()).ok_or(BAD)?;
        }
    }
    u64::try_from(bytes).map_err(|_| BAD)
}

impl Backend {
    /// Both views are the same provided source, before any generated output.
    pub(in crate::conversion) fn verify_source_views(
        &self,
        source: StreamView<'_>,
        view: StreamView<'_>,
    ) -> Result<VerificationReport, ConversionError> {
        self.vp9_admission_scope(SourceValidationPurpose::Conversion)
            .verify_views(source, view)
    }

    pub(in crate::conversion) fn finish_vp9_flat_hls(
        &self,
        source: &Path,
        output: &Path,
    ) -> Result<(), ConversionError> {
        let source = native_media::inspect(source, &self.stop)?;
        let descriptor = source.tracks.first().ok_or(BAD)?;
        if descriptor.codec != *b"vp09" {
            return Ok(());
        }
        let _permit = self.native_sample_permit()?;
        let root = output.canonicalize().map_err(|_| BAD)?;
        let rows = manifest::graph(&root, crate::inspection::OutputFormat::Hls, &self.stop)?;
        let [row] = rows.as_slice() else {
            return Err(BAD);
        };
        native_media::restore_vp9_level(
            &row.initialization,
            &descriptor.decoder_config,
            &self.stop,
        )?;
        let actual = native_media::inspect(&row.initialization, &self.stop)?;
        let [track] = actual.tracks.as_slice() else {
            return Err(BAD);
        };
        let codec = crate::conversion::segments::fmp4_codec(track)?;
        manifest::finish(
            &root,
            crate::inspection::OutputFormat::Hls,
            &rows,
            &[Some(codec)],
            &self.stop,
        )
    }

    pub(in crate::conversion) fn admit_vp9_encrypted_input(
        &self,
        input: &Path,
        index: usize,
        decryption: Option<&InputKeys>,
    ) -> Result<(), ConversionError> {
        let Some(keys) = decryption else {
            return Ok(());
        };
        let movie = native_media::inspect(input, &self.stop)?;
        let track = movie
            .tracks
            .get(index)
            .ok_or(ConversionError("stream-selector-mismatch"))?;
        if track.codec == *b"vp09" && track.protection.scheme != native_media::Scheme::Clear {
            // Reject unsupported source maps before delegating any decryption.
            // Fresh private producer candidates use the separate reprotector.
            self.native_validate_mp4(
                input,
                index,
                Some(crate::conversion::keys::ProbeKeys::All(keys)),
            )?;
        }
        Ok(())
    }

    /// Only fresh private tool output enters the same-key native cipher rewrite.
    /// Public source admission retains its narrower modeled-layout requirement.
    pub(in crate::conversion) fn finish_vp9_mp4(
        &self,
        source: &Path,
        index: usize,
        output: &Path,
        encryption: Option<(&OutputKey, TargetProtection)>,
    ) -> Result<(), ConversionError> {
        let source = native_media::inspect(source, &self.stop)?;
        let descriptor = source
            .tracks
            .get(index)
            .ok_or(ConversionError("stream-selector-mismatch"))?;
        if descriptor.codec != *b"vp09" {
            return Ok(());
        }
        let _permit = self.native_sample_permit()?;
        native_media::restore_vp9_level(output, &descriptor.decoder_config, &self.stop)?;
        if let Some((key, TargetProtection::Cenc)) = encryption {
            native_media::reprotect_generated_file(output, key, &self.stop)?;
        }
        Ok(())
    }

    pub(in crate::conversion) fn vp9_admission_scope(
        &self,
        purpose: SourceValidationPurpose,
    ) -> Self {
        let mut backend = self.clone();
        backend.vp9_admission = Some(Arc::new(SourceAdmission {
            allow_tail: matches!(purpose, SourceValidationPurpose::Conversion),
            needs_repair: AtomicBool::new(false),
        }));
        backend
    }

    pub(in crate::conversion) fn vp9_source_repair_allowed(&self) -> bool {
        self.vp9_admission
            .as_ref()
            .is_some_and(|scope| scope.allow_tail)
    }

    pub(in crate::conversion) fn vp9_source_needs_repair(&self) -> bool {
        self.vp9_admission
            .as_ref()
            .is_some_and(|scope| scope.needs_repair.load(Ordering::Relaxed))
    }

    pub(in crate::conversion) fn observe_vp9_protection(
        &self,
        protection: Option<Vp9Protection>,
    ) -> Result<(), ConversionError> {
        if protection == Some(Vp9Protection::ShakaTailCenc)
            && let Some(scope) = &self.vp9_admission
        {
            if !scope.allow_tail {
                return Err(native_media::VP9_LAYOUT);
            }
            scope.needs_repair.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}
