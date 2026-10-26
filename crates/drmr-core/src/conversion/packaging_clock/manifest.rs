//! Bind normalized packaging paths before restoring any final fragment metadata.
use super::{
    BAD, CHANGED, ConversionError, LIMIT, files,
    metadata::{self, Budget, Plan, Track},
    owned::BoundFile,
};
use crate::{
    concurrency::Stop,
    inspection::{OutputFormat, TrackKind},
};
use roxmltree::{Document, Node};
use std::{
    collections::BTreeSet,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const NS: &str = "urn:mpeg:dash:schema:mpd:2011";
pub(super) struct Output<'a> {
    pub file: BoundFile<'a>,
    pub plan: Plan,
}
pub(super) struct Manifest<'a> {
    file: BoundFile<'a>,
    before: Zeroizing<Vec<u8>>,
    pub relative: Zeroizing<String>,
}

fn read(file: &BoundFile<'_>) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    let mut bytes = Zeroizing::new(Vec::new());
    file.reader()?
        .take(crate::inspection::MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| BAD)?;
    if bytes.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(LIMIT);
    }
    file.check()?;
    Ok(bytes)
}
fn segment(name: &str) -> Option<u64> {
    let number = name.strip_prefix("seg_")?.strip_suffix(".m4s")?;
    if number.is_empty() || number.len() > 20 || !number.bytes().all(|v| v.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

fn package_entries(
    owner: &files::Stage,
    root: &Path,
    count: usize,
    format: OutputFormat,
    stop: &Stop,
) -> Result<(), ConversionError> {
    super::owned::directory(owner, root)?;
    let mut expected: BTreeSet<_> = (0..count).map(|n| format!("track-{n:04}")).collect();
    expected.insert(
        if format == OutputFormat::Dash {
            "manifest.mpd"
        } else {
            "master.m3u8"
        }
        .into(),
    );
    let mut seen = BTreeSet::new();
    for entry in std::fs::read_dir(root).map_err(|_| BAD)? {
        metadata::checkpoint(stop)?;
        let entry = entry.map_err(|_| BAD)?;
        let name = entry.file_name().into_string().map_err(|_| BAD)?;
        if !expected.contains(&name) || !seen.insert(name) {
            return Err(BAD);
        }
    }
    if seen != expected {
        return Err(BAD);
    }
    Ok(())
}

fn resources(
    owner: &files::Stage,
    root: &Path,
    ordinal: usize,
    format: OutputFormat,
    stop: &Stop,
) -> Result<Vec<PathBuf>, ConversionError> {
    let directory = root.join(format!("track-{ordinal:04}"));
    super::owned::directory(owner, &directory)?;
    let mut paths = Vec::new();
    let mut numbers = BTreeSet::new();
    let mut init = false;
    let mut playlist = false;
    let mut retained = 0usize;
    for entry in std::fs::read_dir(&directory).map_err(|_| BAD)? {
        metadata::checkpoint(stop)?;
        let entry = entry.map_err(|_| BAD)?;
        let name = entry.file_name().into_string().map_err(|_| BAD)?;
        if name == "init.mp4" {
            init = true;
        } else if name == "media.m3u8" {
            playlist = true;
        } else if let Some(number) = segment(&name) {
            if !numbers.insert(number) {
                return Err(BAD);
            }
            let path = entry.path();
            retained = retained
                .checked_add(path.as_os_str().len() + std::mem::size_of::<PathBuf>() + 32)
                .filter(|n| *n <= 8 * 1024 * 1024)
                .ok_or(LIMIT)?;
            paths.push(path);
            if paths.len() > 100_000 {
                return Err(LIMIT);
            }
        } else {
            return Err(BAD);
        }
    }
    if !init || paths.is_empty() || (format == OutputFormat::Hls && !playlist) {
        return Err(BAD);
    }
    paths.sort_by_key(|p| p.file_name().and_then(|n| n.to_str()).and_then(segment));
    if format == OutputFormat::Hls {
        let bound = BoundFile::open(owner, &directory.join("media.m3u8"))?;
        let bytes = read(&bound)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| BAD)?;
        let declared: BTreeSet<_> = text
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect();
        let actual: BTreeSet<_> = paths
            .iter()
            .map(|p| p.file_name().and_then(|n| n.to_str()).ok_or(BAD))
            .collect::<Result<_, _>>()?;
        if !text.starts_with("#EXTM3U")
            || !text.contains("#EXT-X-ENDLIST")
            || !text
                .lines()
                .any(|line| line == "#EXT-X-MAP:URI=\"init.mp4\"")
            || declared != actual
        {
            return Err(BAD);
        }
    }
    Ok(paths)
}

pub(super) fn outputs<'a>(
    owner: &'a files::Stage,
    root: &Path,
    tracks: &[Option<Track>],
    kinds: &[TrackKind],
    format: OutputFormat,
    stop: &Stop,
) -> Result<Vec<Output<'a>>, ConversionError> {
    if !matches!(format, OutputFormat::Dash | OutputFormat::Hls) {
        return Err(BAD);
    }
    package_entries(owner, root, tracks.len(), format, stop)?;
    let mut budget = Budget::default();
    let mut output = Vec::new();
    let mut total = 0_usize;
    for (ordinal, (source, kind)) in tracks.iter().zip(kinds).enumerate() {
        let paths = resources(owner, root, ordinal, format, stop)?;
        total = total
            .checked_add(paths.len())
            .filter(|n| *n <= 100_000)
            .ok_or(LIMIT)?;
        if *kind != TrackKind::Audio || source.is_none_or(|track| track.first == 0) {
            continue;
        }
        let source = source.ok_or(BAD)?;
        let init = BoundFile::open(owner, &root.join(format!("track-{ordinal:04}/init.mp4")))?;
        let actual = metadata::initialization(&init, &mut budget, stop)?;
        let numerator = u128::from(source.first) * u128::from(actual.scale);
        if numerator % u128::from(source.scale) != 0 {
            return Err(BAD);
        }
        let delta = u64::try_from(numerator / u128::from(source.scale)).map_err(|_| BAD)?;
        for path in paths {
            budget.retain(path.as_os_str().len() + std::mem::size_of::<Output<'_>>())?;
            let mut file = BoundFile::open(owner, &path)?;
            let plan = metadata::restore(&file, actual, delta, &mut budget, stop)?;
            file.suspend();
            output.push(Output { file, plan });
        }
    }
    Ok(output)
}

fn children<'a>(node: Node<'a, 'a>, name: &str) -> Vec<Node<'a, 'a>> {
    node.children()
        .filter(|node| node.has_tag_name((NS, name)))
        .collect()
}
fn one<'a>(node: Node<'a, 'a>, name: &str) -> Result<Node<'a, 'a>, ConversionError> {
    let nodes = children(node, name);
    if nodes.len() != 1 {
        return Err(BAD);
    }
    Ok(nodes[0])
}
fn attribute(
    text: &str,
    node: Node<'_, '_>,
    name: &str,
    value: u64,
    edits: &mut Vec<(std::ops::Range<usize>, String)>,
) -> Result<(), ConversionError> {
    if let Some(attr) = node.attribute_node(name) {
        edits.push((attr.range_value(), value.to_string()));
    } else {
        let range = node.range();
        let opening = &text[range.clone()];
        let at = range.start
            + opening
                .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
                .ok_or(BAD)?;
        edits.push((at..at, format!(" {name}=\"{value}\"")));
    }
    Ok(())
}

pub(super) fn dash<'a>(
    owner: &'a files::Stage,
    root: &Path,
    tracks: &[Option<Track>],
    stop: &Stop,
) -> Result<Manifest<'a>, ConversionError> {
    let file = BoundFile::open(owner, &root.join("manifest.mpd"))?;
    let before = read(&file)?;
    super::super::dash::source_references(&before)?;
    let text = std::str::from_utf8(&before).map_err(|_| BAD)?;
    let document = Document::parse_with_options(
        text,
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| BAD)?;
    let mpd = document.root_element();
    if !mpd.has_tag_name((NS, "MPD")) {
        return Err(BAD);
    }
    let period = one(mpd, "Period")?;
    let reps: Vec<_> = children(period, "AdaptationSet")
        .into_iter()
        .flat_map(|set| children(set, "Representation"))
        .collect();
    if reps.len() != tracks.len()
        || document
            .descendants()
            .filter(|node| node.is_element() && node.tag_name().name() == "Representation")
            .count()
            != reps.len()
    {
        return Err(BAD);
    }
    let mut seen = BTreeSet::new();
    let mut edits = Vec::new();
    for rep in reps {
        metadata::checkpoint(stop)?;
        let template = one(rep, "SegmentTemplate")?;
        let init = template.attribute("initialization").ok_or(BAD)?;
        let ordinal = (0..tracks.len())
            .find(|n| init == format!("track-{n:04}/init.mp4"))
            .ok_or(BAD)?;
        if !seen.insert(ordinal)
            || template.attribute("media")
                != Some(format!("track-{ordinal:04}/seg_$Number$.m4s").as_str())
        {
            return Err(BAD);
        }
        let scale = template
            .attribute("timescale")
            .unwrap_or("1")
            .parse::<u64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or(BAD)?;
        let delta = tracks[ordinal].map_or(Ok(0), |track| {
            let value = u128::from(track.first) * u128::from(scale);
            if value % u128::from(track.scale) != 0 {
                return Err(BAD);
            }
            u64::try_from(value / u128::from(track.scale)).map_err(|_| BAD)
        })?;
        let samples = children(one(template, "SegmentTimeline")?, "S");
        if samples.is_empty() {
            return Err(BAD);
        }
        if delta != 0 {
            for (index, node) in samples.iter().enumerate() {
                if let Some(raw) = node.attribute("t") {
                    let value = raw
                        .parse::<u64>()
                        .map_err(|_| BAD)?
                        .checked_add(delta)
                        .ok_or(BAD)?;
                    attribute(text, *node, "t", value, &mut edits)?;
                } else if index == 0 {
                    attribute(text, *node, "t", delta, &mut edits)?;
                }
            }
        }
        // The packaging tool may have selected the still-absolute video clock as a shared origin.
        attribute(text, template, "presentationTimeOffset", 0, &mut edits)?;
    }
    edits.sort_by_key(|(range, _)| std::cmp::Reverse((range.start, range.end)));
    let mut relative = Zeroizing::new(text.to_owned());
    let mut boundary = text.len();
    for (range, value) in edits {
        metadata::checkpoint(stop)?;
        if range.end > boundary {
            return Err(BAD);
        }
        boundary = range.start;
        relative.replace_range(range, &value);
    }
    if relative.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(LIMIT);
    }
    Ok(Manifest {
        file,
        before,
        relative,
    })
}

impl Manifest<'_> {
    pub(super) fn finish(mut self, root: &Path, stop: &Stop) -> Result<(), ConversionError> {
        metadata::checkpoint(stop)?;
        let finalized = Zeroizing::new(super::super::dash::finish_transport_projection(
            self.relative.as_bytes(),
            root,
            stop,
        )?);
        if finalized.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
            return Err(LIMIT);
        }
        self.file.update(|writer| {
            metadata::checkpoint(stop)?;
            writer.seek(SeekFrom::Start(0)).map_err(|_| BAD)?;
            let mut current = Zeroizing::new(Vec::new());
            (&mut *writer)
                .take(crate::inspection::MAX_MANIFEST_BYTES + 1)
                .read_to_end(&mut current)
                .map_err(|_| BAD)?;
            if current.as_slice() != self.before.as_slice() {
                return Err(CHANGED);
            }
            writer.seek(SeekFrom::Start(0)).map_err(|_| BAD)?;
            writer.write_all(finalized.as_bytes()).map_err(|_| BAD)?;
            writer.set_len(finalized.len() as u64).map_err(|_| BAD)?;
            Ok(())
        })
    }
}
