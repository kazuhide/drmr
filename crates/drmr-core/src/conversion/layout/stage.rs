//! Re-layout the private candidate before any final media or manifest verification.
use super::{ConversionError, INVALID, LayoutMode, SourceLayout, path_uri, paths};
use crate::{
    concurrency::Stop,
    conversion::{
        dash_addressing::DashAddressing,
        files::{Stage, StageRelocation},
    },
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
    pub templates: BTreeMap<PathBuf, RelocatedTemplate>,
}
#[derive(Clone)]
pub(in crate::conversion) struct RelocatedTemplate {
    pub media: Option<String>,
    pub start_number: u64,
}

impl SourceLayout {
    /// Move a completed private package without copying media bytes, then rewrite its
    /// references. All independent media, signaling and manifest checks must run
    /// against this final layout after any further DASH addressing selection.
    pub(in crate::conversion) fn apply_to_stage(
        &self,
        stage: &Stage,
        format: OutputFormat,
        addressing: DashAddressing,
        stop: &Stop,
    ) -> Result<RelocatedOutput, ConversionError> {
        check(stop)?;
        self.validate(format, self.tracks.len())?;
        let mut tree = StageRelocation::new(stage)?;
        let output = self.capture_stage(format, addressing, &mut tree, stop)?;
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
        addressing: DashAddressing,
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
        let mut state = CaptureState {
            output: RelocatedOutput {
                tracks: Vec::new(),
                files: BTreeMap::new(),
                templates: BTreeMap::new(),
            },
            claims: paths::PathClaims::default(),
            retained_bytes: 0,
        };
        state.claims.reserve_file(Path::new(manifest))?;
        let mut physical_tracks = Vec::with_capacity(groups.first.len());
        for (ordinal, first) in groups.first.iter().enumerate() {
            physical_tracks.push(capture_track(
                ordinal,
                &self.tracks[*first],
                format,
                addressing,
                tree,
                stop,
                &mut state,
            )?);
        }
        state.output.tracks = groups
            .ordinals
            .into_iter()
            .map(|ordinal| physical_tracks[ordinal].clone())
            .collect();
        tree.check()?;
        Ok(state.output)
    }
}

struct CaptureState {
    output: RelocatedOutput,
    claims: paths::PathClaims,
    retained_bytes: usize,
}

fn capture_track(
    ordinal: usize,
    placement: &super::TrackPlacement,
    format: OutputFormat,
    addressing: DashAddressing,
    tree: &mut StageRelocation<'_>,
    stop: &Stop,
    state: &mut CaptureState,
) -> Result<RelocatedTrack, ConversionError> {
    check(stop)?;
    let directory = PathBuf::from(format!("track-{ordinal:04}"));
    tree.capture_directory(&directory)?;
    let mut actual = RelocatedTrack {
        playlist: placement.playlist.clone(),
        initialization: None,
        media: Vec::new(),
    };
    let entries = tree.entries(&directory, 100_000 - state.output.files.len())?;
    let indices = segment_indices(&entries)?;
    let count = indices.len();
    let extension = if format == OutputFormat::HlsTs {
        "ts"
    } else {
        "m4s"
    };
    let selected_names = placement
        .media_names
        .as_ref()
        .map(|names| names.for_output(format, addressing, count))
        .transpose()?;
    let mut media = Vec::with_capacity(count);
    for name in &entries {
        check(stop)?;
        let name = name.to_str().ok_or(INVALID)?;
        let source = directory.join(name);
        if state.output.files.len() == 100_000 {
            return Err(INVALID);
        }
        tree.capture_file(&source)?;
        let destination = if name == "media.m3u8" {
            if format == OutputFormat::Dash {
                return Err(INVALID);
            }
            actual.playlist.clone()
        } else if let Some(names) = &placement.media_names {
            let name = if name == "init.mp4" {
                names.initialization.clone()
            } else {
                let index = *indices.get(name).ok_or(INVALID)?;
                format!(
                    "{}{}",
                    placement.generated_stem,
                    selected_names
                        .ok_or(INVALID)?
                        .file_name(index, count, extension)?
                )
            };
            placement.media_directory.join(name)
        } else {
            placement
                .media_directory
                .join(format!("{}{name}", placement.generated_stem))
        };
        state.claims.reserve_file(&destination)?;
        state.retained_bytes = path_budget(state.retained_bytes, &source, &destination)?;
        if name == "init.mp4" {
            actual.initialization = Some(destination.clone());
        } else if name != "media.m3u8" {
            media.push((*indices.get(name).ok_or(INVALID)?, destination.clone()));
        }
        state.output.files.insert(source, destination);
    }
    media.sort_by_key(|(index, _)| *index);
    actual.media = media.into_iter().map(|(_, path)| path).collect();
    if actual.media.is_empty() || (format == OutputFormat::HlsTs) == actual.initialization.is_some()
    {
        return Err(INVALID);
    }
    if format != OutputFormat::Dash
        && !state
            .output
            .files
            .contains_key(&directory.join("media.m3u8"))
    {
        return Err(INVALID);
    }
    state.output.templates.insert(
        directory.join("init.mp4"),
        media_template(placement, selected_names, extension, count)?,
    );
    Ok(actual)
}

fn segment_indices(entries: &[PathBuf]) -> Result<BTreeMap<&str, usize>, ConversionError> {
    let mut numbered = entries
        .iter()
        .filter_map(|name| match name.to_str() {
            Some("init.mp4" | "media.m3u8") => None,
            Some(name) => Some(segment_number(name).map(|number| (number, name))),
            None => Some(Err(INVALID)),
        })
        .collect::<Result<Vec<_>, ConversionError>>()?;
    numbered.sort_by_key(|(number, _)| *number);
    if numbered.is_empty() || numbered.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err(INVALID);
    }
    let indices = numbered
        .iter()
        .enumerate()
        .map(|(index, (_, name))| (*name, index))
        .collect::<BTreeMap<_, _>>();
    Ok(indices)
}

fn media_template(
    placement: &super::TrackPlacement,
    names: Option<&super::SegmentNames>,
    extension: &str,
    count: usize,
) -> Result<RelocatedTemplate, ConversionError> {
    let prefix = if placement.media_directory.as_os_str().is_empty() {
        String::new()
    } else {
        format!("{}/", path_uri(&placement.media_directory)?)
    };
    let template = match names {
        Some(names) => names.template(extension, count)?,
        None => Some(("seg_$Number$.m4s".into(), 1)),
    };
    let stem_uri = if placement.generated_stem.is_empty() {
        String::new()
    } else {
        path_uri(Path::new(&placement.generated_stem))?.replace('$', "$$")
    };
    let (media, start_number) = match template {
        Some((name, start)) => (
            Some(format!("{}{stem_uri}{name}", prefix.replace('$', "$$"))),
            start,
        ),
        None => (None, 1),
    };
    Ok(RelocatedTemplate {
        media,
        start_number,
    })
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
mod source_names_tests;
#[cfg(test)]
mod tests;
