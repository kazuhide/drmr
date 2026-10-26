//! Rewrite final, privately staged resources into the explicitly selected DASH addressing form.
use super::{
    ConversionError, DASH_NS, Document, MAX_SEGMENTS, MAX_TRACKS, Path, PathBuf,
    addressing::{self, Kind, Representation, Resource, Segment},
    children, error, index, one, reference,
};
use crate::{
    concurrency::Stop,
    conversion::{
        dash_addressing::DashAddressing, files, layout::path_uri as uri, timing::FragmentClock,
    },
    native::FileIdentity,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    io::{Read, Write},
};
use zeroize::Zeroizing;
const FULL_PROFILE: &str = "urn:mpeg:dash:profile:full:2011";

pub(super) fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn relative_resource(
    resolved: &Representation,
    resource: &Resource,
) -> Result<PathBuf, ConversionError> {
    Ok(resolved.base.join(super::relative(&resource.uri)?))
}
pub(super) fn prefix(rep: super::Node<'_, '_>) -> Result<String, ConversionError> {
    if rep.tag_name().namespace().is_none() || rep.lookup_namespace_uri(None) == Some(DASH_NS) {
        Ok(String::new())
    } else {
        rep.lookup_prefix(DASH_NS)
            .filter(|v| !v.is_empty())
            .map(|v| format!("{v}:"))
            .ok_or(error("dash-invalid-xml"))
    }
}
pub(super) fn timeline(
    text: &mut String,
    prefix: &str,
    segments: &[Segment],
) -> Result<(), ConversionError> {
    write!(text, "<{prefix}SegmentTimeline>").map_err(|_| error("output-staging-failed"))?;
    for segment in segments {
        write!(
            text,
            "<{prefix}S t=\"{}\" d=\"{}\"/>",
            segment.time, segment.duration
        )
        .map_err(|_| error("output-staging-failed"))?;
    }
    write!(text, "</{prefix}SegmentTimeline>").map_err(|_| error("output-staging-failed"))
}
pub(super) fn common(kind: &str, resolved: &Representation, prefix: &str) -> String {
    format!(
        "<{prefix}{kind} timescale=\"{}\" presentationTimeOffset=\"{}\" startNumber=\"{}\"",
        resolved.timescale, resolved.offset, resolved.segments[0].number
    )
}

fn list(resolved: &Representation, prefix: &str) -> Result<String, ConversionError> {
    let mut text = common("SegmentList", resolved, prefix);
    text.push('>');
    write!(
        text,
        "<{prefix}Initialization sourceURL=\"{}\"",
        xml(&uri(&relative_resource(
            resolved,
            &resolved.initialization
        )?)?)
    )
    .map_err(|_| error("output-staging-failed"))?;
    if let Some(range) = resolved.initialization.range {
        write!(text, " range=\"{}-{}\"", range.start, range.end)
            .map_err(|_| error("output-staging-failed"))?;
    }
    text.push_str("/>");
    timeline(&mut text, prefix, &resolved.segments)?;
    for segment in &resolved.segments {
        write!(
            text,
            "<{prefix}SegmentURL media=\"{}\"",
            xml(&uri(&relative_resource(resolved, &segment.resource)?)?)
        )
        .map_err(|_| error("output-staging-failed"))?;
        if let Some(range) = segment.resource.range {
            write!(text, " mediaRange=\"{}-{}\"", range.start, range.end)
                .map_err(|_| error("output-staging-failed"))?;
        }
        text.push_str("/>");
    }
    write!(text, "</{prefix}SegmentList>").map_err(|_| error("output-staging-failed"))?;
    Ok(text)
}

fn template_duration(
    rep: super::Node<'_, '_>,
    root: &Path,
    resolved: &Representation,
    prefix: &str,
    millis: Option<u32>,
    stop: &Stop,
) -> Result<String, ConversionError> {
    let (_, _, nodes) = addressing::effective(rep)?;
    let media = super::attribute(&nodes, "media").ok_or(error("dash-media-template-required"))?;
    // A time template would name different files after the MPD clock or nominal timing changes.
    if media.contains("$Time") {
        return Err(error("dash-nominal-time-template-not-supported"));
    }
    let output = super::nominal::output(root, resolved, addressing::period(rep)?, millis, stop)?;
    let mut text = format!(
        "<{prefix}SegmentTemplate timescale=\"{}\" presentationTimeOffset=\"{}\" startNumber=\"{}\" endNumber=\"{}\" presentationDuration=\"{}\" duration=\"{}\" initialization=\"{}\" media=\"{}\"",
        output.timescale,
        output.offset,
        resolved.segments[0].number,
        output.end_number,
        output.presentation_duration,
        output.duration,
        xml(&uri(&relative_resource(
            resolved,
            &resolved.initialization
        )?)?),
        xml(&addressing::template_uri(&resolved.base, media)?)
    );
    text.push_str("/>");
    Ok(text)
}

struct OwnedFile {
    path: PathBuf,
    identity: FileIdentity,
    length: u64,
}
fn owned(path: &Path) -> Result<OwnedFile, ConversionError> {
    let file = files::regular_input(path)?;
    Ok(OwnedFile {
        path: path.into(),
        identity: FileIdentity::read(&file).map_err(|_| error("output-staging-failed"))?,
        length: file
            .metadata()
            .map_err(|_| error("output-staging-failed"))?
            .len(),
    })
}
fn copy(source: &OwnedFile, target: &mut impl Write, stop: &Stop) -> Result<(), ConversionError> {
    let mut input = files::regular_input(&source.path)?;
    if FileIdentity::read(&input).map_err(|_| error("output-staging-failed"))? != source.identity
        || input
            .metadata()
            .map_err(|_| error("output-staging-failed"))?
            .len()
            != source.length
    {
        return Err(error("source-changed"));
    }
    let mut buffer = Zeroizing::new(vec![0; 65536]);
    let mut total = 0_u64;
    loop {
        super::checkpoint(Some(stop))?;
        let read = input
            .read(&mut buffer)
            .map_err(|_| error("media-copy-failed"))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .filter(|v| *v <= source.length)
            .ok_or(error("source-changed"))?;
        target
            .write_all(&buffer[..read])
            .map_err(|_| error("media-copy-failed"))?;
    }
    if total != source.length
        || FileIdentity::read(&files::regular_input(&source.path)?)
            .map_err(|_| error("source-changed"))?
            != source.identity
    {
        return Err(error("source-changed"));
    }
    Ok(())
}

struct Indexed {
    initialization: OwnedFile,
    media: Vec<OwnedFile>,
    index: Vec<u8>,
    scale: u64,
    offset: u64,
}
fn capture_indexed(
    root: &Path,
    resolved: &Representation,
    stop: &Stop,
) -> Result<Indexed, ConversionError> {
    let initialization = reference(root, &resolved.base, &resolved.initialization.uri)?;
    let mut clock = FragmentClock::read(&initialization)?;
    let track_id = clock.identity().0;
    let init = owned(&initialization)?;
    let mut media = Vec::with_capacity(resolved.segments.len());
    let mut items = Vec::with_capacity(resolved.segments.len());
    let mut first = None;
    let mut scale = None;
    let mut next = None;
    for segment in &resolved.segments {
        super::checkpoint(Some(stop))?;
        let path = reference(root, &resolved.base, &segment.resource.uri)?;
        let (declared, presented) = clock.dash_clocks(&path)?;
        if !clock.relocatable() {
            return Err(error("dash-indexed-absolute-fragment-offset-not-supported"));
        }
        if u128::from(declared.0) * u128::from(resolved.timescale)
            != u128::from(segment.time) * u128::from(declared.2)
            || u128::from(declared.1) * u128::from(resolved.timescale)
                != u128::from(segment.duration) * u128::from(declared.2)
        {
            return Err(error("manifest-fragment-timeline-mismatch"));
        }
        if !clock.starts_with_sync() {
            return Err(error("dash-index-sap-not-supported"));
        }
        if scale.is_some_and(|old| old != presented.2) || next.is_some_and(|old| old != presented.0)
        {
            return Err(error("dash-index-timeline-not-representable"));
        }
        first.get_or_insert(presented.0);
        scale = Some(presented.2);
        next = Some(presented.1);
        let source = owned(&path)?;
        items.push((
            source.length,
            presented
                .1
                .checked_sub(presented.0)
                .ok_or(error("dash-index-not-representable"))?,
            1,
        ));
        media.push(source);
    }
    clock.presentation_window()?;
    let scale = scale.ok_or(error("dash-empty-segment-timeline"))?;
    let index = index::build(
        track_id,
        u32::try_from(scale).map_err(|_| error("dash-index-timescale-not-supported"))?,
        first.ok_or(error("dash-empty-segment-timeline"))?,
        &items,
    )?;
    media.iter().try_fold(
        init.length
            .checked_add(index.len() as u64)
            .ok_or(error("dash-media-size-limit"))?,
        |total, source| {
            total
                .checked_add(source.length)
                .filter(|total| *total <= super::MAX_BYTES)
                .ok_or(error("dash-media-size-limit"))
        },
    )?;
    let offset = u128::from(resolved.offset)
        .checked_mul(u128::from(scale))
        .filter(|n| n % u128::from(resolved.timescale) == 0)
        .and_then(|n| u64::try_from(n / u128::from(resolved.timescale)).ok())
        .ok_or(error("dash-presentation-origin-timescale-not-supported"))?;
    Ok(Indexed {
        initialization: init,
        media,
        index,
        scale,
        offset,
    })
}

fn base(
    root: &Path,
    resolved: &Representation,
    prefix: &str,
    stop: &Stop,
    cleanup: &mut BTreeMap<PathBuf, OwnedFile>,
    targets: &mut BTreeSet<PathBuf>,
) -> Result<String, ConversionError> {
    let indexed = capture_indexed(root, resolved, stop)?;
    let relative = relative_resource(resolved, &resolved.initialization)?;
    let parent = relative.parent().ok_or(error("unsupported-path"))?;
    let stem = relative
        .file_stem()
        .and_then(|name| name.to_str())
        .ok_or(error("unsupported-path"))?;
    let relative = parent.join(if stem == "init" {
        "indexed.mp4".into()
    } else {
        format!("{stem}-indexed.mp4")
    });
    if !targets.insert(relative.clone()) {
        return Err(error("dash-indexed-output-collision"));
    }
    let mut output = files::create_new(&root.join(&relative))?;
    copy(&indexed.initialization, &mut output, stop)?;
    output
        .write_all(&indexed.index)
        .map_err(|_| error("media-copy-failed"))?;
    for source in &indexed.media {
        copy(source, &mut output, stop)?;
    }
    output.sync_all().map_err(|_| error("media-copy-failed"))?;
    drop(output);
    let length = indexed.initialization.length;
    let end = length
        .checked_sub(1)
        .ok_or(error("dash-index-not-representable"))?;
    let last = length
        .checked_add(indexed.index.len() as u64)
        .and_then(|n| n.checked_sub(1))
        .ok_or(error("dash-index-not-representable"))?;
    for source in std::iter::once(indexed.initialization).chain(indexed.media) {
        if let Some(previous) = cleanup.insert(source.path.clone(), source)
            && cleanup
                .get(&previous.path)
                .is_none_or(|value| value.identity != previous.identity)
        {
            return Err(error("source-changed"));
        }
    }
    Ok(format!(
        "<{prefix}BaseURL>{}</{prefix}BaseURL><{prefix}SegmentBase timescale=\"{}\" presentationTimeOffset=\"{}\" indexRange=\"{length}-{last}\" indexRangeExact=\"true\"><{prefix}Initialization range=\"0-{end}\"/></{prefix}SegmentBase>",
        xml(&uri(&relative)?),
        indexed.scale,
        indexed.offset
    ))
}

pub(in crate::conversion) fn rewrite_output(
    root: &Path,
    mode: DashAddressing,
    nominal_duration_ms: Option<u32>,
    stop: &Stop,
) -> Result<(), ConversionError> {
    if mode == DashAddressing::Template {
        return Ok(());
    }
    super::checkpoint(Some(stop))?;
    let manifest = root.join("manifest.mpd");
    let bytes = files::read_manifest(&manifest)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| error("dash-invalid-xml"))?;
    let document = Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| error("dash-invalid-xml"))?;
    let root_node = document.root_element();
    let period = one(root_node, "Period")?.ok_or(error("dash-single-period-required"))?;
    let reps: Vec<_> = children(period, "AdaptationSet")
        .into_iter()
        .flat_map(|set| children(set, "Representation"))
        .collect();
    if reps.is_empty() || reps.len() > MAX_TRACKS {
        return Err(error("dash-track-limit-or-structure-not-supported"));
    }
    let root = root
        .canonicalize()
        .map_err(|_| error("output-staging-failed"))?;
    let mut edits = Vec::new();
    if let Some(attribute) = root_node.attribute_node("profiles") {
        edits.push((attribute.range_value(), FULL_PROFILE.to_owned()));
    } else {
        let range = root_node.range();
        let offset = text[range.clone()]
            .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
            .ok_or(error("dash-invalid-xml"))?
            + range.start;
        edits.push((offset..offset, format!(" profiles=\"{FULL_PROFILE}\"")));
    }
    let mut removals = BTreeSet::new();
    let mut cleanup = BTreeMap::new();
    let mut targets = BTreeSet::new();
    let mut remaining = MAX_SEGMENTS;
    for rep in reps {
        super::checkpoint(Some(stop))?;
        let resolved = addressing::resolve(rep, remaining)?;
        if resolved.kind != Kind::Template
            || resolved.nominal.is_some()
            || resolved.index.is_some()
            || resolved.segments.is_empty()
            || resolved.initialization.range.is_some()
            || resolved
                .segments
                .iter()
                .any(|segment| segment.resource.range.is_some())
        {
            return Err(error("dash-addressing-source-not-supported"));
        }
        remaining = remaining.saturating_sub(resolved.segments.len());
        let prefix = prefix(rep)?;
        let node = match mode {
            DashAddressing::Template => unreachable!(),
            DashAddressing::TemplateDuration => {
                template_duration(rep, &root, &resolved, &prefix, nominal_duration_ms, stop)?
            }
            DashAddressing::List => list(&resolved, &prefix)?,
            DashAddressing::Base => {
                base(&root, &resolved, &prefix, stop, &mut cleanup, &mut targets)?
            }
        };
        replace_addressing(text, rep, node, &mut edits, &mut removals)?;
    }
    edits.extend(removals.into_iter().map(|(a, b)| (a..b, String::new())));
    let output = apply_edits(text, edits, stop)?;
    write_manifest(&manifest, &output)?;
    let retained: BTreeSet<_> = super::source_references(output.as_bytes())?
        .into_iter()
        .map(|p| root.join(p))
        .collect();
    for (_, source) in cleanup {
        super::checkpoint(Some(stop))?;
        if retained.contains(&source.path) {
            continue;
        }
        let current = owned(&source.path)?;
        if current.identity != source.identity || current.length != source.length {
            return Err(error("source-changed"));
        }
        std::fs::remove_file(&source.path).map_err(|_| error("output-staging-failed"))?;
    }
    Ok(())
}

pub(super) fn replace_addressing(
    text: &str,
    rep: super::Node<'_, '_>,
    node: String,
    edits: &mut Vec<(std::ops::Range<usize>, String)>,
    removals: &mut BTreeSet<(usize, usize)>,
) -> Result<(), ConversionError> {
    for scope in super::validated_scopes(rep)? {
        for name in ["BaseURL", "SegmentTemplate", "SegmentList", "SegmentBase"] {
            if let Some(node) = one(scope, name)? {
                let range = node.range();
                removals.insert((range.start, range.end));
            }
        }
    }
    let range = rep.range();
    let original = &text[range.clone()];
    if original.ends_with("/>") {
        let tag = original
            .strip_prefix('<')
            .and_then(|name| name.split([' ', '\n', '\r', '\t', '/', '>']).next())
            .ok_or(error("dash-invalid-xml"))?;
        edits.push((range.end - 2..range.end, format!(">{node}</{tag}>")));
    } else {
        let at = original.rfind("</").ok_or(error("dash-invalid-xml"))? + range.start;
        edits.push((at..at, node));
    }
    Ok(())
}

pub(super) fn apply_edits(
    text: &str,
    mut edits: Vec<(std::ops::Range<usize>, String)>,
    stop: &Stop,
) -> Result<Zeroizing<String>, ConversionError> {
    let mut length = text.len();
    for (range, value) in &edits {
        if range.start > range.end
            || range.end > text.len()
            || !text.is_char_boundary(range.start)
            || !text.is_char_boundary(range.end)
        {
            return Err(error("dash-invalid-xml"));
        }
        length = length
            .checked_sub(range.len())
            .and_then(|n| n.checked_add(value.len()))
            .ok_or(error("manifest-size-limit"))?;
    }
    if length as u64 > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(error("manifest-size-limit"));
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse((range.start, range.end)));
    let mut previous = text.len();
    let mut output = Zeroizing::new(text.to_owned());
    let additional = length.saturating_sub(output.len());
    output.reserve(additional);
    for (range, value) in edits {
        super::checkpoint(Some(stop))?;
        if range.end > previous {
            return Err(error("dash-overlapping-manifest-edits"));
        }
        previous = range.start;
        output.replace_range(range, &value);
    }
    super::source_references(output.as_bytes())?;
    Ok(output)
}

pub(super) fn write_manifest(path: &Path, output: &str) -> Result<(), ConversionError> {
    let mut file = crate::native::create_media_or_truncate(path)
        .map_err(|_| error("output-staging-failed"))?;
    file.write_all(output.as_bytes())
        .map_err(|_| error("output-staging-failed"))?;
    file.sync_all().map_err(|_| error("output-staging-failed"))
}
