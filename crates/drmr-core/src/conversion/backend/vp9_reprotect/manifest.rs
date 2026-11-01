//! Update only codec attributes selected by the produced manifest's resource graph.
use super::{BAD, Rendition};
use crate::{
    concurrency::Stop,
    conversion::{ConversionError, files, hls},
    inspection::OutputFormat,
    native,
};
use std::{
    collections::BTreeMap,
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    ops::Range,
    path::{Path, PathBuf},
};

pub(super) fn text(path: &Path) -> Result<String, ConversionError> {
    String::from_utf8(files::read_manifest(path)?).map_err(|_| BAD)
}

fn hls_row(playlist: PathBuf) -> Result<Rendition, ConversionError> {
    let parent = playlist.parent().ok_or(BAD)?;
    let mut initialization = None;
    let mut resources = Vec::new();
    for line in text(&playlist)?.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
            let attrs = hls::attributes(value)?;
            if initialization.is_some() || attrs.get("BYTERANGE").is_some() {
                return Err(BAD);
            }
            initialization = Some(hls::local_resource(parent, attrs.get("URI").ok_or(BAD)?)?);
        } else if !line.is_empty() && !line.starts_with('#') {
            if resources.len() == 4096 {
                return Err(ConversionError("hls-resource-limit"));
            }
            resources.push(hls::local_resource(parent, line)?);
        }
    }
    if resources.is_empty() {
        return Err(BAD);
    }
    Ok(Rendition {
        initialization: initialization.ok_or(BAD)?,
        resources,
        playlist: Some(playlist),
    })
}

pub(super) fn graph(
    root: &Path,
    format: OutputFormat,
    stop: &Stop,
) -> Result<Vec<Rendition>, ConversionError> {
    match format {
        OutputFormat::Hls => {
            crate::conversion::presentation::hls_media_paths(&root.join("master.m3u8"))?
                .into_iter()
                .map(|media| hls_row(media.path))
                .collect()
        }
        OutputFormat::Dash => {
            crate::conversion::dash::layout_output_resources(&root.join("manifest.mpd"), stop)?
                .into_iter()
                .map(|row| {
                    let (first, rest) = row.split_first().ok_or(BAD)?;
                    if rest.is_empty() {
                        return Err(BAD);
                    }
                    Ok(Rendition {
                        initialization: root.join(first),
                        resources: rest.iter().map(|path| root.join(path)).collect(),
                        playlist: None,
                    })
                })
                .collect()
        }
        _ => Err(BAD),
    }
}

fn update(path: &Path, original: &str, value: &str, stop: &Stop) -> Result<(), ConversionError> {
    if value.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(BAD);
    }
    crate::conversion::private_file_mode(path)?;
    let mut file = native::open_media_update(path).map_err(|_| BAD)?;
    let mut held = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(crate::inspection::MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut held)
        .map_err(|_| BAD)?;
    if held != original.as_bytes() {
        return Err(ConversionError("native-source-changed"));
    }
    if stop.is_set() {
        return Err(ConversionError("cancelled"));
    }
    file.seek(SeekFrom::Start(0)).map_err(|_| BAD)?;
    file.write_all(value.as_bytes()).map_err(|_| BAD)?;
    file.set_len(u64::try_from(value.len()).map_err(|_| BAD)?)
        .map_err(|_| BAD)?;
    file.sync_all().map_err(|_| BAD)
}

fn edited(source: &str, mut edits: Vec<(Range<usize>, String)>) -> Result<String, ConversionError> {
    edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
    let mut result = source.to_owned();
    let mut end = source.len();
    for (range, value) in edits {
        if range.end > end || range.start > range.end {
            return Err(BAD);
        }
        end = range.start;
        result.replace_range(range, &value);
    }
    Ok(result)
}

// Locate a value after the ordinary HLS attribute parser has validated its syntax.
fn attribute_range(value: &str, wanted: &str) -> Result<Range<usize>, ConversionError> {
    let attrs = hls::attributes(value)?;
    let expected = attrs.get(wanted).ok_or(BAD)?;
    let mut at = 0;
    while at < value.len() {
        let equal = at + value[at..].find('=').ok_or(BAD)?;
        let name = value[at..equal].trim();
        let mut start = equal + 1;
        let quoted = value.as_bytes().get(start) == Some(&b'"');
        if quoted {
            start += 1;
        }
        let end = start
            + value[start..]
                .find(if quoted { '"' } else { ',' })
                .unwrap_or(value.len() - start);
        if name == wanted {
            if value.get(start..end) != Some(expected) {
                return Err(BAD);
            }
            return Ok(start..end);
        }
        at = end + usize::from(quoted);
        if at < value.len() {
            if value.as_bytes()[at] != b',' {
                return Err(BAD);
            }
            at += 1;
        }
    }
    Err(BAD)
}

fn hls_codecs(
    root: &Path,
    codecs: &BTreeMap<PathBuf, String>,
    stop: &Stop,
) -> Result<(), ConversionError> {
    let path = root.join("master.m3u8");
    let source = text(&path)?;
    let mut edits = Vec::new();
    let mut pending = None;
    let mut offset = 0;
    for raw in source.split_inclusive('\n') {
        let line = raw.trim();
        let start = offset + raw.len() - raw.trim_start().len();
        if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            if pending.is_some() {
                return Err(BAD);
            }
            let range = attribute_range(value, "CODECS")?;
            let base = start + "#EXT-X-STREAM-INF:".len();
            pending = Some((
                base + range.start..base + range.end,
                value[range].to_owned(),
            ));
        } else if !line.is_empty() && !line.starts_with('#') {
            let (range, old) = pending.take().ok_or(BAD)?;
            let playlist = hls::local_resource(root, line)?;
            if let Some(codec) = codecs.get(&playlist) {
                let mut count = 0;
                let values = old
                    .split(',')
                    .map(|value| {
                        if value.trim().starts_with("vp09.") {
                            count += 1;
                            codec.as_str()
                        } else {
                            value
                        }
                    })
                    .collect::<Vec<_>>();
                if count != 1 {
                    return Err(BAD);
                }
                edits.push((range, values.join(",")));
            }
        }
        offset += raw.len();
    }
    if pending.is_some() {
        return Err(BAD);
    }
    update(&path, &source, &edited(&source, edits)?, stop)
}

fn set_codec(
    node: roxmltree::Node<'_, '_>,
    source: &str,
    value: &str,
    edits: &mut Vec<(Range<usize>, String)>,
) -> Result<(), ConversionError> {
    if let Some(attribute) = node.attribute_node("codecs") {
        edits.push((attribute.range_value(), value.to_owned()));
    } else {
        let start = node.range().start;
        let close = start + source[start..].find('>').ok_or(BAD)?;
        let at = if source.as_bytes().get(close.wrapping_sub(1)) == Some(&b'/') {
            close - 1
        } else {
            close
        };
        edits.push((at..at, format!(" codecs=\"{value}\"")));
    }
    Ok(())
}

fn dash_codecs(
    root: &Path,
    rows: &[Rendition],
    codecs: &[Option<String>],
    stop: &Stop,
) -> Result<(), ConversionError> {
    let path = root.join("manifest.mpd");
    let source = text(&path)?;
    let document = roxmltree::Document::parse_with_options(
        &source,
        roxmltree::ParsingOptions {
            nodes_limit: 20_000,
            ..Default::default()
        },
    )
    .map_err(|_| BAD)?;
    let mut index = 0_usize;
    let mut edits = Vec::new();
    for set in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "AdaptationSet")
    {
        let representations = set
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "Representation")
            .collect::<Vec<_>>();
        let first = index;
        index = index
            .checked_add(representations.len())
            .filter(|end| *end <= rows.len())
            .ok_or(BAD)?;
        let selected = &codecs[first..index];
        if selected.iter().all(Option::is_none) {
            continue;
        }
        let mut actual = Vec::with_capacity(representations.len());
        for (representation, codec) in representations.iter().zip(selected) {
            actual.push(
                codec
                    .as_deref()
                    .or_else(|| representation.attribute("codecs"))
                    .or_else(|| set.attribute("codecs"))
                    .ok_or(BAD)?,
            );
        }
        if let Some(attribute) = set.attribute_node("codecs") {
            if actual.windows(2).all(|pair| pair[0] == pair[1]) {
                edits.push((attribute.range_value(), actual[0].to_owned()));
            } else {
                edits.push((attribute.range(), String::new()));
            }
        }
        for (representation, value) in representations.into_iter().zip(actual) {
            set_codec(representation, &source, value, &mut edits)?;
        }
    }
    if index != rows.len() || codecs.len() != rows.len() {
        return Err(BAD);
    }
    update(&path, &source, &edited(&source, edits)?, stop)
}

pub(super) fn finish(
    root: &Path,
    format: OutputFormat,
    rows: &[Rendition],
    codecs: &[Option<String>],
    stop: &Stop,
) -> Result<(), ConversionError> {
    match format {
        OutputFormat::Hls => {
            let selected = rows
                .iter()
                .zip(codecs)
                .filter_map(|(row, codec)| Some((row.playlist.as_ref()?.clone(), codec.clone()?)))
                .collect();
            hls_codecs(root, &selected, stop)
        }
        OutputFormat::Dash => dash_codecs(root, rows, codecs, stop),
        _ => Err(BAD),
    }
}
