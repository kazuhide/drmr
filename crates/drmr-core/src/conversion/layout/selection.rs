//! A saved directory choice binds each output ordinal to its qualified source selector.
use super::{COLLISION, ConversionError, INVALID, LayoutMode, SegmentNames, UNSUPPORTED, paths};
use crate::inspection::{OutputFormat, TrackKind};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// One already qualified source track. All paths are relative to its frozen source root.
#[derive(Clone)]
pub struct SourceTrack {
    pub source: PathBuf,
    pub selector: usize,
    pub kind: TrackKind,
    pub playlist: Option<PathBuf>,
    pub media: Vec<PathBuf>,
}

/// Resolved output paths; no URL, key or DRM endpoint is retained.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrackPlacement {
    pub output_ordinal: usize,
    pub source: PathBuf,
    pub selector: usize,
    pub kind: TrackKind,
    pub media_directory: PathBuf,
    pub playlist: PathBuf,
    pub generated_stem: String,
    #[serde(
        default,
        deserialize_with = "present_names",
        skip_serializing_if = "Option::is_none"
    )]
    pub media_names: Option<SourceNames>,
}

/// Source-derived media names, fixed when a new plan is saved.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceNames {
    pub initialization: String,
    pub segments: SegmentNames,
}

impl SourceNames {
    pub(super) fn for_output(
        &self,
        format: OutputFormat,
        addressing: crate::conversion::dash_addressing::DashAddressing,
        count: usize,
    ) -> Result<&SegmentNames, ConversionError> {
        use crate::conversion::dash_addressing::DashAddressing;
        if format == OutputFormat::Dash
            && matches!(
                addressing,
                DashAddressing::Template | DashAddressing::TemplateDuration
            )
            && self.segments.template("m4s", count)?.is_none()
        {
            Ok(&SegmentNames::Generated)
        } else {
            Ok(&self.segments)
        }
    }
}

fn present_names<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<SourceNames>, D::Error> {
    SourceNames::deserialize(deserializer).map(Some)
}

/// The complete, ordered placement selected while saving a plan.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLayout {
    pub mode: LayoutMode,
    pub tracks: Vec<TrackPlacement>,
}
impl std::fmt::Debug for SourceLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceLayout")
            .field("mode", &self.mode)
            .field("tracks", &self.tracks.len())
            .finish_non_exhaustive()
    }
}
impl std::fmt::Debug for TrackPlacement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackPlacement")
            .field("kind", &self.kind)
            .field("selector", &self.selector)
            .finish_non_exhaustive()
    }
}
impl SourceLayout {
    /// Identify source filename intent without accepting a partially upgraded layout.
    pub fn has_source_names(&self) -> bool {
        !self.tracks.is_empty() && self.tracks.iter().all(|track| track.media_names.is_some())
    }

    /// Older plans retain their recorded generated filenames and serialization.
    pub fn has_legacy_names(&self) -> bool {
        !self.tracks.is_empty() && self.tracks.iter().all(|track| track.media_names.is_none())
    }

    /// Reconstruct a pre-source-filename layout for an existing saved plan.
    pub fn resolve_legacy_from_tracks(
        mode: LayoutMode,
        format: OutputFormat,
        tracks: &[SourceTrack],
    ) -> Result<Self, ConversionError> {
        Self::resolve_legacy_for_processing(
            mode,
            format,
            crate::conversion::ProcessingMode::Stitch,
            tracks,
        )
    }

    /// Preserve the exact naming and grouping used by older saved plans.
    pub fn resolve_legacy_for_processing(
        mode: LayoutMode,
        format: OutputFormat,
        processing: crate::conversion::ProcessingMode,
        tracks: &[SourceTrack],
    ) -> Result<Self, ConversionError> {
        Self::resolve_with_names(mode, format, processing, tracks, false)
    }

    /// Resolve directories from actual referenced media, preserving logical track order.
    pub fn resolve_from_tracks(
        mode: LayoutMode,
        format: OutputFormat,
        tracks: &[SourceTrack],
    ) -> Result<Self, ConversionError> {
        Self::resolve_for_processing(
            mode,
            format,
            crate::conversion::ProcessingMode::Stitch,
            tracks,
        )
    }

    /// Resolve logical streams and physical renditions for the fixed processing route.
    pub fn resolve_for_processing(
        mode: LayoutMode,
        format: OutputFormat,
        processing: crate::conversion::ProcessingMode,
        tracks: &[SourceTrack],
    ) -> Result<Self, ConversionError> {
        Self::resolve_with_names(mode, format, processing, tracks, true)
    }

    pub(super) fn resolve_with_names(
        mode: LayoutMode,
        format: OutputFormat,
        processing: crate::conversion::ProcessingMode,
        tracks: &[SourceTrack],
        preserve_names: bool,
    ) -> Result<Self, ConversionError> {
        if tracks.is_empty() || tracks.len() > 64 {
            return Err(UNSUPPORTED);
        }
        let groups = super::grouping::Groups::resolve(
            format,
            processing,
            tracks.iter().map(|track| track.source.as_path()),
        )?;
        let directories = groups
            .first
            .iter()
            .enumerate()
            .map(|(ordinal, first)| match mode {
                LayoutMode::Normalized => Ok(PathBuf::from(format!("track-{ordinal:04}"))),
                LayoutMode::Source => media_parent(&tracks[*first].media),
            })
            .collect::<Result<Vec<_>, ConversionError>>()?;
        let mut placements: Vec<TrackPlacement> = Vec::with_capacity(tracks.len());
        let mut playlists = BTreeSet::new();
        let mut retained_name_bytes = 0usize;
        for (ordinal, track) in tracks.iter().enumerate() {
            paths::relative_components(&track.source)?;
            if track.selector >= 64 || !matches!(track.kind, TrackKind::Video | TrackKind::Audio) {
                return Err(INVALID);
            }
            let physical = groups.ordinals[ordinal];
            let first = groups.first[physical];
            if first != ordinal {
                if track.media != tracks[first].media || track.playlist != tracks[first].playlist {
                    return Err(INVALID);
                }
                let mut placement = placements[first].clone();
                placement.selector = track.selector;
                placement.kind = track.kind;
                placements.push(placement);
                continue;
            }
            let (media_directory, mut playlist, generated_stem) = initial_paths(
                mode,
                physical,
                &groups.first,
                &directories,
                tracks,
                preserve_names,
            )?;
            let parent = playlist.parent().ok_or(INVALID)?.to_path_buf();
            let mut suffix = 0;
            while top_level_manifest(&playlist)?
                || playlists.contains(&paths::alias(&playlist)?)
                || mode == LayoutMode::Source
                    && if preserve_names {
                        !playlist
                            .extension()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.eq_ignore_ascii_case("m3u8"))
                    } else {
                        generated_name(&playlist, &directories)?
                    }
            {
                if mode == LayoutMode::Normalized || suffix > 64 {
                    return Err(COLLISION);
                }
                playlist = parent.join(format!("playlist-{ordinal:04}-{suffix}.m3u8"));
                suffix += 1;
            }
            playlists.insert(paths::alias(&playlist)?);
            let media_names = if mode == LayoutMode::Source && preserve_names {
                let segments = if track.playlist.is_none()
                    && track.media.as_slice() == [track.source.clone()]
                {
                    SegmentNames::Generated
                } else {
                    SegmentNames::infer(&track.media)?
                };
                retained_name_bytes = name_budget(retained_name_bytes, segments.stored_bytes()?)?;
                Some(SourceNames {
                    initialization: format!("{generated_stem}init.mp4"),
                    segments,
                })
            } else {
                None
            };
            placements.push(TrackPlacement {
                output_ordinal: physical,
                source: track.source.clone(),
                selector: track.selector,
                kind: track.kind,
                media_directory,
                playlist,
                media_names,
                generated_stem,
            });
        }
        let result = Self {
            mode,
            tracks: placements,
        };
        result.validate_for_processing(format, tracks.len(), processing)?;
        Ok(result)
    }

    /// Check a serialized layout without granting any filesystem capability.
    pub fn validate(
        &self,
        format: OutputFormat,
        track_count: usize,
    ) -> Result<(), ConversionError> {
        if !matches!(
            format,
            OutputFormat::Hls | OutputFormat::HlsTs | OutputFormat::Dash
        ) || self.tracks.is_empty()
            || self.tracks.len() > 64
            || self.tracks.len() != track_count
        {
            return Err(UNSUPPORTED);
        }
        let mut claims = paths::PathClaims::default();
        if !self.has_source_names() && !self.has_legacy_names() {
            return Err(INVALID);
        }
        claims.reserve_file(Path::new("master.m3u8"))?;
        claims.reserve_file(Path::new("manifest.mpd"))?;
        let mut selectors = BTreeSet::new();
        let groups = self.physical_groups()?;
        let mut group_kinds = BTreeSet::new();
        let mut retained_name_bytes = 0usize;
        let directories = groups
            .first
            .iter()
            .map(|first| self.tracks[*first].media_directory.clone())
            .collect::<Vec<_>>();
        for (ordinal, track) in self.tracks.iter().enumerate() {
            paths::relative_components(&track.source)?;
            paths::relative_components(&track.playlist)?;
            if !track.media_directory.as_os_str().is_empty() {
                paths::relative_components(&track.media_directory)?;
            }
            let source_alias = paths::alias(&track.source)?;
            if track.selector >= 64
                || !matches!(track.kind, TrackKind::Video | TrackKind::Audio)
                || !selectors.insert((source_alias, track.selector))
                || !group_kinds.insert((track.output_ordinal, track.kind.as_str()))
            {
                return Err(INVALID);
            }
            let physical = track.output_ordinal;
            let first = groups.first[physical];
            if first != ordinal {
                let original = &self.tracks[first];
                if format != OutputFormat::HlsTs
                    || track.source != original.source
                    || track.media_directory != original.media_directory
                    || track.playlist != original.playlist
                    || track.generated_stem != original.generated_stem
                    || track.media_names != original.media_names
                {
                    return Err(INVALID);
                }
                continue;
            }
            match self.mode {
                LayoutMode::Normalized
                    if track.media_directory != Path::new(&format!("track-{physical:04}"))
                        || track.playlist != track.media_directory.join("media.m3u8")
                        || !track.generated_stem.is_empty()
                        || track.media_names.is_some() =>
                {
                    return Err(INVALID);
                }
                LayoutMode::Source
                    if track.media_names.is_none()
                        && track.generated_stem != format!("track-{physical:04}-") =>
                {
                    return Err(INVALID);
                }
                _ => {}
            }
            if track.media_names.is_some() {
                let expected = source_stem(physical, &groups.first, &directories, |index| {
                    (
                        &self.tracks[index].source,
                        self.tracks[index].selector,
                        self.tracks[index].kind,
                    )
                })?;
                retained_name_bytes =
                    name_budget(retained_name_bytes, validate_names(track, &expected)?)?;
            }
            if self.mode == LayoutMode::Source
                && self.has_legacy_names()
                && generated_name(&track.playlist, &directories)?
            {
                return Err(COLLISION);
            }
            claims.reserve_file(&track.playlist)?;
            claims.reserve_file(
                &track
                    .media_directory
                    .join(format!("{}init.mp4", track.generated_stem)),
            )?;
        }
        Ok(())
    }

    /// Validate the physical grouping against the independently saved processing route.
    pub fn validate_for_processing(
        &self,
        format: OutputFormat,
        track_count: usize,
        processing: crate::conversion::ProcessingMode,
    ) -> Result<(), ConversionError> {
        self.validate(format, track_count)?;
        let expected = super::grouping::Groups::resolve(
            format,
            processing,
            self.tracks.iter().map(|track| track.source.as_path()),
        )?;
        if expected.ordinals != self.physical_groups()?.ordinals {
            return Err(INVALID);
        }
        Ok(())
    }

    pub(super) fn physical_groups(&self) -> Result<super::grouping::Groups, ConversionError> {
        super::grouping::Groups::from_saved(self.tracks.iter().map(|track| track.output_ordinal))
    }
}

fn initial_paths(
    mode: LayoutMode,
    physical: usize,
    first: &[usize],
    directories: &[PathBuf],
    tracks: &[SourceTrack],
    preserve_names: bool,
) -> Result<(PathBuf, PathBuf, String), ConversionError> {
    let track = &tracks[first[physical]];
    let result = if mode == LayoutMode::Normalized {
        let directory = PathBuf::from(format!("track-{physical:04}"));
        (
            directory.clone(),
            directory.join("media.m3u8"),
            String::new(),
        )
    } else {
        let directory = directories[physical].clone();
        let playlist = track
            .playlist
            .clone()
            .unwrap_or_else(|| directory.join(format!("playlist-{physical:04}.m3u8")));
        let stem = if preserve_names {
            source_stem(physical, first, directories, |index| {
                (
                    &tracks[index].source,
                    tracks[index].selector,
                    tracks[index].kind,
                )
            })?
        } else {
            format!("track-{physical:04}-")
        };
        (directory, playlist, stem)
    };
    Ok(result)
}

fn validate_names(track: &TrackPlacement, expected: &str) -> Result<usize, ConversionError> {
    let names = track.media_names.as_ref().ok_or(INVALID)?;
    if track.generated_stem != expected || names.initialization != format!("{expected}init.mp4") {
        return Err(INVALID);
    }
    names.segments.validate()?;
    paths::relative_components(Path::new(&names.initialization))?;
    names.segments.stored_bytes()
}

fn name_budget(current: usize, additional: usize) -> Result<usize, ConversionError> {
    current
        .checked_add(additional)
        .filter(|bytes| *bytes <= 16 * 1024 * 1024)
        .ok_or(ConversionError("resource-budget"))
}

fn source_stem<'a>(
    physical: usize,
    first: &[usize],
    directories: &[PathBuf],
    track: impl Fn(usize) -> (&'a PathBuf, usize, TrackKind),
) -> Result<String, ConversionError> {
    let directory_alias = |path: &Path| {
        if path.as_os_str().is_empty() {
            Ok(String::new())
        } else {
            paths::alias(path)
        }
    };
    let directory = directory_alias(&directories[physical])?;
    let neighbors = first
        .iter()
        .enumerate()
        .filter_map(
            |(ordinal, index)| match directory_alias(&directories[ordinal]) {
                Ok(alias) if alias == directory => Some(Ok((ordinal, *index))),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            },
        )
        .collect::<Result<Vec<_>, ConversionError>>()?;
    if neighbors.len() == 1 {
        return Ok(String::new());
    }
    let candidate = |index: usize| -> Result<String, ConversionError> {
        let (source, _, kind) = track(index);
        let stem = source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or(INVALID)?;
        Ok(format!("{stem}-{}-", kind.as_str()))
    };
    let base = candidate(first[physical])?;
    let key = paths::alias(Path::new(&format!("{base}init.mp4")))?;
    let mut collisions = 0;
    for (_, index) in neighbors {
        if paths::alias(Path::new(&format!("{}init.mp4", candidate(index)?)))? == key {
            collisions += 1;
        }
    }
    let result = if collisions > 1 {
        format!("{base}{physical}-")
    } else {
        base
    };
    paths::relative_components(Path::new(&format!("{result}init.mp4")))?;
    Ok(result)
}

fn top_level_manifest(path: &Path) -> Result<bool, ConversionError> {
    Ok(matches!(
        paths::alias(path)?.as_str(),
        "master.m3u8" | "manifest.mpd"
    ))
}

fn generated_name(path: &Path, directories: &[PathBuf]) -> Result<bool, ConversionError> {
    let parent = path.parent().ok_or(INVALID)?;
    let alias = |path: &Path| {
        if path.as_os_str().is_empty() {
            Ok(String::new())
        } else {
            paths::alias(path)
        }
    };
    let parent = alias(parent)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(INVALID)?
        .to_lowercase();
    for (ordinal, directory) in directories.iter().enumerate() {
        if parent == alias(directory)? && name.starts_with(&format!("track-{ordinal:04}-")) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn media_parent(media: &[PathBuf]) -> Result<PathBuf, ConversionError> {
    if media.is_empty() || media.len() > 100_000 {
        return Err(UNSUPPORTED);
    }
    let mut common: Option<Vec<String>> = None;
    for path in media {
        let mut parent = paths::relative_components(path)?;
        parent.pop().ok_or(INVALID)?;
        if let Some(common) = &common {
            if common != &parent {
                return Err(UNSUPPORTED);
            }
        } else {
            common = Some(parent);
        }
    }
    Ok(common.ok_or(UNSUPPORTED)?.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn track(source: &str, media: &str, kind: TrackKind) -> SourceTrack {
        SourceTrack {
            source: source.into(),
            selector: 0,
            kind,
            playlist: Some(source.into()),
            media: vec![media.into()],
        }
    }

    #[test]
    fn isolated_source_directory_does_not_need_a_track_prefix() {
        let layout = SourceLayout::resolve_from_tracks(
            LayoutMode::Source,
            OutputFormat::Hls,
            &[track(
                "audio.m3u8",
                "audio/64000/r000600.ts",
                TrackKind::Audio,
            )],
        )
        .unwrap();
        assert_eq!(layout.tracks[0].generated_stem, "");
        assert_eq!(layout.tracks[0].media_directory, Path::new("audio/64000"));
    }

    #[test]
    fn source_layout_preserves_media_parents_and_root_child_playlists() {
        let tracks = [
            track("video.m3u8", "video/1080p/seg_0.ts", TrackKind::Video),
            track("audio.m3u8", "audio/64000/seg_0.ts", TrackKind::Audio),
        ];
        let layout =
            SourceLayout::resolve_from_tracks(LayoutMode::Source, OutputFormat::Hls, &tracks)
                .unwrap();
        assert_eq!(layout.tracks[0].media_directory, Path::new("video/1080p"));
        assert_eq!(layout.tracks[1].media_directory, Path::new("audio/64000"));
        assert_eq!(layout.tracks[0].playlist, Path::new("video.m3u8"));
        assert_eq!(layout.tracks[1].playlist, Path::new("audio.m3u8"));
        layout.validate(OutputFormat::Hls, 2).unwrap();
    }

    #[test]
    fn shared_flat_parent_uses_unique_generated_file_stems() {
        let tracks = [
            track("a.m3u8", "media/a.ts", TrackKind::Video),
            track("b.m3u8", "media/b.ts", TrackKind::Video),
        ];
        let layout =
            SourceLayout::resolve_from_tracks(LayoutMode::Source, OutputFormat::Dash, &tracks)
                .unwrap();
        assert_eq!(layout.tracks[0].media_directory, Path::new("media"));
        assert_eq!(layout.tracks[1].media_directory, Path::new("media"));
        assert_ne!(
            layout.tracks[0].generated_stem,
            layout.tracks[1].generated_stem
        );
    }

    #[test]
    fn root_manifest_aliases_use_a_safe_child_playlist_name() {
        for source in ["master.m3u8", "MASTER.M3U8", "Master.m3u8", "MANIFEST.MPD"] {
            for format in [OutputFormat::Hls, OutputFormat::HlsTs, OutputFormat::Dash] {
                let tracks = [track(source, "media/segment.ts", TrackKind::Video)];
                let layout = SourceLayout::resolve_from_tracks(LayoutMode::Source, format, &tracks)
                    .unwrap_or_else(|error| panic!("{source}/{format:?}: {error:?}"));
                assert_eq!(layout.tracks[0].playlist, Path::new("playlist-0000-0.m3u8"));
                assert_eq!(layout.tracks[0].media_directory, Path::new("media"));
                assert_eq!(layout.tracks[0].source, Path::new(source));
                layout.validate(format, 1).unwrap();
            }
        }
        let nested = [track(
            "nested/MASTER.M3U8",
            "media/segment.ts",
            TrackKind::Video,
        )];
        let layout =
            SourceLayout::resolve_from_tracks(LayoutMode::Source, OutputFormat::Hls, &nested)
                .unwrap();
        assert_eq!(layout.tracks[0].playlist, Path::new("nested/MASTER.M3U8"));
    }

    #[test]
    fn source_mode_never_silently_flattens_multiple_media_parents() {
        let mut source = track("video.m3u8", "video/first/seg.ts", TrackKind::Video);
        source.media.push("video/second/seg.ts".into());
        assert_eq!(
            SourceLayout::resolve_from_tracks(LayoutMode::Source, OutputFormat::Hls, &[source])
                .err(),
            Some(UNSUPPORTED)
        );
    }

    #[test]
    fn multiplexed_transport_keeps_one_physical_group_only_in_segments_mode() {
        let mut audio = track("muxed.m3u8", "media/seg.ts", TrackKind::Audio);
        audio.selector = 1;
        let sources = [track("muxed.m3u8", "media/seg.ts", TrackKind::Video), audio];
        let grouped = SourceLayout::resolve_for_processing(
            LayoutMode::Source,
            OutputFormat::HlsTs,
            crate::conversion::ProcessingMode::Segments,
            &sources,
        )
        .unwrap();
        assert_eq!(grouped.tracks[0].output_ordinal, 0);
        assert_eq!(grouped.tracks[1].output_ordinal, 0);
        assert_eq!(grouped.tracks[0].playlist, grouped.tracks[1].playlist);
        assert_eq!(
            grouped.tracks[0].generated_stem,
            grouped.tracks[1].generated_stem
        );
        assert!(
            grouped
                .validate_for_processing(
                    OutputFormat::HlsTs,
                    2,
                    crate::conversion::ProcessingMode::Stitch
                )
                .is_err()
        );
        let split =
            SourceLayout::resolve_from_tracks(LayoutMode::Source, OutputFormat::HlsTs, &sources)
                .unwrap();
        assert_eq!(split.tracks[1].output_ordinal, 1);
        SourceLayout::resolve_from_tracks(LayoutMode::Source, OutputFormat::Hls, &sources).unwrap();
    }

    #[test]
    fn saved_physical_groups_reject_gaps_and_unrelated_aliases() {
        let mut audio = track("muxed.m3u8", "media/seg.ts", TrackKind::Audio);
        audio.selector = 1;
        let sources = [track("muxed.m3u8", "media/seg.ts", TrackKind::Video), audio];
        let layout = SourceLayout::resolve_for_processing(
            LayoutMode::Source,
            OutputFormat::HlsTs,
            crate::conversion::ProcessingMode::Segments,
            &sources,
        )
        .unwrap();
        let mut gap = layout.clone();
        gap.tracks[1].output_ordinal = 2;
        assert!(gap.validate(OutputFormat::HlsTs, 2).is_err());
        let mut unrelated = layout.clone();
        unrelated.tracks[1].source = "other.m3u8".into();
        assert!(unrelated.validate(OutputFormat::HlsTs, 2).is_err());
        let mut split_paths = layout.clone();
        split_paths.tracks[1].media_directory = "other".into();
        assert!(split_paths.validate(OutputFormat::HlsTs, 2).is_err());
        let mut duplicate_selector = layout;
        duplicate_selector.tracks[1].selector = 0;
        assert!(duplicate_selector.validate(OutputFormat::HlsTs, 2).is_err());
    }

    #[test]
    fn normalized_layout_preserves_legacy_track_directory_names() {
        let layout = SourceLayout::resolve_from_tracks(
            LayoutMode::Normalized,
            OutputFormat::Hls,
            &[track("audio.m3u8", "audio/seg.ts", TrackKind::Audio)],
        )
        .unwrap();
        assert_eq!(layout.tracks[0].media_directory, Path::new("track-0000"));
        assert_eq!(
            layout.tracks[0].playlist,
            Path::new("track-0000/media.m3u8")
        );
        assert_eq!(layout.tracks[0].generated_stem, "");
    }

    #[test]
    fn a_source_playlist_cannot_take_a_generated_media_filename() {
        let layout = SourceLayout::resolve_from_tracks(
            LayoutMode::Source,
            OutputFormat::Hls,
            &[track(
                "media/track-0000-seg_1.m4s",
                "media/seg.ts",
                TrackKind::Video,
            )],
        )
        .unwrap();
        assert_ne!(
            layout.tracks[0].playlist,
            Path::new("media/track-0000-seg_1.m4s")
        );
        assert_eq!(layout.tracks[0].playlist.parent(), Some(Path::new("media")));
    }

    #[test]
    fn multiplexed_tracks_keep_the_shared_directory_with_distinct_selectors() {
        let first = track("input.m3u8", "media/seg.ts", TrackKind::Video);
        let mut second = track("input.m3u8", "media/seg.ts", TrackKind::Audio);
        second.selector = 1;
        let layout = SourceLayout::resolve_from_tracks(
            LayoutMode::Source,
            OutputFormat::Hls,
            &[first, second],
        )
        .unwrap();
        assert_eq!(
            layout.tracks[0].media_directory,
            layout.tracks[1].media_directory
        );
        assert_ne!(layout.tracks[0].playlist, layout.tracks[1].playlist);
        assert_ne!(
            layout.tracks[0].generated_stem,
            layout.tracks[1].generated_stem
        );
    }

    #[test]
    fn serialized_layouts_reject_escapes_count_changes_and_unknown_fields() {
        let layout = SourceLayout::resolve_from_tracks(
            LayoutMode::Source,
            OutputFormat::Hls,
            &[track("video.m3u8", "video/seg.ts", TrackKind::Video)],
        )
        .unwrap();
        let mut changed = layout.clone();
        changed.tracks[0].playlist = "../escape.m3u8".into();
        assert!(changed.validate(OutputFormat::Hls, 1).is_err());
        assert!(layout.validate(OutputFormat::Hls, 2).is_err());
        changed = layout.clone();
        changed.tracks[0].generated_stem = "other-".into();
        assert!(changed.validate(OutputFormat::Hls, 1).is_err());
        let mut value = serde_json::to_value(&layout).unwrap();
        value["extra"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<SourceLayout>(value).is_err());
    }
}
