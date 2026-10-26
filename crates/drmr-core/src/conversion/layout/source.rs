//! Resolve placement from the admitted graph without reopening media payloads.
use super::{
    ConversionError, INVALID, LayoutMode, QualifiedTrack, SourceLayout, SourceTrack, UNSUPPORTED,
};
use crate::{
    concurrency::Stop,
    conversion::{SourceQualification, dash, files, hls, presentation},
    inspection::{InputFormat, OutputFormat},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

impl SourceLayout {
    /// Bind the actual admitted stream order to paths in the same frozen source graph.
    /// The caller must keep that graph sealed until execution or plan capture finishes.
    pub fn resolve_from_qualified_source(
        input: &Path,
        qualified: &SourceQualification,
        mode: LayoutMode,
        output: OutputFormat,
        processing: crate::conversion::ProcessingMode,
        stop: &Stop,
    ) -> Result<Self, ConversionError> {
        if !qualified.verification.passed
            || qualified.layout_tracks.len() != qualified.track_protection.len()
        {
            return Err(UNSUPPORTED);
        }
        Self::resolve_from_admitted_tracks(
            input,
            qualified.input.format,
            &qualified.layout_tracks,
            mode,
            output,
            processing,
            stop,
        )
    }

    /// Reuse facts already produced by source preparation; inspect only bounded
    /// manifest topology and index metadata, never media payloads or decoding.
    pub(in crate::conversion) fn resolve_from_admitted_tracks(
        input: &Path,
        source_format: InputFormat,
        facts: &[QualifiedTrack],
        mode: LayoutMode,
        output: OutputFormat,
        processing: crate::conversion::ProcessingMode,
        stop: &Stop,
    ) -> Result<Self, ConversionError> {
        let tracks = source_tracks(input, source_format, facts, stop)?;
        let layout = Self::resolve_for_processing(mode, output, processing, &tracks)?;
        check(stop)?;
        Ok(layout)
    }

    /// Check the selected directories against the final manifest after all layout and
    /// addressing changes. This complements the independent media and DRM verification.
    pub(in crate::conversion) fn verify_actual_output(
        &self,
        root: &Path,
        format: OutputFormat,
        stop: &Stop,
    ) -> Result<(), ConversionError> {
        check(stop)?;
        self.validate(format, self.tracks.len())?;
        let root = root.canonicalize().map_err(|_| INVALID)?;
        let groups = self.physical_groups()?;
        let mut selected = BTreeSet::new();
        if format == OutputFormat::Dash {
            let rows = dash::layout_output_resources(&root.join("manifest.mpd"), stop)?;
            if rows.len() != groups.first.len() {
                return Err(UNSUPPORTED);
            }
            for media in rows {
                check(stop)?;
                let matches = groups
                    .first
                    .iter()
                    .filter(|ordinal| placement_matches(&self.tracks[**ordinal], &media))
                    .copied()
                    .collect::<Vec<_>>();
                if matches.len() != 1 || !selected.insert(matches[0]) {
                    return Err(INVALID);
                }
            }
        } else {
            let declarations = presentation::hls_media_paths(&root.join("master.m3u8"))?;
            if declarations.len() != groups.first.len() {
                return Err(UNSUPPORTED);
            }
            for declaration in declarations {
                check(stop)?;
                let path = relative(&root, &declaration.path)?;
                let ordinal = groups
                    .first
                    .iter()
                    .find(|ordinal| self.tracks[**ordinal].playlist == path)
                    .copied()
                    .ok_or(INVALID)?;
                let track = &self.tracks[ordinal];
                if !selected.insert(ordinal)
                    || !placement_matches(
                        track,
                        &hls_resources(&declaration.path, &root, stop, true)?,
                    )
                {
                    return Err(INVALID);
                }
            }
        }
        if selected.len() != groups.first.len() {
            return Err(INVALID);
        }
        Ok(())
    }
}

fn placement_matches(track: &super::TrackPlacement, media: &[PathBuf]) -> bool {
    !media.is_empty()
        && media.iter().all(|path| {
            path.parent() == Some(track.media_directory.as_path())
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&track.generated_stem))
        })
}

fn source_tracks(
    input: &Path,
    format: InputFormat,
    qualified: &[QualifiedTrack],
    stop: &Stop,
) -> Result<Vec<SourceTrack>, ConversionError> {
    check(stop)?;
    if qualified.is_empty() || qualified.len() > 64 {
        return Err(UNSUPPORTED);
    }
    let root = input
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
        .canonicalize()
        .map_err(|_| INVALID)?;
    let input = root.join(input.file_name().ok_or(INVALID)?);
    match format {
        InputFormat::Hls => hls_tracks(&input, &root, qualified, stop),
        InputFormat::Dash => {
            let rows = dash::layout_resources(&input, stop)?;
            if rows.len() != qualified.len() {
                return Err(UNSUPPORTED);
            }
            let source = relative(&root, &input)?;
            rows.into_iter()
                .zip(qualified)
                .enumerate()
                .map(|(ordinal, (media, fact))| {
                    if fact.selector != ordinal {
                        return Err(UNSUPPORTED);
                    }
                    Ok(SourceTrack {
                        source: source.clone(),
                        selector: fact.selector,
                        kind: fact.kind,
                        playlist: None,
                        media,
                    })
                })
                .collect()
        }
        InputFormat::Mp4 => {
            let source = relative(&root, &input)?;
            Ok(qualified
                .iter()
                .map(|fact| SourceTrack {
                    source: source.clone(),
                    selector: fact.selector,
                    kind: fact.kind,
                    playlist: None,
                    media: vec![source.clone()],
                })
                .collect())
        }
    }
}

fn hls_tracks(
    input: &Path,
    root: &Path,
    qualified: &[QualifiedTrack],
    stop: &Stop,
) -> Result<Vec<SourceTrack>, ConversionError> {
    let declarations = presentation::hls_media_paths(input)?;
    if declarations.len() != qualified.len() && declarations.len() != 1 {
        return Err(UNSUPPORTED);
    }
    let mut tracks = Vec::with_capacity(qualified.len());
    for (ordinal, fact) in qualified.iter().enumerate() {
        check(stop)?;
        let declaration = &declarations[if declarations.len() == 1 { 0 } else { ordinal }];
        if declaration.role.is_some_and(|kind| kind != fact.kind) {
            return Err(UNSUPPORTED);
        }
        let source = relative(root, &declaration.path)?;
        tracks.push(SourceTrack {
            source: source.clone(),
            selector: fact.selector,
            kind: fact.kind,
            playlist: Some(source),
            media: media_resources(&declaration.path, root, stop)?,
        });
    }
    Ok(tracks)
}

fn media_resources(
    playlist: &Path,
    root: &Path,
    stop: &Stop,
) -> Result<Vec<PathBuf>, ConversionError> {
    hls_resources(playlist, root, stop, false)
}

fn hls_resources(
    playlist: &Path,
    root: &Path,
    stop: &Stop,
    include_initialization: bool,
) -> Result<Vec<PathBuf>, ConversionError> {
    let bytes = files::read_manifest(playlist)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| INVALID)?;
    let parent = playlist.parent().ok_or(INVALID)?;
    let mut media = Vec::new();
    for line in text.lines().map(str::trim) {
        check(stop)?;
        let initialization =
            if include_initialization && let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
                Some(
                    hls::attributes(value)?
                        .get("URI")
                        .ok_or(INVALID)?
                        .to_owned(),
                )
            } else {
                None
            };
        let uri = initialization
            .as_deref()
            .or_else(|| (!line.is_empty() && !line.starts_with('#')).then_some(line));
        let Some(uri) = uri else { continue };
        if media.len() == 100_000 + usize::from(include_initialization) {
            return Err(UNSUPPORTED);
        }
        media.push(relative(root, &hls::local_resource(parent, uri)?)?);
    }
    if media.is_empty() {
        return Err(UNSUPPORTED);
    }
    Ok(media)
}

fn relative(root: &Path, source: &Path) -> Result<PathBuf, ConversionError> {
    let path = source
        .strip_prefix(root)
        .map_err(|_| INVALID)?
        .to_path_buf();
    super::paths::relative_components(&path)?;
    Ok(path)
}

fn check(stop: &Stop) -> Result<(), ConversionError> {
    if stop.is_set() {
        Err(ConversionError("cancelled"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inspection::TrackKind;
    use std::fs;

    #[test]
    fn actual_media_parent_is_used_instead_of_the_root_child_playlist_parent() {
        let root = crate::native::private_tempdir().unwrap();
        fs::create_dir_all(root.path().join("video/1080p-high")).unwrap();
        fs::write(
            root.path().join("video/1080p-high/seg_0.ts"),
            b"unchanged media",
        )
        .unwrap();
        fs::write(
            root.path().join("video.m3u8"),
            "#EXTM3U\n#EXTINF:6,\nvideo/1080p-high/seg_0.ts\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
        let input = root.path().join("input.m3u8");
        fs::write(
            &input,
            "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000,CODECS=\"avc1.64001f\"\nvideo.m3u8\n",
        )
        .unwrap();
        let layout = SourceLayout::resolve_from_admitted_tracks(
            &input,
            InputFormat::Hls,
            &[QualifiedTrack {
                kind: TrackKind::Video,
                selector: 0,
            }],
            LayoutMode::Source,
            OutputFormat::Hls,
            crate::conversion::ProcessingMode::Stitch,
            &Stop::new(),
        )
        .unwrap();
        assert_eq!(
            layout.tracks[0].media_directory,
            Path::new("video/1080p-high")
        );
        assert_eq!(layout.tracks[0].playlist, Path::new("video.m3u8"));
        assert_eq!(layout.tracks[0].source, Path::new("video.m3u8"));
    }

    #[test]
    fn cancellation_does_not_open_a_source() {
        let stop = Stop::new();
        stop.set();
        assert_eq!(
            SourceLayout::resolve_from_admitted_tracks(
                Path::new("missing/input.m3u8"),
                InputFormat::Hls,
                &[],
                LayoutMode::Source,
                OutputFormat::Hls,
                crate::conversion::ProcessingMode::Stitch,
                &stop
            )
            .err(),
            Some(ConversionError("cancelled"))
        );
    }

    #[test]
    fn source_initialization_may_be_outside_the_media_parent() {
        let root = crate::native::private_tempdir().unwrap();
        fs::create_dir_all(root.path().join("headers")).unwrap();
        fs::create_dir_all(root.path().join("video/720p")).unwrap();
        fs::write(
            root.path().join("headers/source.mp4"),
            b"already admitted initialization",
        )
        .unwrap();
        fs::write(
            root.path().join("video/720p/seg_1.m4s"),
            b"already admitted media",
        )
        .unwrap();
        let manifests = [
            (
                InputFormat::Hls,
                "input.m3u8",
                "#EXTM3U\n#EXT-X-VERSION:6\n#EXT-X-TARGETDURATION:6\n#EXT-X-MAP:URI=\"headers/source.mp4\"\n#EXTINF:6,\nvideo/720p/seg_1.m4s\n#EXT-X-ENDLIST\n",
            ),
            (
                InputFormat::Dash,
                "input.mpd",
                r#"<MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" mediaPresentationDuration="PT6S"><Period><AdaptationSet><Representation id="v"><SegmentTemplate timescale="1" duration="6" initialization="headers/source.mp4" media="video/720p/seg_$Number$.m4s"/></Representation></AdaptationSet></Period></MPD>"#,
            ),
        ];
        for (format, name, text) in manifests {
            let input = root.path().join(name);
            fs::write(&input, text).unwrap();
            let layout = SourceLayout::resolve_from_admitted_tracks(
                &input,
                format,
                &[QualifiedTrack {
                    kind: TrackKind::Video,
                    selector: 0,
                }],
                LayoutMode::Source,
                OutputFormat::Dash,
                crate::conversion::ProcessingMode::Stitch,
                &Stop::new(),
            )
            .unwrap();
            assert_eq!(layout.tracks[0].media_directory, Path::new("video/720p"));
        }
    }

    #[test]
    fn local_parent_aliases_bind_the_same_relative_mp4_source() {
        let root = crate::native::private_tempdir().unwrap();
        fs::create_dir(root.path().join("alias")).unwrap();
        fs::write(root.path().join("input.mp4"), b"already admitted media").unwrap();
        let input = root.path().join("alias/../input.mp4");
        let layout = SourceLayout::resolve_from_admitted_tracks(
            &input,
            InputFormat::Mp4,
            &[QualifiedTrack {
                kind: TrackKind::Audio,
                selector: 0,
            }],
            LayoutMode::Source,
            OutputFormat::Hls,
            crate::conversion::ProcessingMode::Stitch,
            &Stop::new(),
        )
        .unwrap();
        assert_eq!(layout.tracks[0].source, Path::new("input.mp4"));
        assert_eq!(layout.tracks[0].media_directory, Path::new(""));
    }

    #[test]
    fn a_bare_relative_admitted_name_uses_the_current_directory() {
        let layout = SourceLayout::resolve_from_admitted_tracks(
            Path::new("already-admitted.mp4"),
            InputFormat::Mp4,
            &[QualifiedTrack {
                kind: TrackKind::Video,
                selector: 0,
            }],
            LayoutMode::Source,
            OutputFormat::Dash,
            crate::conversion::ProcessingMode::Stitch,
            &Stop::new(),
        )
        .unwrap();
        assert_eq!(layout.tracks[0].source, Path::new("already-admitted.mp4"));
        assert_eq!(layout.tracks[0].media_directory, Path::new(""));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_source_parents_keep_the_same_mp4_and_hls_relative_paths() {
        let owner = crate::native::private_tempdir().unwrap();
        let root = owner.path().join("real");
        fs::create_dir_all(root.join("video/720p")).unwrap();
        fs::write(root.join("input.mp4"), b"already admitted media").unwrap();
        fs::write(
            root.join("video/720p/seg_0.ts"),
            b"already admitted segment",
        )
        .unwrap();
        fs::write(root.join("media.m3u8"), "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nvideo/720p/seg_0.ts\n#EXT-X-ENDLIST\n").unwrap();
        let alias = owner.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        for (format, name, directory) in [
            (InputFormat::Mp4, "input.mp4", ""),
            (InputFormat::Hls, "media.m3u8", "video/720p"),
        ] {
            let resolve = |parent: &Path| {
                SourceLayout::resolve_from_admitted_tracks(
                    &parent.join(name),
                    format,
                    &[QualifiedTrack {
                        kind: TrackKind::Video,
                        selector: 0,
                    }],
                    LayoutMode::Source,
                    OutputFormat::Dash,
                    crate::conversion::ProcessingMode::Stitch,
                    &Stop::new(),
                )
                .unwrap()
            };
            assert_eq!(resolve(&alias), resolve(&root.canonicalize().unwrap()));
            assert_eq!(
                resolve(&alias).tracks[0].media_directory,
                Path::new(directory)
            );
        }
        assert_eq!(
            fs::read(root.join("input.mp4")).unwrap(),
            b"already admitted media"
        );
        assert_eq!(
            fs::read(root.join("video/720p/seg_0.ts")).unwrap(),
            b"already admitted segment"
        );
    }
}
