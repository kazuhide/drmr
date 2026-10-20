//! Exact checks for provider records in the final published presentation graph.
use super::{ConversionError, MISMATCH, Objects, OutputKey, OutputSignaling, read_bounded};
use crate::{
    inspection::{OutputFormat, TargetProtection},
    mp4,
};
use base64::Engine as _;
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

const DASH: &str = "urn:mpeg:dash:schema:mpd:2011";
const CENC: &str = "urn:mpeg:cenc:2013";
const PR: &str = "urn:uuid:9a04f079-9840-4286-ab92-e65be0885f95";
const WV: &str = "urn:uuid:edef8ba9-79d6-4ace-a3c8-27dcd51d21ed";
const COMMON: &str = "urn:mpeg:dash:mp4protection:2011";

pub(in crate::conversion) struct MultiObjects {
    playready: Option<Objects>,
    widevine: Option<Zeroizing<Vec<u8>>>,
    pub hex: Zeroizing<String>,
}
impl OutputSignaling {
    pub(in crate::conversion) fn multi_objects(
        &self,
        key: &OutputKey,
    ) -> Result<MultiObjects, ConversionError> {
        let multi = self.multi.as_ref().ok_or(super::INVALID)?;
        let playready = (!self.license_url.is_empty())
            .then(|| OutputSignaling::playready(&self.license_url)?.objects(key))
            .transpose()?;
        if let Some(widevine) = &multi.widevine {
            super::multi::validate_widevine(widevine, Some(&key.0.id_hex()))?;
        }
        let mut hex = Zeroizing::new(String::new());
        if let Some(widevine) = &multi.widevine {
            hex.push_str(&hex::encode(widevine));
        }
        if let Some(playready) = &playready {
            hex.push_str(&playready.hex);
        }
        Ok(MultiObjects {
            playready,
            widevine: multi.widevine.clone(),
            hex,
        })
    }
}
impl MultiObjects {
    pub fn secrets(&self) -> Vec<&str> {
        let mut values = vec![self.hex.as_str()];
        if let Some(playready) = &self.playready {
            values.extend(playready.secrets());
        }
        values
    }
    fn init(&self, path: &Path, key: &OutputKey) -> Result<(), ConversionError> {
        let data = read_bounded(path)?;
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
            if pssh
                .iter()
                .filter(|node| node.header.bytes(&data) == bytes)
                .count()
                != 1
            {
                return Err(MISMATCH);
            }
        }
        init_key(&data, key)
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
        if common.attribute("value") != Some("cbcs")
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
    pub fn finish(&self, input: &Path, key: &OutputKey) -> Result<(), ConversionError> {
        let root = input.parent().ok_or(MISMATCH)?;
        let data = read_bounded(input)?;
        let text = std::str::from_utf8(&data).map_err(|_| MISMATCH)?;
        let document = roxmltree::Document::parse(text).map_err(|_| MISMATCH)?;
        if !document.root_element().has_tag_name((DASH, "MPD")) {
            return Err(MISMATCH);
        }
        let mut resources = BTreeSet::new();
        let mut declarations = 0;
        let mut sets = 0;
        for set in document
            .descendants()
            .filter(|node| node.has_tag_name((DASH, "AdaptationSet")))
        {
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
                let path = super::super::dash::initialization_reference(root, rep)
                    .map_err(|_| MISMATCH)?;
                if !resources.insert(path.clone()) {
                    return Err(MISMATCH);
                }
                self.init(&path, key)?;
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
        Ok(())
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
fn init_key(data: &[u8], key: &OutputKey) -> Result<(), ConversionError> {
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
        || schm[0].header.payload(data).get(4..8) != Some(b"cbcs")
    {
        return Err(MISMATCH);
    }
    Ok(())
}

pub(super) fn fairplay(
    input: &Path,
    format: OutputFormat,
    target: TargetProtection,
    uri: &str,
    key: Option<&OutputKey>,
    hls_key: Option<&super::super::HlsOutputKey>,
) -> Result<(), ConversionError> {
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
        let data = read_bounded(&path)?;
        let text = std::str::from_utf8(&data).map_err(|_| MISMATCH)?;
        version(text, if format == OutputFormat::Hls { 6 } else { 5 }, true)?;
        let mut keys = 0;
        let mut media = 0;
        let mut maps = 0;
        let expected_iv = playlist_iv(&path, text, format, target, key, hls_key)?;
        for value in &session_keys {
            declaration(
                value,
                uri,
                expected_iv.as_deref().map(String::as_str),
                false,
            )?;
        }
        for line in text.lines().map(str::trim) {
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
                media += 1;
            }
        }
        if keys != 1 || media == 0 {
            return Err(MISMATCH);
        }
    }
    Ok(())
}
fn playlist_iv(
    path: &Path,
    text: &str,
    format: OutputFormat,
    target: TargetProtection,
    key: Option<&OutputKey>,
    hls_key: Option<&super::super::HlsOutputKey>,
) -> Result<Option<Zeroizing<String>>, ConversionError> {
    let mut maps = 0;
    let mut expected_iv = None;
    if format == OutputFormat::Hls {
        let key = key.ok_or(super::INVALID)?;
        for line in text.lines().map(str::trim) {
            if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
                let attrs = super::super::hls::attributes(value).map_err(|_| MISMATCH)?;
                if attrs.get("BYTERANGE").is_some() {
                    return Err(MISMATCH);
                }
                let init = read_bounded(&resource(
                    path.parent().ok_or(MISMATCH)?,
                    attrs.get("URI").ok_or(MISMATCH)?,
                )?)?;
                let boxes = mp4::parse_boxes(&init).map_err(|_| MISMATCH)?;
                if mp4::walk(&boxes)
                    .iter()
                    .any(|node| matches!(&node.kind(), b"pssh" | b"uuid"))
                {
                    return Err(MISMATCH);
                }
                init_key(&init, key)?;
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
pub(super) fn unrequested(frozen: &super::super::FrozenSource) -> Result<(), ConversionError> {
    let mut root = frozen.entrypoint().to_path_buf();
    for _ in frozen.inventory().entrypoint.components() {
        root.pop();
    }
    let mut manifests = BTreeSet::new();
    let mut encrypted = BTreeSet::new();
    for file in &frozen.inventory().files {
        let path = root.join(&file.path);
        if let Some(hls) = manifest_kind(&path)? {
            let data = read_bounded(&path)?;
            let text = std::str::from_utf8(&data)
                .map_err(|_| MISMATCH)?
                .trim_start_matches('\u{feff}');
            if hls {
                encrypted.extend(unrequested_hls(&path, text)?);
            } else {
                unrequested_dash(text)?;
            }
            manifests.insert(path);
        }
    }
    for file in &frozen.inventory().files {
        let path = root.join(&file.path);
        if !manifests.contains(&path) {
            no_provider_media(&path, encrypted.contains(&path))?;
        }
    }
    frozen.check_original().map_err(|_| MISMATCH)
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
fn unrequested_hls(path: &Path, text: &str) -> Result<BTreeSet<PathBuf>, ConversionError> {
    let mut encrypted = BTreeSet::new();
    let mut whole_encryption = false;
    for line in text.lines().map(str::trim) {
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
                encrypted.insert(resource(path.parent().ok_or(MISMATCH)?, &uri)?);
            }
        }
    }
    Ok(encrypted)
}
fn unrequested_dash(text: &str) -> Result<(), ConversionError> {
    let document = roxmltree::Document::parse(text).map_err(|_| MISMATCH)?;
    for node in document.descendants().filter(roxmltree::Node::is_element) {
        if matches!(node.tag_name().name(), "pssh" | "pro") {
            return Err(MISMATCH);
        }
        if node.tag_name().name() == "ContentProtection"
            && (!node.has_tag_name((DASH, "ContentProtection"))
                || node.attribute("schemeIdUri") != Some(COMMON)
                || node.children().any(|child| child.is_element()))
        {
            return Err(MISMATCH);
        }
    }
    Ok(())
}
fn no_provider_media(path: &Path, whole_encryption: bool) -> Result<(), ConversionError> {
    let mut file = File::open(path).map_err(|_| MISMATCH)?;
    let mut prefix = [0; 3 * 188];
    let length = file.read(&mut prefix).map_err(|_| MISMATCH)?;
    let bytes = file.metadata().map_err(|_| MISMATCH)?.len();
    let bmff = prefix[..length].get(4..8).is_some_and(|kind| {
        matches!(
            kind,
            b"ftyp" | b"styp" | b"moov" | b"moof" | b"mdat" | b"sidx" | b"free" | b"pssh" | b"uuid"
        )
    });
    // Opaque whole-file AES-128 bytes have no readable provider metadata. An actual
    // BMFF header still receives the box checks, even under that manifest declaration.
    if whole_encryption && !bmff {
        return Ok(());
    }
    // Transport streams have no ISO BMFF provider boxes.
    if !bmff
        && bytes != 0
        && bytes.is_multiple_of(188)
        && prefix[..length].chunks(188).all(|packet| packet[0] == 0x47)
    {
        return Ok(());
    }
    no_provider_boxes(path)
}
fn no_provider_boxes(path: &Path) -> Result<(), ConversionError> {
    let mut file = File::open(path).map_err(|_| MISMATCH)?;
    let length = file.metadata().map_err(|_| MISMATCH)?.len();
    let mut at = 0u64;
    let mut count = 0;
    while at < length {
        count += 1;
        if count > 100_000 {
            return Err(MISMATCH);
        }
        let mut header = [0; 8];
        file.read_exact(&mut header).map_err(|_| MISMATCH)?;
        let mut bytes = Zeroizing::new(header.to_vec());
        let size = match u32::from_be_bytes(header[..4].try_into().unwrap()) {
            0 => length - at,
            1 => {
                let mut wide = [0; 8];
                file.read_exact(&mut wide).map_err(|_| MISMATCH)?;
                bytes.extend(wide);
                u64::from_be_bytes(wide)
            }
            value => u64::from(value),
        };
        if size < bytes.len() as u64 || size > length - at {
            return Err(MISMATCH);
        }
        if matches!(&header[4..], b"moov" | b"moof" | b"pssh" | b"uuid") {
            if size > super::LIMIT as u64 {
                return Err(MISMATCH);
            }
            let begin = bytes.len();
            bytes.resize(usize::try_from(size).map_err(|_| MISMATCH)?, 0);
            file.read_exact(&mut bytes[begin..]).map_err(|_| MISMATCH)?;
            let boxes = mp4::parse_boxes(&bytes).map_err(|_| MISMATCH)?;
            for node in mp4::walk(&boxes) {
                if node.kind() == *b"uuid" {
                    return Err(MISMATCH);
                }
                if node.kind() == *b"pssh" {
                    let pssh = mp4::Pssh::parse(node.header.payload(&bytes)).ok_or(MISMATCH)?;
                    if hex::encode(pssh.system_id) != "1077efecc0b24d02ace33c1e52e2fb4b" {
                        return Err(MISMATCH);
                    }
                }
            }
        }
        at += size;
        file.seek(SeekFrom::Start(at)).map_err(|_| MISMATCH)?;
    }
    Ok(())
}
