//! Exact checks for provider records in the final published presentation graph.
use super::{ConversionError, MISMATCH, Objects, OutputKey, OutputSignaling, read_bounded};
use crate::{
    inspection::{OutputFormat, TargetProtection},
    mp4,
};
use base64::Engine as _;
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs::File,
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

mod common;
mod graph;
mod media;

const DASH: &str = "urn:mpeg:dash:schema:mpd:2011";
const CENC: &str = "urn:mpeg:cenc:2013";
const PR: &str = "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95";
const WV: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
const COMMON: &str = "urn:mpeg:dash:mp4protection:2011";

fn read_slice(
    path: &Path,
    range: Option<(u64, u64)>,
    stop: &crate::concurrency::Stop,
) -> Result<Zeroizing<Vec<u8>>, ConversionError> {
    let Some((start, end)) = range else {
        return read_bounded(path);
    };
    super::cancelled(stop)?;
    let mut input = super::super::files::regular_input(path).map_err(|_| MISMATCH)?;
    let identity = crate::native::FileIdentity::read(&input).map_err(|_| MISMATCH)?;
    let length = input.metadata().map_err(|_| MISMATCH)?.len();
    let count = end
        .checked_sub(start)
        .and_then(|n| n.checked_add(1))
        .and_then(|n| usize::try_from(n).ok())
        .filter(|n| *n <= super::LIMIT && end < length)
        .ok_or(MISMATCH)?;
    input.seek(SeekFrom::Start(start)).map_err(|_| MISMATCH)?;
    let mut bytes = Zeroizing::new(vec![0; count]);
    for chunk in bytes.chunks_mut(65_536) {
        super::cancelled(stop)?;
        input.read_exact(chunk).map_err(|_| MISMATCH)?;
    }
    if input.metadata().map_err(|_| MISMATCH)?.len() != length
        || crate::native::FileIdentity::read(
            &super::super::files::regular_input(path).map_err(|_| MISMATCH)?,
        )
        .map_err(|_| MISMATCH)?
            != identity
    {
        return Err(MISMATCH);
    }
    Ok(bytes)
}

/// Generated provider records have no diagnostic representation of their contents.
pub(in crate::conversion) struct SegmentRecords {
    pub initialization: Zeroizing<Vec<u8>>,
    pub content_protection: Zeroizing<String>,
}

impl OutputSignaling {
    /// Reuse the same provider objects for native resource writing and final validation.
    pub(in crate::conversion) fn segment_records(
        &self,
        key: &OutputKey,
        target: TargetProtection,
    ) -> Result<SegmentRecords, ConversionError> {
        let objects = if self.is_multi() {
            self.multi_objects(key, target)?
        } else {
            MultiObjects::legacy_playready(self.objects(key, target)?)
        };
        let kid = key.0.id_hex();
        let uuid = format!(
            "{}-{}-{}-{}-{}",
            &kid[..8],
            &kid[8..12],
            &kid[12..16],
            &kid[16..20],
            &kid[20..]
        );
        let mut content_protection = Zeroizing::new(format!(
            "<ContentProtection schemeIdUri=\"{COMMON}\" value=\"{}\" cenc:default_KID=\"{uuid}\"/>",
            target.as_str()
        ));
        let mut initialization = Zeroizing::new(Vec::new());
        if let Some(widevine) = &objects.widevine {
            initialization.extend_from_slice(widevine);
            let pssh = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(widevine));
            write!(content_protection, "<ContentProtection schemeIdUri=\"{WV}\"><cenc:pssh>{}</cenc:pssh></ContentProtection>", pssh.as_str()).map_err(|_| MISMATCH)?;
        }
        if let Some(playready) = &objects.playready {
            initialization.extend_from_slice(&playready.bytes);
            write!(content_protection, "<ContentProtection schemeIdUri=\"{PR}\"><cenc:pssh>{}</cenc:pssh><mspr:pro>{}</mspr:pro></ContentProtection>", playready.pssh.as_str(), playready.pro.as_str()).map_err(|_| MISMATCH)?;
        }
        Ok(SegmentRecords {
            initialization,
            content_protection,
        })
    }
}

pub(in crate::conversion) struct MultiObjects {
    target: TargetProtection,
    playready: Option<Objects>,
    widevine: Option<Zeroizing<Vec<u8>>>,
    widevine_hex: Option<Zeroizing<String>>,
    pub hex: Zeroizing<String>,
}
impl OutputSignaling {
    pub(in crate::conversion) fn multi_objects(
        &self,
        key: &OutputKey,
        target: TargetProtection,
    ) -> Result<MultiObjects, ConversionError> {
        self.validate_profile(OutputFormat::Dash, target)?;
        let multi = self.multi.as_ref().ok_or(super::INVALID)?;
        let playready = (!self.license_url.is_empty())
            .then(|| OutputSignaling::playready(&self.license_url)?.objects(key, target))
            .transpose()?;
        if let Some(widevine) = &multi.widevine {
            super::multi::validate_widevine(widevine, Some(&key.0.id_hex()), Some(target))?;
        }
        let widevine_hex = multi
            .widevine
            .as_ref()
            .map(|value| Zeroizing::new(hex::encode(value)));
        let mut hex = Zeroizing::new(String::new());
        if let Some(widevine) = &widevine_hex {
            hex.push_str(widevine);
        }
        if let Some(playready) = &playready {
            hex.push_str(&playready.hex);
        }
        Ok(MultiObjects {
            target,
            playready,
            widevine: multi.widevine.clone(),
            widevine_hex,
            hex,
        })
    }

    /// Key-file parsing can bind identifiers before a command chooses its target scheme.
    pub(in crate::conversion) fn validate_output_key(
        &self,
        key: &OutputKey,
    ) -> Result<(), ConversionError> {
        if let Some(widevine) = self
            .multi
            .as_ref()
            .and_then(|multi| multi.widevine.as_ref())
        {
            super::multi::validate_widevine(widevine, Some(&key.0.id_hex()), None)?;
        }
        Ok(())
    }
}
impl MultiObjects {
    pub(super) fn legacy_playready(playready: Objects) -> Self {
        Self {
            target: playready.target,
            hex: playready.hex.clone(),
            playready: Some(playready),
            widevine: None,
            widevine_hex: None,
        }
    }
    pub fn secrets(&self) -> Vec<&str> {
        let mut values = vec![self.hex.as_str()];
        values.extend(self.widevine_hex.iter().map(|value| value.as_str()));
        if let Some(playready) = &self.playready {
            values.extend(playready.secrets());
        }
        values
    }
    fn init(
        &self,
        path: &Path,
        range: Option<(u64, u64)>,
        key: &OutputKey,
        stop: &crate::concurrency::Stop,
    ) -> Result<(), ConversionError> {
        super::cancelled(stop)?;
        let data = read_slice(path, range, stop)?;
        let boxes = mp4::parse_boxes(&data).map_err(|_| MISMATCH)?;
        let nodes = mp4::walk(&boxes);
        let pssh: Vec<_> = nodes
            .iter()
            .filter(|node| node.kind() == *b"pssh")
            .collect();
        let expected = usize::from(self.widevine.is_some()) + usize::from(self.playready.is_some());
        if pssh.len() != expected || nodes.iter().any(|node| node.kind() == *b"uuid") {
            return Err(MISMATCH);
        }
        for bytes in self
            .widevine
            .iter()
            .map(|value| value.as_slice())
            .chain(self.playready.iter().map(|value| value.bytes.as_slice()))
        {
            super::cancelled(stop)?;
            if pssh
                .iter()
                .filter(|node| node.header.bytes(&data) == bytes)
                .count()
                != 1
            {
                return Err(MISMATCH);
            }
        }
        init_key(&data, key, self.target)
    }
    fn protection(
        &self,
        set: roxmltree::Node<'_, '_>,
        key: &OutputKey,
    ) -> Result<usize, ConversionError> {
        let expected_kid = key.0.id_hex();
        let protection: Vec<_> = set
            .children()
            .filter(|node| node.has_tag_name((DASH, "ContentProtection")))
            .collect();
        let expected =
            1 + usize::from(self.widevine.is_some()) + usize::from(self.playready.is_some());
        if protection.len() != expected
            || set
                .descendants()
                .filter(|node| node.is_element() && node.tag_name().name() == "ContentProtection")
                .count()
                != expected
        {
            return Err(MISMATCH);
        }
        let common = protection
            .iter()
            .find(|node| node.attribute("schemeIdUri") == Some(COMMON))
            .ok_or(MISMATCH)?;
        if common.attribute("value") != Some(self.target.as_str())
            || common
                .attribute((CENC, "default_KID"))
                .is_none_or(|value| value.replace('-', "").to_ascii_lowercase() != expected_kid)
            || common.children().any(|node| node.is_element())
        {
            return Err(MISMATCH);
        }
        if let Some(widevine) = &self.widevine {
            let node = provider(&protection, WV)?;
            let children: Vec<_> = node
                .children()
                .filter(roxmltree::Node::is_element)
                .collect();
            if children.len() != 1 || !children[0].has_tag_name((CENC, "pssh")) {
                return Err(MISMATCH);
            }
            let value = Zeroizing::new(base64::engine::general_purpose::STANDARD.encode(widevine));
            exact_text(children[0], &value)?;
        }
        if let Some(playready) = &self.playready {
            let node = provider(&protection, PR)?;
            let children: Vec<_> = node
                .children()
                .filter(roxmltree::Node::is_element)
                .collect();
            if children.len() != 2 {
                return Err(MISMATCH);
            }
            let pssh = children
                .iter()
                .find(|node| node.has_tag_name((CENC, "pssh")))
                .ok_or(MISMATCH)?;
            let pro = children
                .iter()
                .find(|node| node.has_tag_name(("urn:microsoft:playready", "pro")))
                .ok_or(MISMATCH)?;
            exact_text(*pssh, &playready.pssh)?;
            exact_text(*pro, &playready.pro)?;
        }
        Ok(expected)
    }
    pub fn finish(
        &self,
        frozen: &super::super::FrozenSource,
        key: &OutputKey,
        stop: &crate::concurrency::Stop,
    ) -> Result<(), ConversionError> {
        super::cancelled(stop)?;
        let input = frozen.entrypoint();
        let mut graph = graph::Resources::new(frozen, stop)?;
        graph.assign(input, graph::Role::Manifest)?;
        let root = input.parent().ok_or(MISMATCH)?;
        let data = read_bounded(input)?;
        let text = std::str::from_utf8(&data).map_err(|_| MISMATCH)?;
        let document = roxmltree::Document::parse(text).map_err(|_| MISMATCH)?;
        if !document.root_element().has_tag_name((DASH, "MPD")) {
            return Err(MISMATCH);
        }
        let mut resources = BTreeSet::new();
        let mut media = BTreeSet::new();
        let mut declarations = 0;
        let mut sets = 0;
        let mut admission = super::super::dash::addressing::Admission::local();
        for set in document
            .descendants()
            .filter(|node| node.has_tag_name((DASH, "AdaptationSet")))
        {
            super::cancelled(stop)?;
            sets += 1;
            let reps: Vec<_> = set
                .children()
                .filter(|node| node.has_tag_name((DASH, "Representation")))
                .collect();
            if reps.is_empty() {
                return Err(MISMATCH);
            }
            declarations += self.protection(set, key)?;
            for rep in reps {
                super::cancelled(stop)?;
                let (initialization, segments) =
                    super::super::dash::record_resources(root, rep, stop, &mut admission)
                        .map_err(|_| MISMATCH)?;
                if !resources.insert(initialization.clone()) {
                    return Err(MISMATCH);
                }
                graph.assign(&initialization.path, graph::Role::DashObject)?;
                self.init(&initialization.path, initialization.range, key, stop)?;
                for segment in segments {
                    graph.assign(&segment.path, graph::Role::DashObject)?;
                    media.insert(segment);
                }
            }
        }
        if resources.is_empty()
            || resources.len() > 64
            || document
                .descendants()
                .filter(|node| node.is_element() && node.tag_name().name() == "ContentProtection")
                .count()
                != declarations
            || document
                .descendants()
                .filter(|node| node.is_element() && node.tag_name().name() == "pssh")
                .count()
                != sets
                    * (usize::from(self.widevine.is_some()) + usize::from(self.playready.is_some()))
            || document
                .descendants()
                .filter(|node| node.is_element() && node.tag_name().name() == "pro")
                .count()
                != sets * usize::from(self.playready.is_some())
        {
            return Err(MISMATCH);
        }
        graph.complete()?;
        for resource in media {
            media::check_range(
                &resource.path,
                resource.range,
                media::Selection::Providers(self),
                stop,
            )?;
        }
        unchanged(frozen)
    }
}
fn provider<'a, 'input>(
    nodes: &[roxmltree::Node<'a, 'input>],
    scheme: &str,
) -> Result<roxmltree::Node<'a, 'input>, ConversionError> {
    let mut selected = nodes
        .iter()
        .filter(|node| node.attribute("schemeIdUri") == Some(scheme));
    let result = *selected.next().ok_or(MISMATCH)?;
    if selected.next().is_some() {
        return Err(MISMATCH);
    }
    Ok(result)
}
fn exact_text(node: roxmltree::Node<'_, '_>, expected: &str) -> Result<(), ConversionError> {
    if node.text() != Some(expected)
        || node.children().any(|child| child.is_element())
        || node.children().filter(roxmltree::Node::is_text).count() != 1
    {
        return Err(MISMATCH);
    }
    Ok(())
}
fn resource(root: &Path, uri: &str) -> Result<PathBuf, ConversionError> {
    super::super::hls::local_resource(root, uri).map_err(|_| MISMATCH)
}
fn init_key(data: &[u8], key: &OutputKey, target: TargetProtection) -> Result<(), ConversionError> {
    let boxes = mp4::parse_boxes(data).map_err(|_| MISMATCH)?;
    let nodes = mp4::walk(&boxes);
    let tenc: Vec<_> = nodes
        .iter()
        .filter(|node| node.kind() == *b"tenc")
        .collect();
    let schm: Vec<_> = nodes
        .iter()
        .filter(|node| node.kind() == *b"schm")
        .collect();
    if tenc.len() != 1 || schm.len() != 1 {
        return Err(MISMATCH);
    }
    let tenc = mp4::Tenc::parse(tenc[0].header.payload(data)).ok_or(MISMATCH)?;
    if tenc.default_is_protected != 1
        || tenc.default_kid != key.0.id
        || schm[0].header.payload(data).get(4..8) != Some(target.as_str().as_bytes())
    {
        return Err(MISMATCH);
    }
    Ok(())
}

pub(super) struct FairPlayProfile<'a> {
    pub format: OutputFormat,
    pub target: TargetProtection,
    pub uri: &'a str,
    pub key: Option<&'a OutputKey>,
    pub hls_key: Option<&'a super::super::HlsOutputKey>,
}

pub(super) fn fairplay(
    frozen: &super::super::FrozenSource,
    profile: &FairPlayProfile<'_>,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    super::cancelled(stop)?;
    let format = profile.format;
    let uri = profile.uri;
    let input = frozen.entrypoint();
    let mut graph = graph::Resources::new(frozen, stop)?;
    graph.assign(input, graph::Role::Manifest)?;
    if !valid_fairplay_uri(uri) {
        return Err(super::INVALID);
    }
    let root = input
        .parent()
        .ok_or(MISMATCH)?
        .canonicalize()
        .map_err(|_| MISMATCH)?;
    let data = read_bounded(input)?;
    let text = std::str::from_utf8(&data).map_err(|_| MISMATCH)?;
    let mut playlists = BTreeSet::new();
    let mut variant = false;
    let mut session_keys = Vec::new();
    for line in text.lines().map(str::trim) {
        super::cancelled(stop)?;
        if line.starts_with("#EXT-X-KEY:") {
            return Err(MISMATCH);
        }
        if let Some(value) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attrs = super::super::hls::attributes(value).map_err(|_| MISMATCH)?;
            if let Some(uri) = attrs.get("URI") {
                playlists.insert(resource(&root, uri)?);
            }
        } else if line.starts_with("#EXT-X-STREAM-INF:") {
            variant = true;
        } else if let Some(value) = line.strip_prefix("#EXT-X-SESSION-KEY:") {
            session_keys.push(value);
        } else if !line.is_empty() && !line.starts_with('#') {
            if !variant {
                return Err(MISMATCH);
            }
            playlists.insert(resource(&root, line)?);
            variant = false;
        }
    }
    if variant || playlists.is_empty() || playlists.len() > 64 || session_keys.len() > 1 {
        return Err(MISMATCH);
    }
    version(text, 5, !session_keys.is_empty())?;
    for path in playlists {
        fairplay_playlist(&path, profile, &session_keys, &mut graph, stop)?;
    }
    graph.complete()?;
    for path in graph.media() {
        if format == OutputFormat::Hls {
            media::check(path, media::Selection::FairPlay, stop)?;
        } else {
            no_provider_media(path, false, profile.target, profile.key, stop)?;
        }
    }
    unchanged(frozen)
}
fn fairplay_playlist(
    path: &Path,
    profile: &FairPlayProfile<'_>,
    session_keys: &[&str],
    graph: &mut graph::Resources,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    let format = profile.format;
    let uri = profile.uri;
    super::cancelled(stop)?;
    graph.assign(path, graph::Role::Manifest)?;
    let data = read_bounded(path)?;
    let text = std::str::from_utf8(&data).map_err(|_| MISMATCH)?;
    version(text, if format == OutputFormat::Hls { 6 } else { 5 }, true)?;
    let mut keys = 0;
    let mut media = 0;
    let mut maps = 0;
    let expected_iv = playlist_iv(path, text, profile, graph, stop)?;
    for value in session_keys {
        super::cancelled(stop)?;
        declaration(
            value,
            uri,
            expected_iv.as_deref().map(String::as_str),
            false,
        )?;
    }
    for line in text.lines().map(str::trim) {
        super::cancelled(stop)?;
        if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
            if media != 0 {
                return Err(MISMATCH);
            }
            declaration(
                value,
                uri,
                expected_iv.as_deref().map(String::as_str),
                false,
            )?;
            keys += 1;
        } else if line.starts_with("#EXT-X-MAP:") {
            if format != OutputFormat::Hls || media != 0 || maps != 0 {
                return Err(MISMATCH);
            }
            maps += 1;
        } else if !line.is_empty() && !line.starts_with('#') {
            if keys != 1 || format == OutputFormat::Hls && maps != 1 {
                return Err(MISMATCH);
            }
            graph.assign(
                &resource(path.parent().ok_or(MISMATCH)?, line)?,
                graph::Role::Media,
            )?;
            media += 1;
        }
    }
    if keys != 1 || media == 0 {
        return Err(MISMATCH);
    }
    Ok(())
}
fn playlist_iv(
    path: &Path,
    text: &str,
    profile: &FairPlayProfile<'_>,
    graph: &mut graph::Resources,
    stop: &crate::concurrency::Stop,
) -> Result<Option<Zeroizing<String>>, ConversionError> {
    let FairPlayProfile {
        format,
        target,
        key,
        hls_key,
        ..
    } = *profile;
    let mut maps = 0;
    let mut expected_iv = None;
    if format == OutputFormat::Hls {
        let key = key.ok_or(super::INVALID)?;
        for line in text.lines().map(str::trim) {
            super::cancelled(stop)?;
            if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
                let attrs = super::super::hls::attributes(value).map_err(|_| MISMATCH)?;
                if attrs.get("BYTERANGE").is_some() {
                    return Err(MISMATCH);
                }
                let initialization = resource(
                    path.parent().ok_or(MISMATCH)?,
                    attrs.get("URI").ok_or(MISMATCH)?,
                )?;
                graph.assign(&initialization, graph::Role::Initialization)?;
                let init = read_bounded(&initialization)?;
                let boxes = mp4::parse_boxes(&init).map_err(|_| MISMATCH)?;
                if mp4::walk(&boxes)
                    .iter()
                    .any(|node| matches!(&node.kind(), b"pssh" | b"uuid"))
                {
                    return Err(MISMATCH);
                }
                init_key(&init, key, TargetProtection::Cbcs)?;
                expected_iv = mp4::tenc_constant_iv_hex(&init)
                    .map_err(|_| MISMATCH)?
                    .map(|iv| Zeroizing::new(iv.expose().to_string()));
                maps += 1;
            }
        }
        if maps != 1 {
            return Err(MISMATCH);
        }
    } else if format == OutputFormat::HlsTs && target == TargetProtection::HlsSampleAes {
        expected_iv = Some(Zeroizing::new(hex::encode(
            hls_key
                .and_then(super::super::HlsOutputKey::associated_iv)
                .ok_or(super::INVALID)?
                .0
                .as_ref(),
        )));
    } else {
        return Err(super::UNSUPPORTED);
    }
    Ok(expected_iv)
}
fn version(text: &str, minimum: u32, required: bool) -> Result<(), ConversionError> {
    let mut values = text
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("#EXT-X-VERSION:"));
    let Some(value) = values.next() else {
        return if required { Err(MISMATCH) } else { Ok(()) };
    };
    if values.next().is_some()
        || value.is_empty()
        || value.len() > 10
        || !value.bytes().all(|byte| byte.is_ascii_digit())
        || value
            .parse::<u32>()
            .ok()
            .is_none_or(|value| value == 0 || required && value < minimum)
    {
        return Err(MISMATCH);
    }
    Ok(())
}
fn declaration(
    value: &str,
    uri: &str,
    iv: Option<&str>,
    required_iv: bool,
) -> Result<(), ConversionError> {
    let attrs = super::super::hls::attributes(value).map_err(|_| MISMATCH)?;
    if attrs.get("METHOD") != Some("SAMPLE-AES")
        || attrs.get("URI") != Some(uri)
        || attrs.get("KEYFORMAT") != Some("com.apple.streamingkeydelivery")
        || attrs.get("KEYFORMATVERSIONS") != Some("1")
        || attrs.iter().any(|(name, _)| {
            !matches!(
                name,
                "METHOD" | "URI" | "KEYFORMAT" | "KEYFORMATVERSIONS" | "IV"
            )
        })
    {
        return Err(MISMATCH);
    }
    if let Some(actual) = attrs.get("IV") {
        let expected = iv.ok_or(MISMATCH)?;
        if !actual
            .strip_prefix("0x")
            .unwrap_or(actual)
            .eq_ignore_ascii_case(expected)
        {
            return Err(MISMATCH);
        }
    } else if required_iv {
        return Err(MISMATCH);
    }
    Ok(())
}
pub(super) fn valid_fairplay_uri(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 4096
        || value.contains('"')
        || value.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return false;
    }
    url::Url::parse(value).is_ok_and(|uri| {
        uri.scheme() == "skd"
            && uri.host_str().is_some()
            && uri.username().is_empty()
            && uri.password().is_none()
            && uri.fragment().is_none()
    })
}

/// An empty DRM selection does not authorize provider records hidden in the output graph.
pub(super) fn unrequested(
    frozen: &super::super::FrozenSource,
    target: TargetProtection,
    key: Option<&OutputKey>,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    unrequested_at(frozen, frozen.entrypoint(), target, key, stop)
}
fn unrequested_at(
    frozen: &super::super::FrozenSource,
    entrypoint: &Path,
    target: TargetProtection,
    key: Option<&OutputKey>,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    super::cancelled(stop)?;
    // Match resolver results, including Windows' canonical verbatim prefix, in one namespace.
    let entrypoint = entrypoint.canonicalize().map_err(|_| MISMATCH)?;
    let mut root = entrypoint.clone();
    for _ in frozen.inventory().entrypoint.components() {
        root.pop();
    }
    let root = root.canonicalize().map_err(|_| MISMATCH)?;
    let mut manifests = BTreeSet::new();
    let mut encrypted = BTreeSet::new();
    let mut dash_objects = BTreeSet::new();
    let mut pending = vec![entrypoint.clone()];
    while let Some(path) = pending.pop() {
        super::cancelled(stop)?;
        if manifests.contains(&path) {
            continue;
        }
        let Some(hls) = manifest_kind(&path)? else {
            if path != entrypoint {
                return Err(MISMATCH);
            }
            continue;
        };
        let data = read_bounded(&path)?;
        let text = std::str::from_utf8(&data)
            .map_err(|_| MISMATCH)?
            .trim_start_matches('\u{feff}');
        if hls {
            let resources = unrequested_hls(&path, text, stop)?;
            encrypted.extend(resources.encrypted);
            pending.extend(resources.playlists);
        } else if path == entrypoint {
            common::dash(text, target, key)?;
            let document = roxmltree::Document::parse(text).map_err(|_| MISMATCH)?;
            let parent = path.parent().ok_or(MISMATCH)?;
            let mut slices = BTreeSet::new();
            let mut admission = super::super::dash::addressing::Admission::local();
            for rep in document
                .descendants()
                .filter(|node| node.has_tag_name((DASH, "Representation")))
            {
                let (initialization, media) =
                    super::super::dash::record_resources(parent, rep, stop, &mut admission)
                        .map_err(|_| MISMATCH)?;
                for resource in std::iter::once(initialization).chain(media) {
                    if slices.len() >= 100_064 {
                        return Err(MISMATCH);
                    }
                    dash_objects.insert(resource.path.clone());
                    slices.insert(resource);
                }
            }
            if slices.is_empty() {
                return Err(MISMATCH);
            }
            for resource in slices {
                media::check_range(
                    &resource.path,
                    resource.range,
                    media::Selection::Default { target, key },
                    stop,
                )?;
            }
        } else {
            return Err(MISMATCH);
        }
        manifests.insert(path);
        if manifests.len() > 65 {
            return Err(MISMATCH);
        }
    }
    for file in &frozen.inventory().files {
        super::cancelled(stop)?;
        let path = root.join(&file.path).canonicalize().map_err(|_| MISMATCH)?;
        if !path.starts_with(&root) {
            return Err(MISMATCH);
        }
        if !manifests.contains(&path) && !dash_objects.contains(&path) {
            no_provider_media(&path, encrypted.contains(&path), target, key, stop)?;
        }
    }
    unchanged(frozen)
}
fn unchanged(frozen: &super::super::FrozenSource) -> Result<(), ConversionError> {
    frozen.check_original().map_err(|error| {
        if error.0 == "cancelled" {
            error
        } else {
            MISMATCH
        }
    })
}
fn manifest_kind(path: &Path) -> Result<Option<bool>, ConversionError> {
    let mut prefix = [0; 512];
    let length = File::open(path)
        .map_err(|_| MISMATCH)?
        .read(&mut prefix)
        .map_err(|_| MISMATCH)?;
    let text = String::from_utf8_lossy(&prefix[..length]);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    Ok(if text.starts_with("#EXTM3U") {
        Some(true)
    } else if text.starts_with('<') {
        Some(false)
    } else {
        None
    })
}
#[derive(Default)]
struct HlsResources {
    encrypted: BTreeSet<PathBuf>,
    playlists: BTreeSet<PathBuf>,
}
fn unrequested_hls(
    path: &Path,
    text: &str,
    stop: &crate::concurrency::Stop,
) -> Result<HlsResources, ConversionError> {
    let root = path.parent().ok_or(MISMATCH)?;
    let master = text
        .lines()
        .map(str::trim)
        .any(|line| line.starts_with("#EXT-X-MEDIA:") || line.starts_with("#EXT-X-STREAM-INF:"));
    let mut resources = HlsResources::default();
    let mut whole_encryption = false;
    for line in text.lines().map(str::trim) {
        super::cancelled(stop)?;
        if let Some(value) = line
            .strip_prefix("#EXT-X-KEY:")
            .or_else(|| line.strip_prefix("#EXT-X-SESSION-KEY:"))
        {
            let attrs = super::super::hls::attributes(value).map_err(|_| MISMATCH)?;
            if attrs
                .get("KEYFORMAT")
                .is_some_and(|value| value != "identity")
            {
                return Err(MISMATCH);
            }
            if line.starts_with("#EXT-X-KEY:") {
                whole_encryption = attrs.get("METHOD") == Some("AES-128");
            }
        } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA:") {
            let attrs = super::super::hls::attributes(value).map_err(|_| MISMATCH)?;
            if let Some(uri) = attrs.get("URI") {
                resources.playlists.insert(resource(root, uri)?);
            }
        } else if master && !line.is_empty() && !line.starts_with('#') {
            resources.playlists.insert(resource(root, line)?);
        } else if whole_encryption {
            let uri = if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
                super::super::hls::attributes(value)
                    .map_err(|_| MISMATCH)?
                    .get("URI")
                    .map(str::to_owned)
            } else {
                (!line.is_empty() && !line.starts_with('#')).then(|| line.to_owned())
            };
            if let Some(uri) = uri {
                resources.encrypted.insert(resource(root, &uri)?);
            }
        }
    }
    Ok(resources)
}
fn no_provider_media(
    path: &Path,
    whole_encryption: bool,
    target: TargetProtection,
    key: Option<&OutputKey>,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    super::cancelled(stop)?;
    let mut file = File::open(path).map_err(|_| MISMATCH)?;
    let mut prefix = [0; 3 * 188];
    let length = file.read(&mut prefix).map_err(|_| MISMATCH)?;
    let bytes = file.metadata().map_err(|_| MISMATCH)?.len();
    // Opaque whole-file AES-128 bytes have no readable provider metadata. A box
    // sequence is still checked, including readable boxes before an invalid trailer.
    if whole_encryption {
        return if readable_box_sequence(path, stop)? {
            no_provider_boxes(path, target, key, stop)
        } else {
            Ok(())
        };
    }
    // Transport streams have no ISO BMFF provider boxes.
    if bytes != 0
        && bytes.is_multiple_of(188)
        && prefix[..length].chunks(188).all(|packet| packet[0] == 0x47)
        && !readable_box_sequence(path, stop)?
    {
        return Ok(());
    }
    no_provider_boxes(path, target, key, stop)
}
fn readable_box_sequence(
    path: &Path,
    stop: &crate::concurrency::Stop,
) -> Result<bool, ConversionError> {
    let mut file = File::open(path).map_err(|_| MISMATCH)?;
    let length = file.metadata().map_err(|_| MISMATCH)?.len();
    let mut at = 0u64;
    let mut readable = false;
    let mut count = 0;
    while at < length {
        super::cancelled(stop)?;
        count += 1;
        if count > 100_000 {
            return Err(MISMATCH);
        }
        if length - at < 8 {
            return Ok(readable);
        }
        let mut header = [0; 8];
        file.read_exact(&mut header).map_err(|_| MISMATCH)?;
        readable |= matches!(
            &header[4..],
            b"ftyp" | b"styp" | b"moov" | b"moof" | b"mdat" | b"sidx" | b"free" | b"pssh" | b"uuid"
        );
        let mut header_bytes = 8;
        let size = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
            0 => length - at,
            1 => {
                if length - at < 16 {
                    return Ok(readable);
                }
                let mut wide = [0; 8];
                file.read_exact(&mut wide).map_err(|_| MISMATCH)?;
                header_bytes = 16;
                u64::from_be_bytes(wide)
            }
            value => u64::from(value),
        };
        if size < header_bytes || size > length - at {
            return Ok(readable);
        }
        at += size;
        file.seek(SeekFrom::Start(at)).map_err(|_| MISMATCH)?;
    }
    Ok(count != 0)
}
fn no_provider_boxes(
    path: &Path,
    target: TargetProtection,
    key: Option<&OutputKey>,
    stop: &crate::concurrency::Stop,
) -> Result<(), ConversionError> {
    media::check(path, media::Selection::Default { target, key }, stop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unrequested_graph_uses_one_path_namespace_through_an_aliased_snapshot_root() {
        let owner = crate::native::private_tempdir().unwrap();
        for name in ["audio", "video"] {
            crate::native::create_media_dir_all(&owner.path().join(name)).unwrap();
        }
        let master = owner.path().join("master.m3u8");
        std::fs::write(&master, "#EXTM3U\n#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"a\",URI=\"audio/media.m3u8\"\n#EXT-X-STREAM-INF:BANDWIDTH=100,AUDIO=\"a\"\nvideo/media.m3u8\n").unwrap();
        std::fs::write(owner.path().join("audio/media.m3u8"), "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"https://example.invalid/key\"\n#EXTINF:2,\nsegment.bin\n#EXT-X-ENDLIST\n").unwrap();
        let mut opaque = [0x9a; 32];
        opaque[0] = b'<';
        std::fs::write(owner.path().join("audio/segment.bin"), opaque).unwrap();
        std::fs::write(
            owner.path().join("video/media.m3u8"),
            "#EXTM3U\n#EXTINF:2,\nsegment.bin\n#EXT-X-ENDLIST\n",
        )
        .unwrap();
        let mut transport = [0; 3 * 188];
        for packet in transport.chunks_mut(188) {
            packet[0] = 0x47;
        }
        std::fs::write(owner.path().join("video/segment.bin"), transport).unwrap();
        let frozen =
            super::super::super::FrozenSource::capture(&master, &crate::concurrency::Stop::new())
                .unwrap();
        unrequested(
            &frozen,
            TargetProtection::HlsAes128,
            None,
            &crate::concurrency::Stop::new(),
        )
        .unwrap();
        let alias = owner.path().join("snapshot-alias");
        std::os::unix::fs::symlink(frozen.entrypoint().parent().unwrap(), &alias).unwrap();
        unrequested_at(
            &frozen,
            &alias.join(&frozen.inventory().entrypoint),
            TargetProtection::HlsAes128,
            None,
            &crate::concurrency::Stop::new(),
        )
        .unwrap();
        frozen.check_original().unwrap();
    }

    #[test]
    fn dual_drm_secret_list_covers_standalone_widevine_encodings() {
        let kid = "000102030405060708090a0b0c0d0e0f";
        let bytes = crate::playready::build_pssh_box(
            &crate::playready::WIDEVINE_SYSTEM_ID,
            &hex::decode(format!("1210{kid}")).unwrap(),
        )
        .unwrap();
        let hex = hex::encode(&bytes);
        let signaling =
            OutputSignaling::multi_drm(Some(&hex), Some("https://example.invalid/license"), false)
                .unwrap();
        let key = OutputKey::from_hex(kid, "101112131415161718191a1b1c1d1e1f").unwrap();
        let objects = signaling
            .multi_objects(&key, TargetProtection::Cbcs)
            .unwrap();
        let secrets = crate::toolrun::Secrets::with(objects.secrets());
        for value in [
            hex.clone(),
            hex.to_uppercase(),
            hex.as_bytes()
                .chunks(2)
                .map(|pair| std::str::from_utf8(pair).unwrap())
                .collect::<Vec<_>>()
                .join(" :0x"),
            base64::engine::general_purpose::STANDARD.encode(bytes),
        ] {
            assert_eq!(secrets.scrub(&value), crate::toolrun::REDACTED);
        }
    }
}
