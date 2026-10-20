//! Selection semantics are read from each actual manifest, independently of saved reports.
use super::{ConversionError, Path, Prepared, hls};
use crate::{inspection::OutputFormat, m3u8::Attributes};
use std::{fmt::Write as _, io::Write as _};
const BAD: ConversionError = ConversionError("presentation-metadata-mismatch");
const UNSUPPORTED: ConversionError = ConversionError("presentation-selection-not-representable");
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Origin {
    #[default]
    Container,
    Hls,
    Dash,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Metadata {
    pub origin: Origin,
    pub audio: Option<Rendition>,
    pub audio_group: Option<String>,
    pub resolution: Option<(u64, u64)>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Rendition {
    pub group: String,
    pub name: Option<String>,
    pub language: String,
    pub is_default: Option<bool>,
    pub autoselect: Option<bool>,
    pub channels: Option<String>,
}
fn text(value: &str) -> Result<String, ConversionError> {
    if value.is_empty()
        || value.len() > 256
        || value.chars().any(char::is_control)
        || value.contains('"')
    {
        return Err(BAD);
    }
    Ok(value.into())
}
fn flag(attrs: &Attributes, name: &str) -> Result<bool, ConversionError> {
    match attrs.get(name) {
        None | Some("NO") => Ok(false),
        Some("YES") => Ok(true),
        _ => Err(BAD),
    }
}
impl Rendition {
    pub fn hls(attrs: &Attributes) -> Result<Self, ConversionError> {
        if attrs.get("TYPE") != Some("AUDIO")
            || attrs.iter().any(|(name, _)| {
                !matches!(
                    name,
                    "TYPE"
                        | "URI"
                        | "GROUP-ID"
                        | "NAME"
                        | "DEFAULT"
                        | "AUTOSELECT"
                        | "LANGUAGE"
                        | "CHANNELS"
                )
            })
        {
            return Err(ConversionError("hls-rendition-metadata-not-supported"));
        }
        let is_default = flag(attrs, "DEFAULT")?;
        let autoselect = attrs
            .get("AUTOSELECT")
            .map(|_| flag(attrs, "AUTOSELECT"))
            .transpose()?;
        if is_default && autoselect == Some(false) {
            return Err(BAD);
        }
        let language = text(attrs.get("LANGUAGE").unwrap_or("und"))?;
        if !language
            .split('-')
            .all(|p| !p.is_empty() && p.len() <= 8 && p.bytes().all(|b| b.is_ascii_alphanumeric()))
        {
            return Err(BAD);
        }
        let channels = attrs.get("CHANNELS").map(text).transpose()?;
        if channels
            .as_ref()
            .is_some_and(|v| !matches!(v.as_str(), "1" | "2"))
        {
            return Err(BAD);
        }
        Ok(Self {
            group: text(attrs.get("GROUP-ID").ok_or(BAD)?)?,
            name: attrs.get("NAME").map(text).transpose()?,
            language,
            is_default: Some(is_default),
            autoselect,
            channels,
        })
    }
}
pub(super) fn variant(attrs: &Attributes) -> Result<Metadata, ConversionError> {
    let resolution = attrs
        .get("RESOLUTION")
        .map(|s| {
            let (w, h) = s.split_once('x').ok_or(BAD)?;
            let w = w.parse::<u64>().map_err(|_| BAD)?;
            let h = h.parse::<u64>().map_err(|_| BAD)?;
            if w == 0 || h == 0 {
                return Err(BAD);
            }
            Ok((w, h))
        })
        .transpose()?;
    Ok(Metadata {
        origin: Origin::Hls,
        audio_group: attrs.get("AUDIO").map(text).transpose()?,
        resolution,
        ..Default::default()
    })
}
pub(super) fn validate_profile(
    track: &Prepared,
    backend: &super::super::backend::Backend,
    key: Option<&super::ContentKey>,
) -> Result<(), ConversionError> {
    if let Some((width, height)) = track.metadata.resolution {
        let profile = backend.profile_stream(&track.media, track.index, key)?;
        if profile["width"].as_u64() != Some(width) || profile["height"].as_u64() != Some(height) {
            return Err(BAD);
        }
    }
    Ok(())
}
pub(super) fn dash_representable(tracks: &[Prepared]) -> bool {
    tracks.iter().filter(|t| t.metadata.audio.is_some()).count() <= 1
}
fn quoted(value: &str) -> Result<String, ConversionError> {
    Ok(format!("\"{}\"", text(value)?))
}
fn yes(value: bool) -> &'static str {
    if value { "YES" } else { "NO" }
}
fn write(path: &Path, value: &str) -> Result<(), ConversionError> {
    if value.len() as u64 > crate::inspection::MAX_MANIFEST_BYTES {
        return Err(BAD);
    }
    super::super::files::seal(path.parent().ok_or(BAD)?)?;
    let mut file = crate::native::create_media_or_truncate(path).map_err(|_| BAD)?;
    file.write_all(value.as_bytes()).map_err(|_| BAD)?;
    file.sync_all().map_err(|_| BAD)
}
fn ordinal(uri: &str, count: usize) -> Result<usize, ConversionError> {
    uri.strip_prefix("track-")
        .and_then(|v| v.strip_suffix("/media.m3u8"))
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|n| *n < count)
        .ok_or(BAD)
}
fn hls_finish(root: &Path, tracks: &[Prepared]) -> Result<(), ConversionError> {
    let path = root.join("master.m3u8");
    let bytes = super::super::files::read_manifest(&path)?;
    let source = std::str::from_utf8(&bytes).map_err(|_| BAD)?;
    let audio: Vec<_> = tracks
        .iter()
        .filter_map(|t| t.metadata.audio.as_ref())
        .collect();
    if audio.is_empty() {
        return Ok(());
    }
    if audio.len() != 1 {
        return Err(UNSUPPORTED);
    }
    let group = &audio[0].group;
    let mut result = Vec::new();
    for line in source.lines() {
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attrs = hls::attributes(value)?;
            let index = ordinal(attrs.get("URI").ok_or(BAD)?, tracks.len())?;
            let a = tracks[index].metadata.audio.as_ref().ok_or(BAD)?;
            let name = a.name.as_deref().unwrap_or("Audio");
            let mut declaration = format!(
                "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID={},NAME={},LANGUAGE={},DEFAULT={}",
                quoted(&a.group)?,
                quoted(name)?,
                quoted(&a.language)?,
                yes(a.is_default.unwrap_or(false)),
            );
            if let Some(autoselect) = a.autoselect {
                write!(declaration, ",AUTOSELECT={}", yes(autoselect)).map_err(|_| BAD)?;
            }
            write!(
                declaration,
                ",URI={}",
                quoted(attrs.get("URI").ok_or(BAD)?)?
            )
            .map_err(|_| BAD)?;
            if let Some(channels) = &a.channels {
                write!(declaration, ",CHANNELS={}", quoted(channels)?).map_err(|_| BAD)?;
            }
            result.push(declaration);
        } else if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = hls::attributes(value)?;
            let mut values = Vec::new();
            for (name, value) in attrs.iter() {
                if name == "AUDIO" {
                    values.push(format!("AUDIO={}", quoted(group)?));
                } else if matches!(name, "CODECS" | "CLOSED-CAPTIONS") && value != "NONE" {
                    values.push(format!("{name}={}", quoted(value)?));
                } else {
                    values.push(format!("{name}={value}"));
                }
            }
            result.push(format!("#EXT-X-STREAM-INF:{}", values.join(",")));
        } else {
            result.push(line.into());
        }
    }
    write(&path, &format!("{}\n", result.join("\n")))
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn dash_finish(root: &Path, tracks: &[Prepared]) -> Result<(), ConversionError> {
    if !dash_representable(tracks) {
        return Err(UNSUPPORTED);
    }
    if tracks.iter().all(|t| t.metadata.audio.is_none()) {
        return Ok(());
    }
    let path = root.join("manifest.mpd");
    let bytes = super::super::files::read_manifest(&path)?;
    let source = std::str::from_utf8(&bytes).map_err(|_| BAD)?;
    let doc = roxmltree::Document::parse(source).map_err(|_| BAD)?;
    let mut edits = Vec::new();
    for adaptation in doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "AdaptationSet")
    {
        let representations: Vec<_> = adaptation
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "Representation")
            .collect();
        let mut members = Vec::new();
        for rep in representations {
            let template = rep
                .children()
                .find(|n| n.is_element() && n.tag_name().name() == "SegmentTemplate")
                .ok_or(BAD)?;
            let init = template.attribute("initialization").ok_or(BAD)?;
            let index = init
                .strip_prefix("track-")
                .and_then(|s| s.split_once('/'))
                .and_then(|(s, _)| s.parse::<usize>().ok())
                .filter(|n| *n < tracks.len())
                .ok_or(BAD)?;
            members.push(index);
        }
        let a = members
            .first()
            .and_then(|n| tracks[*n].metadata.audio.as_ref());
        if let Some(a) = a {
            if members.len() != 1 {
                return Err(UNSUPPORTED);
            }
            let at = adaptation.range().start + source[adaptation.range()].find('>').ok_or(BAD)?;
            let priority = if a.is_default.unwrap_or(false) { 2 } else { 1 };
            for (name, value) in [
                ("lang", xml(&a.language)),
                ("selectionPriority", priority.to_string()),
            ] {
                if let Some(attr) = adaptation.attribute_node(name) {
                    edits.push((attr.range_value(), value));
                } else {
                    edits.push((at..at, format!(" {name}=\"{value}\"")));
                }
            }
            if let Some(name) = &a.name {
                edits.push((at + 1..at + 1, format!("<Label>{}</Label>", xml(name))));
            }
        }
    }
    edits.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
    let mut result = source.to_owned();
    for (range, value) in edits {
        result.replace_range(range, &value);
    }
    write(&path, &result)
}
pub(super) fn finish(
    root: &Path,
    format: OutputFormat,
    tracks: &[Prepared],
) -> Result<(), ConversionError> {
    match format {
        OutputFormat::Dash => dash_finish(root, tracks),
        OutputFormat::Hls | OutputFormat::HlsTs => hls_finish(root, tracks),
        OutputFormat::Mp4 => Ok(()),
    }
}
/// Supported DASH selection declarations use only standard language, labels and author priority.
pub(super) fn dash(rep: roxmltree::Node<'_, '_>) -> Result<Metadata, ConversionError> {
    let mut language = None;
    let mut name = None;
    let mut priority = None;
    for scope in rep
        .ancestors()
        .filter(roxmltree::Node::is_element)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
    {
        if let Some(value) = scope.attribute("lang") {
            language = Some(text(value)?);
        }
        if let Some(value) = scope.attribute("selectionPriority") {
            priority = Some(match value {
                "1" => false,
                "2" => true,
                _ => return Err(UNSUPPORTED),
            });
        }
        let labels: Vec<_> = scope
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "Label")
            .collect();
        if labels.len() > 1 {
            return Err(UNSUPPORTED);
        }
        if let Some(label) = labels.first() {
            if label.attributes().len() > 0 || label.children().any(|n| n.is_element()) {
                return Err(UNSUPPORTED);
            }
            name = Some(text(label.text().ok_or(BAD)?)?);
        }
        for role in scope
            .children()
            .filter(|n| n.is_element() && n.tag_name().name() == "Role")
        {
            if role.attribute("schemeIdUri") != Some("urn:mpeg:dash:role:2011")
                || role.attribute("value") != Some("main")
            {
                return Err(UNSUPPORTED);
            }
        }
    }
    let is_audio = rep.ancestors().any(|n| {
        n.attribute("contentType") == Some("audio") || n.attribute("mimeType") == Some("audio/mp4")
    });
    let mut result = Metadata {
        origin: Origin::Dash,
        ..Default::default()
    };
    if is_audio
        && (name.is_some() || priority.is_some() || language.as_deref().is_some_and(|s| s != "und"))
    {
        result.audio = Some(Rendition {
            group: "default-audio-group".into(),
            name,
            language: language.unwrap_or("und".into()),
            is_default: priority,
            autoselect: Some(true),
            channels: None,
        });
    }
    Ok(result)
}
pub(super) fn pair(a: &Metadata, b: &Metadata) -> Result<(), ConversionError> {
    if let Some(source) = &a.audio {
        let target = b.audio.as_ref().ok_or(BAD)?;
        if source.language != target.language
            || source
                .name
                .as_ref()
                .is_some_and(|v| target.name.as_ref() != Some(v))
            || source
                .is_default
                .is_some_and(|v| target.is_default != Some(v))
            || (a.origin == Origin::Hls
                && b.origin == Origin::Hls
                && source.autoselect.unwrap_or(false) != target.autoselect.unwrap_or(false))
            || (a.origin == Origin::Hls
                && b.origin == Origin::Hls
                && (source.group != target.group || source.channels != target.channels))
        {
            return Err(BAD);
        }
    }
    if a.audio_group.is_some() && b.audio_group.is_none() {
        return Err(BAD);
    }
    if a.origin == Origin::Hls && b.origin == Origin::Hls && a.audio_group != b.audio_group {
        return Err(BAD);
    }
    Ok(())
}

pub(super) fn read_dash(path: &Path) -> Result<Vec<Metadata>, ConversionError> {
    let bytes = super::super::files::read_manifest(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| BAD)?;
    let document = roxmltree::Document::parse(text).map_err(|_| BAD)?;
    let mut descriptions = document
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "Representation")
        .map(dash)
        .collect::<Result<Vec<_>, _>>()?;
    if descriptions.iter().filter(|m| m.audio.is_some()).count() > 1 {
        return Err(UNSUPPORTED);
    }
    if descriptions.iter().any(|m| m.audio.is_some()) {
        for metadata in &mut descriptions {
            if metadata.audio.is_none() {
                metadata.audio_group = Some("default-audio-group".into());
            }
        }
    }
    Ok(descriptions)
}

pub(in crate::conversion) fn validate_dash(
    rep: roxmltree::Node<'_, '_>,
) -> Result<(), ConversionError> {
    dash(rep).map(|_| ())
}

/// Container-only inputs get the same explicit generated declarations in either manifest.
pub(super) fn materialize(
    tracks: &mut [Prepared],
    backend: &super::super::backend::Backend,
    keys: &super::InputKeys,
) -> Result<(), ConversionError> {
    if tracks
        .iter()
        .filter(|t| t.report.tracks[0].kind == super::TrackKind::Audio)
        .count()
        != 1
    {
        return Ok(());
    }
    for (index, track) in tracks.iter_mut().enumerate() {
        if track.report.tracks[0].kind == super::TrackKind::Audio && track.metadata.audio.is_none()
        {
            let profile = backend.profile_stream_keys(
                &track.media,
                track.index,
                if track.source_dictionary {
                    Some(super::super::keys::ProbeKeys::All(keys))
                } else {
                    super::key_for(track, keys).map(super::super::keys::ProbeKeys::One)
                },
            )?;
            track.metadata.audio = Some(Rendition {
                group: "default-audio-group".into(),
                name: Some(format!("stream_{index}")),
                language: profile["language"].as_str().unwrap_or("und").into(),
                is_default: Some(false),
                autoselect: Some(true),
                channels: profile["channels"].as_u64().map(|n| n.to_string()),
            });
        }
    }
    Ok(())
}
