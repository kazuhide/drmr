//! Confined local static DASH snapshots; manifests never select executables or network access.

use super::{ConversionError, LocalTrack};
use crate::inspection::{self, Protection, TrackKind};
use roxmltree::{Document, Node};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};

const DASH_NS: &str = "urn:mpeg:dash:schema:mpd:2011";
const MAX_SEGMENTS: usize = 100_000;
const MAX_TRACKS: usize = 64;
const MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;

fn error(code: &'static str) -> ConversionError {
    ConversionError(code)
}

fn children<'a>(node: Node<'a, 'a>, name: &str) -> Vec<Node<'a, 'a>> {
    node.children()
        .filter(|child| {
            child.is_element()
                && child.tag_name().name() == name
                && child.tag_name().namespace() == node.tag_name().namespace()
        })
        .collect()
}

fn one<'a>(node: Node<'a, 'a>, name: &str) -> Result<Option<Node<'a, 'a>>, ConversionError> {
    let mut values = children(node, name).into_iter();
    let first = values.next();
    if values.next().is_some() {
        return Err(error("dash-ambiguous-element"));
    }
    Ok(first)
}

fn relative(value: &str) -> Result<PathBuf, ConversionError> {
    if value.is_empty()
        || value.len() > 4096
        || value.chars().any(char::is_control)
        || value.contains(['?', '#', ':', '\\'])
    {
        return Err(error("dash-resource-reference-not-supported"));
    }
    let decoded = crate::source::pyurl::unquote(value);
    if decoded.contains(['\0', '\\', ':', '\u{fffd}'])
        || Path::new(&decoded)
            .components()
            .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(error("dash-resource-outside-root"));
    }
    Ok(PathBuf::from(decoded))
}

fn reference(root: &Path, base: &Path, value: &str) -> Result<PathBuf, ConversionError> {
    let relative = base.join(relative(value)?);
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part);
        if fs::symlink_metadata(&path)
            .map_err(|_| error("dash-resource-unreadable"))?
            .file_type()
            .is_symlink()
        {
            return Err(error("dash-resource-symlink-not-supported"));
        }
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| error("dash-resource-unreadable"))?;
    if !canonical.starts_with(root) {
        return Err(error("dash-resource-outside-root"));
    }
    Ok(canonical)
}

fn expand(
    template: &str,
    id: &str,
    bandwidth: &str,
    number: u64,
    time: u64,
) -> Result<String, ConversionError> {
    if template.len() > 4096 || id.len() > 128 || bandwidth.len() > 32 {
        return Err(error("dash-template-size-limit"));
    }
    let mut remaining = template;
    let mut out = String::new();
    while let Some(begin) = remaining.find('$') {
        out.push_str(&remaining[..begin]);
        remaining = &remaining[begin + 1..];
        if let Some(rest) = remaining.strip_prefix('$') {
            out.push('$');
            remaining = rest;
            continue;
        }
        let end = remaining.find('$').ok_or(error("dash-invalid-template"))?;
        let token = &remaining[..end];
        let (name, format) = token
            .split_once('%')
            .map_or((token, None), |(name, format)| (name, Some(format)));
        match name {
            "RepresentationID" if format.is_none() => out.push_str(id),
            "Bandwidth" if format.is_none() => out.push_str(bandwidth),
            "Number" | "Time" => {
                let value = if name == "Number" { number } else { time };
                let width = match format {
                    None => 0,
                    Some(format) => format
                        .strip_prefix('0')
                        .and_then(|text| text.strip_suffix('d'))
                        .and_then(|text| text.parse::<usize>().ok())
                        .filter(|width| (1..=12).contains(width))
                        .ok_or(error("dash-invalid-template"))?,
                };
                write!(out, "{value:0width$}").expect("writing a string cannot fail");
            }
            _ => return Err(error("dash-template-expression-not-supported")),
        }
        if out.len() > 4096 {
            return Err(error("dash-template-size-limit"));
        }
        remaining = &remaining[end + 1..];
    }
    out.push_str(remaining);
    if out.len() > 4096 {
        return Err(error("dash-template-size-limit"));
    }
    Ok(out)
}

fn attribute<'a>(templates: &[Node<'a, 'a>], name: &str) -> Option<&'a str> {
    templates.iter().rev().find_map(|node| node.attribute(name))
}

fn integer(text: Option<&str>, default: u64) -> Result<u64, ConversionError> {
    text.map_or(Ok(default), |text| {
        text.parse()
            .map_err(|_| error("dash-invalid-template-number"))
    })
}

fn sequence(
    templates: &[Node<'_, '_>],
    remaining: usize,
) -> Result<Vec<(u64, u64, u64)>, ConversionError> {
    if integer(attribute(templates, "timescale"), 1)? == 0
        || integer(attribute(templates, "presentationTimeOffset"), 0)? != 0
    {
        return Err(error("dash-presentation-offset-or-timescale-not-supported"));
    }
    let timeline = templates
        .iter()
        .rev()
        .find_map(|template| match one(*template, "SegmentTimeline") {
            Ok(Some(node)) => Some(Ok(node)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .ok_or(error("dash-finite-segment-timeline-required"))??;
    let start = integer(attribute(templates, "startNumber"), 1)?;
    let mut end = 0_u64;
    let mut values = Vec::new();
    for sample in children(timeline, "S") {
        let begin = integer(sample.attribute("t"), end)?;
        let duration = integer(sample.attribute("d"), 0)?;
        let repeats = integer(sample.attribute("r"), 0)?
            .checked_add(1)
            .ok_or(error("dash-segment-limit"))?;
        if begin != end || duration == 0 {
            return Err(error("dash-timeline-gap-overlap-or-duration-not-supported"));
        }
        let repeats = usize::try_from(repeats).map_err(|_| error("dash-segment-limit"))?;
        if repeats > remaining.saturating_sub(values.len()) {
            return Err(error("dash-segment-limit"));
        }
        for _ in 0..repeats {
            let number = start
                .checked_add(u64::try_from(values.len()).expect("bounded count"))
                .ok_or(error("dash-template-number-overflow"))?;
            values.push((number, end, duration));
            end = end
                .checked_add(duration)
                .ok_or(error("dash-template-number-overflow"))?;
        }
    }
    if values.is_empty() {
        return Err(error("dash-empty-segment-timeline"));
    }
    Ok(values)
}

fn private_new(path: &Path) -> Result<File, ConversionError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|_| error("temporary-file-unavailable"))
}

fn append(
    source: &Path,
    stage: &Path,
    output: &mut File,
    total: &mut u64,
    timing: Option<(&mut super::timing::FragmentClock, (u64, u64, u64))>,
) -> Result<(), ConversionError> {
    let piece = stage.join("dash-piece");
    super::copy_private(source, &piece)?;
    if let Some((clock, (time, duration, scale))) = timing {
        clock.check_dash(&piece, time, duration, scale)?;
    }
    let size = piece
        .metadata()
        .map_err(|_| error("media-copy-failed"))?
        .len();
    if size > MAX_BYTES.saturating_sub(*total) {
        return Err(error("dash-media-size-limit"));
    }
    let copied = std::io::copy(
        &mut File::open(&piece).map_err(|_| error("media-copy-failed"))?,
        output,
    )
    .map_err(|_| error("media-copy-failed"))?;
    if copied != size {
        return Err(error("media-copy-failed"));
    }
    *total += copied;
    fs::remove_file(piece).map_err(|_| error("media-copy-failed"))?;
    Ok(())
}

fn snapshot_representation(
    representation: Node<'_, '_>,
    declared: Protection,
    directory: &Path,
    stage: &Path,
    index: usize,
    remaining: usize,
) -> Result<(LocalTrack, usize), ConversionError> {
    if !matches!(
        declared,
        Protection::Clear | Protection::Cenc | Protection::Cbcs
    ) {
        return Err(error("dash-protection-unresolved-or-not-supported"));
    }
    let mut scopes: Vec<_> = representation
        .ancestors()
        .filter(Node::is_element)
        .collect();
    scopes.reverse();
    let mut base = PathBuf::new();
    let mut templates = Vec::new();
    for scope in scopes {
        if scope.attributes().any(|attribute| {
            attribute.namespace().is_some_and(|namespace| {
                !(namespace == "http://www.w3.org/2001/XMLSchema-instance"
                    && attribute.name() == "schemaLocation")
            })
        }) || scope
            .attribute("lang")
            .is_some_and(|language| language != "und")
            || [
                "Role",
                "Accessibility",
                "Label",
                "EssentialProperty",
                "SupplementalProperty",
            ]
            .iter()
            .any(|name| !children(scope, name).is_empty())
        {
            return Err(error("dash-presentation-metadata-not-supported"));
        }
        if one(scope, "SegmentList")?.is_some() || one(scope, "SegmentBase")?.is_some() {
            return Err(error("dash-addressing-not-supported"));
        }
        if let Some(url) = one(scope, "BaseURL")? {
            base.push(relative(url.text().unwrap_or("").trim())?);
        }
        if let Some(template) = one(scope, "SegmentTemplate")? {
            templates.push(template);
        }
    }
    let values = sequence(&templates, remaining)?;
    let used = values.len();
    let initialization =
        attribute(&templates, "initialization").ok_or(error("dash-initialization-required"))?;
    let media = attribute(&templates, "media").ok_or(error("dash-media-template-required"))?;
    let id = representation.attribute("id").unwrap_or("");
    let bandwidth = representation.attribute("bandwidth").unwrap_or("");
    let source = stage.join(format!("dash-track-{index}.mp4"));
    let mut combined = private_new(&source)?;
    let mut total = 0;
    let initialization = reference(
        directory,
        &base,
        &expand(initialization, id, bandwidth, values[0].0, values[0].1)?,
    )?;
    append(&initialization, stage, &mut combined, &mut total, None)?;
    let mut clock = super::timing::FragmentClock::read(&source)?;
    let scale = integer(attribute(&templates, "timescale"), 1)?;
    let mut seen = HashSet::new();
    for (number, time, duration) in values {
        let segment = reference(
            directory,
            &base,
            &expand(media, id, bandwidth, number, time)?,
        )?;
        if !seen.insert(segment.clone()) {
            return Err(error("dash-repeated-resource-not-supported"));
        }
        append(
            &segment,
            stage,
            &mut combined,
            &mut total,
            Some((&mut clock, (time, duration, scale))),
        )?;
    }
    combined
        .sync_all()
        .map_err(|_| error("media-copy-failed"))?;
    drop(combined);
    let actual = inspection::inspect(&source).map_err(|_| error("dash-invalid-staged-media"))?;
    if actual.tracks.len() != 1 || actual.protection != declared {
        return Err(error("dash-media-protection-mismatch"));
    }
    let kind = actual.tracks[0].kind;
    if !matches!(kind, TrackKind::Video | TrackKind::Audio) {
        return Err(error("dash-media-track-not-supported"));
    }
    Ok((
        LocalTrack {
            source,
            kind,
            index: 0,
        },
        used,
    ))
}

/// Snapshot only a finite, single-period `SegmentTemplate` presentation inside its local root.
/// The returned role is observed from staged initialization metadata, never trusted XML.
pub(crate) fn snapshot_dash(path: &Path, stage: &Path) -> Result<Vec<LocalTrack>, ConversionError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| error("input-unreadable"))?;
    if !metadata.is_file() || metadata.len() > inspection::MAX_MANIFEST_BYTES {
        return Err(error("dash-manifest-unreadable-or-too-large"));
    }
    let manifest = stage.join("dash-manifest.mpd");
    super::copy_private(path, &manifest)?;
    let bytes = inspection::read_manifest(&manifest)
        .map_err(|_| error("dash-manifest-unreadable-or-too-large"))?;
    let declared = inspection::inspect(&manifest).map_err(|_| error("dash-inspection-failed"))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| error("dash-invalid-xml"))?;
    let document = Document::parse_with_options(
        text.trim_start_matches('\u{feff}'),
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| error("dash-invalid-xml"))?;
    let root = document.root_element();
    if root.tag_name().name() != "MPD"
        || !matches!(root.tag_name().namespace(), None | Some(DASH_NS))
        || root.attribute("type").unwrap_or("static") != "static"
    {
        return Err(error("dash-static-presentation-required"));
    }
    let period = one(root, "Period")?.ok_or(error("dash-single-period-required"))?;
    if period
        .attribute("start")
        .is_some_and(|start| !matches!(start, "PT0S" | "PT0.0S"))
    {
        return Err(error("dash-period-offset-not-supported"));
    }
    let representations: Vec<_> = children(period, "AdaptationSet")
        .into_iter()
        .flat_map(|adaptation| children(adaptation, "Representation"))
        .collect();
    if representations.is_empty()
        || representations.len() > MAX_TRACKS
        || declared.tracks.len() != representations.len()
    {
        return Err(error("dash-track-limit-or-structure-not-supported"));
    }
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let directory = parent
        .canonicalize()
        .map_err(|_| error("input-unreadable"))?;
    let mut tracks = Vec::new();
    let mut segments = 0;
    for (index, (representation, declared)) in
        representations.into_iter().zip(declared.tracks).enumerate()
    {
        let (track, used) = snapshot_representation(
            representation,
            declared.protection,
            &directory,
            stage,
            index,
            MAX_SEGMENTS - segments,
        )?;
        tracks.push(track);
        segments += used;
    }
    Ok(tracks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn template_values_are_bounded_and_unknown_expressions_are_refused() {
        assert_eq!(
            expand(
                "v/$RepresentationID$/seg_$Number%05d$_$Time$.$$.m4s",
                "v0",
                "42",
                7,
                100
            )
            .unwrap(),
            "v/v0/seg_00007_100.$.m4s"
        );
        for input in [
            "$Unknown$",
            "$Number%099d$",
            "$Number",
            "$RepresentationID%05d$",
        ] {
            assert!(expand(input, "v0", "42", 7, 100).is_err());
        }
    }

    #[test]
    fn generated_static_dash_becomes_two_private_observed_media_tracks() {
        let root = tempfile::tempdir().unwrap();
        let media = root.path().join("media");
        for name in ["video", "audio"] {
            std::fs::create_dir_all(media.join(name)).unwrap();
        }
        let clip =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../drmr/tests/fixtures/bbb/clip.mp4");
        let mut command = Command::new("packager");
        for role in ["video", "audio"] {
            command.arg(format!(
                "in={},stream={role},init_segment={}/init.mp4,segment_template={}/seg_$Number$.m4s",
                clip.display(),
                media.join(role).display(),
                media.join(role).display()
            ));
        }
        let manifest = media.join("manifest.mpd");
        let result = command
            .args([
                "--segment_duration",
                "2",
                "--fragment_duration",
                "2",
                "--generate_static_live_mpd",
                "--mp4_reset_initial_composition_offset_to_zero=false",
                "--mpd_output",
            ])
            .arg(&manifest)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let stage = tempfile::tempdir().unwrap();
        let tracks = snapshot_dash(&manifest, stage.path()).unwrap();
        assert_eq!(tracks.len(), 2);
        assert_eq!(
            tracks.iter().map(|track| track.kind).collect::<Vec<_>>(),
            [TrackKind::Video, TrackKind::Audio]
        );
        for track in &tracks {
            assert!(track.source.starts_with(stage.path()));
            assert_eq!(
                inspection::inspect(&track.source).unwrap().protection,
                Protection::Clear
            );
            assert_eq!(track.index, 0);
        }
        assert_eq!(
            std::fs::read_dir(&media).unwrap().count(),
            3,
            "snapshot never writes beside source media"
        );
    }

    #[test]
    fn dynamic_periods_external_addresses_and_missing_timeline_are_refused() {
        let root = tempfile::tempdir().unwrap();
        let manifest = root.path().join("manifest.mpd");
        for body in [
            "<MPD type='dynamic'><Period/></MPD>",
            "<MPD type='static'><Period/><Period/></MPD>",
            "<MPD type='static'><BaseURL>https://credential-sentinel.invalid/</BaseURL><Period><AdaptationSet><Representation id='v'><SegmentTemplate initialization='init.mp4' media='seg_$Number$.m4s'><SegmentTimeline><S d='2' r='4'/></SegmentTimeline></SegmentTemplate></Representation></AdaptationSet></Period></MPD>",
            "<MPD type='static'><Period><AdaptationSet><Representation id='v'><SegmentTemplate initialization='init.mp4' media='seg_$Number$.m4s'/></Representation></AdaptationSet></Period></MPD>",
        ] {
            std::fs::write(&manifest, body).unwrap();
            let stage = tempfile::tempdir().unwrap();
            let error = snapshot_dash(&manifest, stage.path())
                .err()
                .expect("unsupported DASH must fail");
            assert!(!format!("{error:?}").contains("sentinel"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn references_cannot_escape_with_parent_components_or_symlinks() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.mp4"), b"sentinel").unwrap();
        symlink(outside.path(), root.path().join("alias")).unwrap();
        assert!(reference(root.path(), Path::new(""), "../secret.mp4").is_err());
        assert!(reference(root.path(), Path::new(""), "alias/secret.mp4").is_err());
        assert!(reference(root.path(), Path::new(""), "%2e%2e/secret.mp4").is_err());
    }
}
