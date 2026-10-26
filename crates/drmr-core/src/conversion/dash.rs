//! Confined local static DASH snapshots; manifests never select executables or network access.

use super::{ConversionError, LocalTrack};
pub(super) mod addressing;
mod index;
mod nominal;
mod policy;
mod relocate;
mod resources;
mod segments;
mod verify;
mod window;
pub(in crate::conversion) use policy::rewrite_output;
pub(in crate::conversion) use relocate::relocate_output;
pub(in crate::conversion) use resources::{check_acquired_limits, record_resources};
pub(crate) use verify::verify_output_addressing;
#[cfg(test)]
mod addressing_tests;
#[cfg(test)]
mod policy_tests;
use crate::inspection::{self, Protection, TrackKind};
use roxmltree::{Document, Node};
use segments::{Budget, checkpoint};
pub(super) use segments::{SegmentBoundary, Segmented, SegmentedRepresentation};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};
pub(super) use window::Window;
pub(super) use window::finish_transport;
pub(in crate::conversion) use window::finish_transport_projection;

pub(super) const DASH_NS: &str = "urn:mpeg:dash:schema:mpd:2011";
pub(super) const MAX_SEGMENTS: usize = 100_000;
pub(super) const MAX_TRACKS: usize = 64;
const MAX_BYTES: u64 = 16 * 1024 * 1024 * 1024;

/// Validate a staged flat index before an owned metadata-only clock adjustment.
pub(in crate::conversion) fn staged_index_scale(
    bytes: &[u8],
    start: u64,
    file_bytes: u64,
    track: u32,
) -> Result<u64, ConversionError> {
    let end = start
        .checked_add(bytes.len() as u64)
        .and_then(|end| end.checked_sub(1))
        .ok_or(error("dash-invalid-index"))?;
    let parsed = index::parse(
        bytes,
        addressing::ByteRange { start, end },
        file_bytes,
        "media.mp4",
        MAX_SEGMENTS,
    )?;
    if parsed.reference_id != track {
        return Err(error("dash-index-track-or-timescale-mismatch"));
    }
    Ok(parsed.timescale)
}

enum CapturedRepresentation {
    Joined(LocalTrack),
    Segments(SegmentedRepresentation),
}
struct Completed {
    manifest: PathBuf,
    report: inspection::Inspection,
    representations: Vec<CapturedRepresentation>,
}
struct Capture<'a> {
    stop: Option<&'a crate::concurrency::Stop>,
    budget: &'a mut Budget,
}

fn error(code: &'static str) -> ConversionError {
    ConversionError(code)
}

pub(super) fn children<'a>(node: Node<'a, 'a>, name: &str) -> Vec<Node<'a, 'a>> {
    node.children()
        .filter(|child| {
            child.is_element()
                && child.tag_name().name() == name
                && child.tag_name().namespace() == node.tag_name().namespace()
        })
        .collect()
}

pub(super) fn one<'a>(
    node: Node<'a, 'a>,
    name: &str,
) -> Result<Option<Node<'a, 'a>>, ConversionError> {
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
    #[cfg(windows)]
    super::files::regular_input(&path)?;
    let canonical = path
        .canonicalize()
        .map_err(|_| error("dash-resource-unreadable"))?;
    if !canonical.starts_with(root) {
        return Err(error("dash-resource-outside-root"));
    }
    Ok(canonical)
}

pub(super) fn expand(
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

pub(super) fn attribute<'a>(templates: &[Node<'a, 'a>], name: &str) -> Option<&'a str> {
    templates.iter().rev().find_map(|node| node.attribute(name))
}

pub(super) fn integer(text: Option<&str>, default: u64) -> Result<u64, ConversionError> {
    text.map_or(Ok(default), |text| {
        text.parse()
            .map_err(|_| error("dash-invalid-template-number"))
    })
}

pub(super) fn timeline<'a>(templates: &[Node<'a, 'a>]) -> Result<Node<'a, 'a>, ConversionError> {
    templates
        .iter()
        .rev()
        .find_map(|template| match one(*template, "SegmentTimeline") {
            Ok(Some(node)) => Some(Ok(node)),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        })
        .ok_or(error("dash-finite-segment-timeline-required"))?
}

pub(super) fn sequence(
    templates: &[Node<'_, '_>],
    remaining: usize,
) -> Result<Vec<(u64, u64, u64)>, ConversionError> {
    if integer(attribute(templates, "timescale"), 1)? == 0 {
        return Err(error("dash-presentation-offset-or-timescale-not-supported"));
    }
    let timeline = timeline(templates)?;
    let start = integer(attribute(templates, "startNumber"), 1)?;
    let mut end = 0_u64;
    let mut values = Vec::new();
    for sample in children(timeline, "S") {
        let begin = integer(sample.attribute("t"), end)?;
        if values.is_empty() {
            end = begin;
        }
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
    super::files::create_new(path)
}

/// Apply the same presentation and addressing admission before local or remote resolution.
pub(super) fn validated_scopes<'a>(
    representation: Node<'a, 'a>,
) -> Result<Vec<Node<'a, 'a>>, ConversionError> {
    let mut scopes: Vec<_> = representation
        .ancestors()
        .filter(Node::is_element)
        .collect();
    scopes.reverse();
    super::presentation::metadata::validate_dash(representation)?;
    for &scope in &scopes {
        if scope.attributes().any(|attribute| {
            attribute.namespace().is_some_and(|namespace| {
                !(namespace == "http://www.w3.org/2001/XMLSchema-instance"
                    && attribute.name() == "schemaLocation")
            })
        }) || [
            "Accessibility",
            "EssentialProperty",
            "SupplementalProperty",
            "EventStream",
            "InbandEventStream",
            "ContentComponent",
            "ProducerReferenceTime",
        ]
        .iter()
        .any(|name| !children(scope, name).is_empty())
        {
            return Err(error("dash-presentation-metadata-not-supported"));
        }
        let mut count = 0;
        for name in ["SegmentTemplate", "SegmentList", "SegmentBase"] {
            count += usize::from(one(scope, name)?.is_some());
        }
        if count > 1 {
            return Err(error("dash-addressing-ambiguous"));
        }
    }
    Ok(scopes)
}

fn representation_scopes<'a>(
    representation: Node<'a, 'a>,
) -> Result<(PathBuf, Vec<Node<'a, 'a>>), ConversionError> {
    let mut base = PathBuf::new();
    let mut templates = Vec::new();
    for scope in validated_scopes(representation)? {
        if let Some(url) = one(scope, "BaseURL")? {
            base.push(relative(url.text().unwrap_or("").trim())?);
        }
        if let Some(template) = one(scope, "SegmentTemplate")? {
            templates.push(template);
        }
    }
    Ok((base, templates))
}

/// Enumerate the finite local resources from already-frozen manifest bytes.
/// Media admission remains the responsibility of the ordinary snapshot handler.
pub(super) fn source_references(bytes: &[u8]) -> Result<Vec<PathBuf>, ConversionError> {
    let text = std::str::from_utf8(bytes).map_err(|_| error("dash-invalid-xml"))?;
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
    let representations: Vec<_> = children(period, "AdaptationSet")
        .into_iter()
        .flat_map(|adaptation| children(adaptation, "Representation"))
        .collect();
    if representations.is_empty() || representations.len() > MAX_TRACKS {
        return Err(error("dash-track-limit-or-structure-not-supported"));
    }
    let mut resources = Vec::new();
    let mut path_bytes = 0usize;
    let mut add = |path: PathBuf| {
        path_bytes = path_bytes
            .checked_add(path.as_os_str().len())
            .filter(|bytes| *bytes <= 16 * 1024 * 1024)
            .ok_or(error("source-graph-limit"))?;
        resources.push(path);
        Ok::<_, ConversionError>(())
    };
    let mut remaining = MAX_SEGMENTS;
    let mut retained = HashSet::new();
    for representation in representations {
        let resolved = addressing::resolve(representation, remaining)?;
        remaining = remaining.saturating_sub(resolved.segments.len());
        for resource in std::iter::once(&resolved.initialization)
            .chain(resolved.segments.iter().map(|segment| &segment.resource))
            .chain(resolved.index.iter())
        {
            let path = resolved.base.join(relative(&resource.uri)?);
            if retained.insert(path.clone()) {
                add(path)?;
            }
        }
    }
    Ok(resources)
}

/// Ordered media-object paths for already qualified source-layout planning; no media is read.
pub(in crate::conversion) fn layout_resources(
    path: &Path,
    stop: &crate::concurrency::Stop,
) -> Result<Vec<Vec<PathBuf>>, ConversionError> {
    layout_resource_paths(path, stop, false)
}

/// Final output placement includes initialization, unlike source media-parent inference.
pub(in crate::conversion) fn layout_output_resources(
    path: &Path,
    stop: &crate::concurrency::Stop,
) -> Result<Vec<Vec<PathBuf>>, ConversionError> {
    layout_resource_paths(path, stop, true)
}

fn layout_resource_paths(
    path: &Path,
    stop: &crate::concurrency::Stop,
    include_initialization: bool,
) -> Result<Vec<Vec<PathBuf>>, ConversionError> {
    checkpoint(Some(stop))?;
    let bytes = super::files::read_manifest(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| error("dash-invalid-xml"))?;
    let doc = Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| error("dash-invalid-xml"))?;
    let root = doc.root_element();
    let period = one(root, "Period")?.ok_or(error("dash-single-period-required"))?;
    let mut rows = Vec::new();
    let mut remaining = MAX_SEGMENTS;
    for rep in children(period, "AdaptationSet")
        .into_iter()
        .flat_map(|set| children(set, "Representation"))
    {
        checkpoint(Some(stop))?;
        let resolved = addressing::resolve(rep, remaining)?;
        remaining = remaining.saturating_sub(resolved.segments.len());
        let mut row = Vec::new();
        let mut seen = HashSet::new();
        let base = resolved.kind == addressing::Kind::Base;
        let initialization = (include_initialization || base).then_some(&resolved.initialization);
        for resource in initialization.into_iter().chain(
            resolved
                .segments
                .iter()
                .filter(|_| !base)
                .map(|segment| &segment.resource),
        ) {
            let path = resolved.base.join(relative(&resource.uri)?);
            if seen.insert(path.clone()) {
                row.push(path);
            }
        }
        rows.push(row);
        if rows.len() > MAX_TRACKS {
            return Err(error("dash-track-limit-or-structure-not-supported"));
        }
    }
    if rows.is_empty() {
        return Err(error("dash-track-limit-or-structure-not-supported"));
    }
    Ok(rows)
}

fn captured_representation(
    source: Option<PathBuf>,
    initialization: Option<PathBuf>,
    resources: Vec<(PathBuf, SegmentBoundary)>,
    declared: Protection,
    window: window::Window,
) -> Result<CapturedRepresentation, ConversionError> {
    let observed = source
        .as_deref()
        .or(initialization.as_deref())
        .ok_or(error("dash-initialization-required"))?;
    let actual = inspection::inspect(observed).map_err(|_| error("dash-invalid-staged-media"))?;
    if actual.tracks.len() != 1 || actual.protection != declared {
        return Err(error("dash-media-protection-mismatch"));
    }
    let kind = actual.tracks[0].kind;
    if !matches!(kind, TrackKind::Video | TrackKind::Audio) {
        return Err(error("dash-media-track-not-supported"));
    }
    if let Some(source) = source {
        Ok(CapturedRepresentation::Joined(LocalTrack {
            source,
            kind,
            index: 0,
        }))
    } else {
        Ok(CapturedRepresentation::Segments(SegmentedRepresentation {
            initialization: initialization.ok_or(error("dash-initialization-required"))?,
            resources,
            window,
            kind,
            protection: actual.protection,
        }))
    }
}

fn snapshot_representation(
    representation: Node<'_, '_>,
    declared: Protection,
    directory: &Path,
    stage: &Path,
    index: usize,
    remaining: usize,
    capture: &mut Capture<'_>,
) -> Result<(CapturedRepresentation, usize, window::Window), ConversionError> {
    checkpoint(capture.stop)?;
    if !matches!(
        declared,
        Protection::Clear | Protection::Cenc | Protection::Cbcs
    ) {
        return Err(error("dash-protection-unresolved-or-not-supported"));
    }
    let mut resolved = addressing::resolve(representation, remaining)?;
    let fallback_stop = crate::concurrency::Stop::new();
    let stop = capture.stop.unwrap_or(&fallback_stop);
    let resource_directory = stage.join(format!("dash-representation-{index:04}"));
    super::files::create_dir(&resource_directory)?;
    let locate =
        |resource: &addressing::Resource| reference(directory, &resolved.base, &resource.uri);
    let original_initialization = locate(&resolved.initialization)?;
    let copied_initialization = resource_directory.join("initialization.mp4");
    capture.budget.snapshot_range(
        &original_initialization,
        &copied_initialization,
        resolved.initialization.range,
        stop,
    )?;
    let mut clock = super::timing::FragmentClock::read(&copied_initialization)?;
    self::index::resolve(directory, &mut resolved, clock.identity(), remaining, stop)?;
    let used = resolved.segments.len();
    let source = capture
        .stop
        .is_none()
        .then(|| stage.join(format!("dash-track-{index}.mp4")));
    let mut combined = source.as_deref().map(private_new).transpose()?;
    if let Some(joined) = combined.as_mut() {
        let mut initialization = super::files::regular_input(&copied_initialization)?;
        std::io::copy(&mut initialization, joined).map_err(|_| error("media-copy-failed"))?;
    }
    let mut resources = Vec::with_capacity(if combined.is_some() { 0 } else { used });
    for (ordinal, segment) in resolved.segments.iter().enumerate() {
        checkpoint(capture.stop)?;
        let original = reference(directory, &resolved.base, &segment.resource.uri)?;
        let copied = resource_directory.join(format!("segment-{ordinal:08}.m4s"));
        capture
            .budget
            .snapshot_range(&original, &copied, segment.resource.range, stop)?;
        let decode = segments::check_clock(&resolved, segment, &mut clock, &copied, ordinal)?;
        if let Some(joined) = combined.as_mut() {
            std::io::copy(&mut super::files::regular_input(&copied)?, joined)
                .map_err(|_| error("media-copy-failed"))?;
            fs::remove_file(&copied).map_err(|_| error("media-copy-failed"))?;
        } else {
            // Store actual decode intervals even when the public MPD uses nominal or index timing.
            resources.push((
                copied,
                SegmentBoundary {
                    number: segment.number,
                    time: decode.0,
                    duration: decode.1,
                    timescale: decode.2,
                },
            ));
        }
    }
    let actual = clock.presentation_window()?;
    if let Some(duration) = resolved.presentation_duration {
        let end = resolved
            .offset
            .checked_add(duration)
            .ok_or(error("dash-template-number-overflow"))?;
        if u128::from(actual.1) * u128::from(resolved.timescale)
            != u128::from(end) * u128::from(actual.2)
        {
            return Err(error("dash-representation-duration-mismatch"));
        }
    }
    let window = window::Window {
        origin: (resolved.offset, resolved.timescale),
        first: (actual.0, actual.2),
        end: (actual.1, actual.2),
    };
    if let Some(joined) = combined.take() {
        joined.sync_all().map_err(|_| error("media-copy-failed"))?;
        drop(joined);
        fs::remove_file(&copied_initialization).map_err(|_| error("media-copy-failed"))?;
    }
    let initialization = source.is_none().then_some(copied_initialization);

    let representation =
        captured_representation(source, initialization, resources, declared, window)?;
    Ok((representation, used, window))
}

/// Snapshot only a finite, single-period `SegmentTemplate` presentation inside its local root.
/// The returned role is observed from staged initialization metadata, never trusted XML.
pub(crate) fn snapshot_dash(path: &Path, stage: &Path) -> Result<Vec<LocalTrack>, ConversionError> {
    snapshot(path, stage, None)?
        .representations
        .into_iter()
        .map(|representation| match representation {
            CapturedRepresentation::Joined(track) => Ok(track),
            CapturedRepresentation::Segments(_) => Err(error("dash-snapshot-mode-invalid")),
        })
        .collect()
}

/// Retain qualified original resources and exact timeline units without joining tracks.
pub(super) fn snapshot_segments(
    path: &Path,
    stage: &Path,
    stop: &crate::concurrency::Stop,
) -> Result<Segmented, ConversionError> {
    let captured = snapshot(path, stage, Some(stop))?;
    let representations = captured
        .representations
        .into_iter()
        .map(|representation| match representation {
            CapturedRepresentation::Segments(track) => Ok(track),
            CapturedRepresentation::Joined(_) => Err(error("dash-snapshot-mode-invalid")),
        })
        .collect::<Result<_, _>>()?;
    Ok(Segmented {
        manifest: captured.manifest,
        report: captured.report,
        representations,
    })
}

fn snapshot(
    path: &Path,
    stage: &Path,
    stop: Option<&crate::concurrency::Stop>,
) -> Result<Completed, ConversionError> {
    checkpoint(stop)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| error("input-unreadable"))?;
    if !metadata.is_file() || metadata.len() > inspection::MAX_MANIFEST_BYTES {
        return Err(error("dash-manifest-unreadable-or-too-large"));
    }
    let manifest = stage.join("dash-manifest.mpd");
    let mut budget = Budget::default();
    if let Some(stop) = stop {
        budget.snapshot(path, &manifest, stop)?;
    } else {
        super::copy_private(path, &manifest)?;
    }
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
    let mut windows = window::Presentation::default();
    for (index, (representation, declared)) in representations
        .into_iter()
        .zip(&declared.tracks)
        .enumerate()
    {
        checkpoint(stop)?;
        let (track, used, track_window) = snapshot_representation(
            representation,
            declared.protection,
            &directory,
            stage,
            index,
            MAX_SEGMENTS - segments,
            &mut Capture {
                stop,
                budget: &mut budget,
            },
        )?;
        tracks.push(track);
        segments += used;
        windows.include(track_window)?;
    }
    windows.check_origin()?;
    if root.attribute("mediaPresentationDuration").is_none()
        && period.attribute("duration").is_none()
    {
        return Err(error("dash-presentation-duration-required"));
    }
    for value in [
        root.attribute("mediaPresentationDuration"),
        period.attribute("duration"),
    ]
    .into_iter()
    .flatten()
    {
        windows.check_duration(value)?;
    }
    Ok(Completed {
        manifest,
        report: declared,
        representations: tracks,
    })
}

pub(super) fn decimal_duration(value: &str) -> Result<(u64, u64), ConversionError> {
    let text = value
        .strip_prefix("PT")
        .and_then(|v| v.strip_suffix('S'))
        .ok_or(error("dash-duration-syntax-not-supported"))?;
    if text.is_empty() || text.len() > 32 {
        return Err(error("dash-duration-syntax-not-supported"));
    }
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    if whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > 9
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || (text.contains('.') && fraction.is_empty())
    {
        return Err(error("dash-duration-syntax-not-supported"));
    }
    let whole = whole
        .parse::<u64>()
        .map_err(|_| error("dash-duration-syntax-not-supported"))?;
    let scale = 10u64.pow(
        u32::try_from(fraction.len()).map_err(|_| error("dash-duration-syntax-not-supported"))?,
    );
    let fraction = if fraction.is_empty() {
        0
    } else {
        fraction
            .parse::<u64>()
            .map_err(|_| error("dash-duration-syntax-not-supported"))?
    };
    let numerator = whole
        .checked_mul(scale)
        .and_then(|n| n.checked_add(fraction))
        .filter(|n| *n > 0)
        .ok_or(error("dash-duration-syntax-not-supported"))?;
    Ok((numerator, scale))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn absolute_segment_timeline_keeps_its_nonzero_clock_and_refuses_internal_gaps() {
        let xml = "<SegmentTemplate timescale='90000' presentationTimeOffset='125997'><SegmentTimeline><S t='126000' d='1920' r='2'/><S d='960'/></SegmentTimeline></SegmentTemplate>";
        let doc = Document::parse(xml).unwrap();
        assert_eq!(
            sequence(&[doc.root_element()], 4).unwrap(),
            vec![
                (1, 126_000, 1920),
                (2, 127_920, 1920),
                (3, 129_840, 1920),
                (4, 131_760, 960)
            ]
        );
        for changed in [
            xml.replace("<S d='960'", "<S t='131759' d='960'"),
            xml.replace("<S d='960'", "<S t='131761' d='960'"),
        ] {
            let doc = Document::parse(&changed).unwrap();
            assert_eq!(
                sequence(&[doc.root_element()], 4).unwrap_err().0,
                "dash-timeline-gap-overlap-or-duration-not-supported"
            );
        }
    }

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

    fn assert_segment_snapshot(tracks: &[LocalTrack], segmented: &Segmented, root: &Path) {
        for (track, representation) in tracks.iter().zip(&segmented.representations) {
            assert_eq!(track.kind, representation.kind);
            assert_eq!(representation.protection, Protection::Clear);
            assert!(representation.initialization.starts_with(root));
            assert!(representation.initialization.is_file());
            let mut clock =
                super::super::timing::FragmentClock::read(&representation.initialization).unwrap();
            let mut original = fs::read(&representation.initialization).unwrap();
            for (index, (resource, boundary)) in representation.resources.iter().enumerate() {
                assert!(resource.starts_with(root));
                assert_eq!(boundary.number, u64::try_from(index + 1).unwrap());
                assert!(boundary.duration > 0 && boundary.timescale > 0);
                let (time, duration, scale) = clock.dash_segment(resource).unwrap();
                assert_eq!(
                    u128::from(time) * u128::from(boundary.timescale),
                    u128::from(boundary.time) * u128::from(scale)
                );
                assert_eq!(
                    u128::from(duration) * u128::from(boundary.timescale),
                    u128::from(boundary.duration) * u128::from(scale)
                );
                if index > 0 {
                    let previous = representation.resources[index - 1].1;
                    assert_eq!(boundary.time, previous.time + previous.duration);
                }
                original.extend(fs::read(resource).unwrap());
            }
            assert_eq!(original, fs::read(&track.source).unwrap());
            let (first, end, scale) = clock.presentation_window().unwrap();
            assert_eq!(representation.window.first, (first, scale));
            assert_eq!(representation.window.end, (end, scale));
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
        let stage = super::super::files::Stage::new(None).unwrap();
        let tracks = snapshot_dash(&manifest, stage.path()).unwrap();
        let segment_stage = super::super::files::Stage::new(None).unwrap();
        let segmented = snapshot_segments(
            &manifest,
            segment_stage.path(),
            &crate::concurrency::Stop::new(),
        )
        .unwrap();
        assert_eq!(segmented.representations.len(), 2);
        assert!(segmented.manifest.is_file());
        assert_eq!(segmented.report.tracks.len(), 2);
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
        assert_segment_snapshot(&tracks, &segmented, segment_stage.path());
        assert!(!segment_stage.path().join("dash-track-0.mp4").exists());
        assert!(!segment_stage.path().join("dash-track-1.mp4").exists());
        assert_eq!(
            std::fs::read_dir(&media).unwrap().count(),
            3,
            "snapshot never writes beside source media"
        );
        let original = fs::read_to_string(&manifest).unwrap();
        for (close, declaration) in [
            (
                "</Period>",
                "<EventStream schemeIdUri=\"urn:example:event\" timescale=\"1\"><Event presentationTime=\"1\" duration=\"1\" id=\"1\"/></EventStream>",
            ),
            (
                "</Representation>",
                "<InbandEventStream schemeIdUri=\"urn:example:event\"/>",
            ),
        ] {
            fs::write(
                &manifest,
                original.replacen(close, &format!("{declaration}{close}"), 1),
            )
            .unwrap();
            let denied = super::super::files::Stage::new(None).unwrap();
            assert_eq!(
                snapshot_dash(&manifest, denied.path()).unwrap_err().0,
                "dash-presentation-metadata-not-supported"
            );
            let denied_segments = super::super::files::Stage::new(None).unwrap();
            let Err(error) = snapshot_segments(
                &manifest,
                denied_segments.path(),
                &crate::concurrency::Stop::new(),
            ) else {
                panic!("unsupported metadata was accepted");
            };
            assert_eq!(error.0, "dash-presentation-metadata-not-supported");
        }
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
            let stage = super::super::files::Stage::new(None).unwrap();
            let error =
                snapshot_dash(&manifest, stage.path()).expect_err("unsupported DASH must fail");
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

// The qualified static profile uses finite seconds, with at most nanosecond precision.
