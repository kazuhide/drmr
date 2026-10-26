//! Re-layout the private candidate before any final media or manifest verification.
use super::{ConversionError, INVALID, LayoutMode, SourceLayout, path_uri, paths};
use crate::{
    concurrency::Stop,
    conversion::files::{Stage, StageRelocation},
    inspection::OutputFormat,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Clone)]
pub(in crate::conversion) struct RelocatedTrack {
    pub playlist: PathBuf,
    pub initialization: Option<PathBuf>,
    pub media: Vec<PathBuf>,
}
pub(in crate::conversion) struct RelocatedOutput {
    pub tracks: Vec<RelocatedTrack>,
    pub files: BTreeMap<PathBuf, PathBuf>,
    pub templates: BTreeMap<PathBuf, String>,
}

impl SourceLayout {
    /// Move a completed private package without copying media bytes, then rewrite its
    /// references. All independent media, signaling and manifest checks must run
    /// against this final layout after any further DASH addressing selection.
    pub(in crate::conversion) fn apply_to_stage(
        &self,
        stage: &Stage,
        format: OutputFormat,
        stop: &Stop,
    ) -> Result<RelocatedOutput, ConversionError> {
        check(stop)?;
        self.validate(format, self.tracks.len())?;
        let mut tree = StageRelocation::new(stage)?;
        let output = self.capture_stage(format, &mut tree, stop)?;
        let physical_count = self.physical_groups()?.first.len();
        if self.mode == LayoutMode::Normalized {
            return Ok(output);
        }
        let playlists = if format == OutputFormat::Dash {
            Vec::new()
        } else {
            let mut names = vec![PathBuf::from("master.m3u8")];
            names.extend(
                (0..physical_count)
                    .map(|ordinal| PathBuf::from(format!("track-{ordinal:04}/media.m3u8"))),
            );
            names
                .into_iter()
                .map(|name| {
                    let destination = output
                        .files
                        .get(&name)
                        .cloned()
                        .unwrap_or_else(|| name.clone());
                    let bytes = tree.read_manifest(&name)?;
                    rewrite_hls(&bytes, &name, &destination, &output.files)?;
                    Ok((name, destination))
                })
                .collect::<Result<Vec<_>, ConversionError>>()?
        };
        let PendingRelocation {
            directory: temporary_directory,
            files: temporary,
        } = relocate_files(&output.files, &mut tree, stop)?;
        for ordinal in 0..physical_count {
            check(stop)?;
            tree.remove_empty_directory(Path::new(&format!("track-{ordinal:04}")))?;
        }
        for (_, destination) in &temporary {
            check(stop)?;
            tree.prepare_parents(destination)?;
        }
        for (source, destination) in temporary {
            check(stop)?;
            tree.move_file(&source, &destination)?;
        }
        for (original, destination) in playlists {
            check(stop)?;
            let bytes = tree.read_manifest(&destination)?;
            let bytes = rewrite_hls(&bytes, &original, &destination, &output.files)?;
            tree.replace_manifest(&destination, &bytes)?;
        }
        tree.remove_empty_directory(&temporary_directory)?;
        tree.check()?;
        if format == OutputFormat::Dash {
            crate::conversion::dash::relocate_output(
                stage.path(),
                &output.files,
                &output.templates,
                stop,
            )?;
        }
        tree.check()?;
        Ok(output)
    }

    fn capture_stage(
        &self,
        format: OutputFormat,
        tree: &mut StageRelocation<'_>,
        stop: &Stop,
    ) -> Result<RelocatedOutput, ConversionError> {
        let manifest = if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        };
        let groups = self.physical_groups()?;
        let mut expected = (0..groups.first.len())
            .map(|ordinal| format!("track-{ordinal:04}"))
            .collect::<std::collections::BTreeSet<_>>();
        expected.insert(manifest.into());
        for name in tree.entries(Path::new(""), groups.first.len() + 1)? {
            if !expected.remove(name.to_str().ok_or(INVALID)?) {
                return Err(INVALID);
            }
        }
        if !expected.is_empty() {
            return Err(INVALID);
        }
        tree.capture_file(Path::new(manifest))?;
        let mut output = RelocatedOutput {
            tracks: Vec::with_capacity(self.tracks.len()),
            files: BTreeMap::new(),
            templates: BTreeMap::new(),
        };
        let mut claims = paths::PathClaims::default();
        claims.reserve_file(Path::new(manifest))?;
        let mut retained_bytes = 0usize;
        let mut physical_tracks = Vec::with_capacity(groups.first.len());
        for (ordinal, first) in groups.first.iter().enumerate() {
            let placement = &self.tracks[*first];
            check(stop)?;
            let directory = PathBuf::from(format!("track-{ordinal:04}"));
            tree.capture_directory(&directory)?;
            let mut actual = RelocatedTrack {
                playlist: placement.playlist.clone(),
                initialization: None,
                media: Vec::new(),
            };
            for name in tree.entries(&directory, 100_000 - output.files.len())? {
                check(stop)?;
                let name = name.to_str().ok_or(INVALID)?;
                let source = directory.join(name);
                if output.files.len() == 100_000 {
                    return Err(INVALID);
                }
                tree.capture_file(&source)?;
                let destination = if name == "media.m3u8" {
                    if format == OutputFormat::Dash {
                        return Err(INVALID);
                    }
                    actual.playlist.clone()
                } else {
                    placement
                        .media_directory
                        .join(format!("{}{name}", placement.generated_stem))
                };
                claims.reserve_file(&destination)?;
                retained_bytes = path_budget(retained_bytes, &source, &destination)?;
                if name == "init.mp4" {
                    actual.initialization = Some(destination.clone());
                } else if name != "media.m3u8" {
                    segment_number(name)?;
                    actual.media.push(destination.clone());
                }
                output.files.insert(source, destination);
            }
            actual.media = ordered_media(actual.media, &placement.generated_stem)?;
            if actual.media.is_empty()
                || (format == OutputFormat::HlsTs) == actual.initialization.is_some()
            {
                return Err(INVALID);
            }
            if format != OutputFormat::Dash
                && !output.files.contains_key(&directory.join("media.m3u8"))
            {
                return Err(INVALID);
            }
            let prefix = if placement.media_directory.as_os_str().is_empty() {
                String::new()
            } else {
                format!("{}/", path_uri(&placement.media_directory)?)
            };
            output.templates.insert(
                directory.join("init.mp4"),
                format!("{prefix}{}seg_$Number$.m4s", placement.generated_stem),
            );
            physical_tracks.push(actual);
        }
        output.tracks = groups
            .ordinals
            .into_iter()
            .map(|ordinal| physical_tracks[ordinal].clone())
            .collect();
        tree.check()?;
        Ok(output)
    }
}

fn path_budget(
    current: usize,
    source: &Path,
    destination: &Path,
) -> Result<usize, ConversionError> {
    current
        .checked_add(
            (source.as_os_str().len() + destination.as_os_str().len())
                .saturating_mul(8)
                .saturating_add(1024),
        )
        .filter(|bytes| *bytes <= 128 * 1024 * 1024)
        .ok_or(ConversionError("resource-budget"))
}

struct PendingRelocation {
    directory: PathBuf,
    files: Vec<(PathBuf, PathBuf)>,
}

fn relocate_files(
    files: &BTreeMap<PathBuf, PathBuf>,
    tree: &mut StageRelocation<'_>,
    stop: &Stop,
) -> Result<PendingRelocation, ConversionError> {
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| ConversionError("secure-random-unavailable"))?;
    let directory = PathBuf::from(format!(".drmr-layout-{}", hex::encode(nonce)));
    tree.prepare_parents(&directory.join("pending"))?;
    let mut pending = Vec::with_capacity(files.len());
    for (ordinal, (source, destination)) in files.iter().enumerate() {
        check(stop)?;
        let temporary = directory.join(format!("r{ordinal:06}"));
        tree.move_file(source, &temporary)?;
        pending.push((temporary, destination.clone()));
    }
    Ok(PendingRelocation {
        directory,
        files: pending,
    })
}

fn ordered_media(media: Vec<PathBuf>, prefix: &str) -> Result<Vec<PathBuf>, ConversionError> {
    let mut media = media
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_prefix(prefix))
                .ok_or(INVALID)?;
            Ok((segment_number(name)?, path))
        })
        .collect::<Result<Vec<_>, ConversionError>>()?;
    media.sort_by_key(|(number, _)| *number);
    if media.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(INVALID);
    }
    Ok(media.into_iter().map(|(_, path)| path).collect())
}

fn segment_number(name: &str) -> Result<u64, ConversionError> {
    let number = name
        .strip_prefix("seg_")
        .and_then(|s| s.strip_suffix(".m4s"))
        .or_else(|| {
            name.strip_prefix("segment-")
                .and_then(|s| s.strip_suffix(".m4s").or_else(|| s.strip_suffix(".ts")))
        })
        .ok_or(INVALID)?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return Err(INVALID);
    }
    number.parse().map_err(|_| INVALID)
}

fn rewrite_hls(
    bytes: &[u8],
    original: &Path,
    destination: &Path,
    files: &BTreeMap<PathBuf, PathBuf>,
) -> Result<Vec<u8>, ConversionError> {
    let text = std::str::from_utf8(bytes).map_err(|_| INVALID)?;
    let old_parent = original.parent().ok_or(INVALID)?;
    let new_parent = destination.parent().ok_or(INVALID)?;
    let rewrite = |uri: &str| {
        if uri.contains([':', '?', '#', '\\']) {
            return Err(INVALID);
        }
        let decoded = crate::source::pyurl::unquote(uri);
        paths::relative_components(Path::new(&decoded))?;
        let new = files.get(&old_parent.join(decoded)).ok_or(INVALID)?;
        path_uri(new.strip_prefix(new_parent).map_err(|_| INVALID)?)
    };
    let mut result = String::new();
    for line in text.lines() {
        if !line.is_empty() && !line.starts_with('#') {
            result.push_str(&rewrite(line)?);
        } else if line.starts_with("#EXT-X-MAP:")
            || line.starts_with("#EXT-X-MEDIA:")
            || line.starts_with("#EXT-X-I-FRAME-STREAM-INF:")
        {
            let (_, attributes) = line.split_once(':').ok_or(INVALID)?;
            let attributes = crate::conversion::hls::attributes(attributes)?;
            if let Some(uri) = attributes.get("URI") {
                let needle = format!("URI=\"{uri}\"");
                if line.matches(&needle).count() != 1 {
                    return Err(INVALID);
                }
                result.push_str(&line.replacen(&needle, &format!("URI=\"{}\"", rewrite(uri)?), 1));
            } else {
                result.push_str(line);
            }
        } else {
            result.push_str(line);
        }
        result.push('\n');
        if result.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
            return Err(INVALID);
        }
    }
    Ok(result.into_bytes())
}

fn check(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
