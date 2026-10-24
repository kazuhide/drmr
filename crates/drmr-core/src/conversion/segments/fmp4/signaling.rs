//! Outer AES ciphertext and its localized plaintext need separate metadata guards.
use super::{ConversionError, Converter, Presentation, proof};
use crate::{
    conversion::{
        FrozenSource, ProcessingMode, files, frozen::OriginalGuard, native, native_media::Scheme,
    },
    inspection::{OutputFormat, Protection, TargetProtection},
};
use std::{fmt::Write as _, io::Write as _, path::Path, sync::Arc};

const MISMATCH: ConversionError = ConversionError("output-signaling-verification-failed");

pub(super) struct PlaintextGuard {
    originals: Vec<Arc<OriginalGuard>>,
}

impl PlaintextGuard {
    pub fn check(&self) -> Result<(), ConversionError> {
        self.originals.iter().try_for_each(|guard| guard.check())
    }
}

pub(super) fn verify_plaintext(
    converter: &Converter,
    presentation: &Presentation,
    media: &[native::SegmentedMedia],
) -> Result<PlaintextGuard, ConversionError> {
    if presentation.parts.len() != media.len() {
        return Err(MISMATCH);
    }
    let mut guard = PlaintextGuard {
        originals: Vec::new(),
    };
    for (part, observed) in presentation.parts.iter().zip(media) {
        converter.backend.check_cancelled()?;
        if !matches!(part.outer, Protection::HlsAes128 | Protection::Mixed)
            || observed
                .media
                .tracks
                .iter()
                .any(|track| track.descriptor.protection.scheme != Scheme::Clear)
        {
            continue;
        }
        let _permit = proof::permit(converter)?;
        let path = project(converter, part)?;
        let frozen = FrozenSource::capture(&path, &converter.backend.stop)?;
        let mut clear = converter.with_processing_clone(ProcessingMode::Segments);
        // Whole-resource AES output has no requested inner DRM domain. Original
        // encrypted input signaling is deliberately outside this output check.
        clear.output_signaling = None;
        clear.verify_segment_signaling(
            &frozen,
            OutputFormat::Hls,
            TargetProtection::Clear,
            None,
            None,
            None,
        )?;
        guard.originals.push(frozen.guard());
        // The guarded plaintext originals remain in the localization workspace;
        // release the extra copied graph before checking the next cohort.
    }
    guard.check()?;
    Ok(guard)
}

fn reference<'a>(directory: &Path, path: &'a Path) -> Result<&'a str, ConversionError> {
    if path.parent() != Some(directory) {
        return Err(MISMATCH);
    }
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| {
            !name.is_empty()
                && name.bytes().all(|value| {
                    value.is_ascii_alphanumeric() || matches!(value, b'-' | b'_' | b'.')
                })
        })
        .ok_or(MISMATCH)
}

fn project(
    converter: &Converter,
    part: &super::source::Part,
) -> Result<std::path::PathBuf, ConversionError> {
    let directory = part.initialization.parent().ok_or(MISMATCH)?;
    let initialization = reference(directory, &part.initialization)?;
    let mut size = 256_usize
        .checked_add(initialization.len())
        .ok_or(MISMATCH)?;
    let mut target = 1;
    for resource in &part.resources {
        converter.backend.check_cancelled()?;
        let name = reference(directory, &resource.path)?;
        size = size
            .checked_add(name.len())
            .and_then(|value| value.checked_add(resource.duration.len()))
            .and_then(|value| value.checked_add(32))
            .ok_or(MISMATCH)?;
        if resource.dash.timescale == 0 || resource.dash.duration == 0 {
            return Err(MISMATCH);
        }
        target = target.max(resource.dash.duration.div_ceil(resource.dash.timescale));
    }
    if size as u64 > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(MISMATCH);
    }
    let mut text = String::new();
    text.try_reserve_exact(size)
        .map_err(|_| ConversionError("resource-budget"))?;
    write!(text, "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXT-X-TARGETDURATION:{target}\n#EXT-X-MAP:URI=\"{initialization}\"\n").map_err(|_| MISMATCH)?;
    for resource in &part.resources {
        converter.backend.check_cancelled()?;
        writeln!(
            text,
            "#EXTINF:{},\n{}",
            resource.duration,
            reference(directory, &resource.path)?
        )
        .map_err(|_| MISMATCH)?;
    }
    text.push_str("#EXT-X-ENDLIST\n");
    if text.len() > size {
        return Err(MISMATCH);
    }
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|_| ConversionError("secure-random-unavailable"))?;
    let path = directory.join(format!(
        ".drmr-output-metadata-{}.m3u8",
        hex::encode(random)
    ));
    converter.backend.check_cancelled()?;
    let mut file = files::create_new(&path)?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|_| MISMATCH)?;
    Ok(path)
}
